//! SwitchyOmega Conditions and AutoProxy use different exception precedence.
use super::{Bundle, ImportSummary, SkipReason, SkippedCondition, named_group};
use crate::error::{Error, Result};
use crate::model::{AppModel, Matchers, Network, RouteAction, RouteRule, RouteRuleGroup};
use std::collections::BTreeSet;
use std::net::IpAddr;

fn invalid(line: usize, reason: &str) -> Error {
    Error::config(format!("第 {line} 行：{reason}"))
}

fn skip(summary: &mut ImportSummary, line: usize, reason: SkipReason) {
    summary.skipped += 1;
    if summary.skipped_lines.len() < 8 {
        summary
            .skipped_lines
            .push(SkippedCondition { line, reason });
    }
}

fn profile(name: &str, model: &AppModel, fallback: &mut BTreeSet<String>) -> RouteAction {
    if name == "direct" {
        RouteAction::direct()
    } else if let Some(id) = named_group(model, name) {
        RouteAction::proxy_via(id)
    } else {
        if name != "proxy" {
            fallback.insert(name.into());
        }
        RouteAction::proxy()
    }
}

pub(super) fn convert(text: &str, model: &AppModel) -> Result<(Bundle, ImportSummary)> {
    let decoded;
    let text = if text.starts_with("W0F1dG9Qcm94") {
        decoded = crate::util::decode_base64_any(text)
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or_else(|| Error::config("AutoProxy Base64 文本无效，请重新导出"))?;
        decoded.trim()
    } else {
        text
    };
    let mut lines = text.lines().enumerate();
    let header = lines.next().map(|(_, l)| l.trim()).unwrap_or("");
    let auto = header == "[AutoProxy]"
        || header
            .strip_prefix("[AutoProxy ")
            .and_then(|s| s.strip_suffix(']'))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
    if !auto && header != "[SwitchyOmega Conditions]" {
        return Err(Error::config(
            "无法识别规则格式，请选择 Xross 路由 JSON、SwitchyOmega Conditions 或 AutoProxy 列表",
        ));
    }
    let mut summary = ImportSummary {
        format: if auto { "AutoProxy" } else { "SwitchyOmega" }.into(),
        ..Default::default()
    };
    let mut rules = Vec::new();
    let mut exceptions = Vec::new();
    let mut fallback = BTreeSet::new();
    let mut note = None;
    let mut with_result = false;
    let mut exclusive_indices = Vec::new();
    let mut default = None;
    for (idx, raw) in lines {
        let line = idx + 1;
        let raw = raw.trim();
        if raw.is_empty() || (auto && raw.starts_with('!')) || (!auto && raw.starts_with(';')) {
            continue;
        }
        if !auto && raw.starts_with('@') {
            let (directive, arg) = raw.split_once(char::is_whitespace).unwrap_or((raw, ""));
            let arg = arg.trim();
            if directive.eq_ignore_ascii_case("@note") {
                note = Some(arg);
            } else if directive.eq_ignore_ascii_case("@with")
                && (arg.eq_ignore_ascii_case("result") || arg.eq_ignore_ascii_case("results"))
                && rules.is_empty()
            {
                with_result = true;
            } else {
                return Err(invalid(line, "未知指令或指令位置无效，请检查文件"));
            }
            continue;
        }
        if default.is_some() {
            return Err(invalid(line, "默认规则 * 必须位于所有条件之后"));
        }
        let label = note.take();
        let (condition, exclusive) = match raw.strip_prefix(if auto { "@@" } else { "!" }) {
            Some(c) => (c.trim(), true),
            None => (raw, false),
        };
        let (condition, target) = if !auto && with_result && !exclusive {
            let (c, name) = condition
                .rsplit_once(" +")
                .filter(|(_, n)| !n.trim().is_empty())
                .ok_or_else(|| invalid(line, "规则缺少 +情景模式，请补齐后导入"))?;
            (c.trim(), Some(name.trim()))
        } else {
            (condition, None)
        };
        let matchers = if auto {
            condition
                .strip_prefix("||")
                .ok_or(SkipReason::Url)
                .and_then(|s| {
                    // AutoProxy's other expressions match the full URL, not its host.
                    host(&format!("*.{s}")).ok_or(SkipReason::InvalidPattern)
                })
        } else {
            parse_condition(condition)
        };
        let m = match matchers {
            Ok(m) => m,
            Err(reason) => {
                skip(&mut summary, line, reason);
                continue;
            }
        };
        let action = if let Some(target) = target {
            profile(target, model, &mut fallback)
        } else if exclusive {
            RouteAction::direct()
        } else {
            RouteAction::proxy()
        };
        if m.network == Some(Network::TcpUdp) {
            summary.catch_all = true;
            if with_result && condition == "*" && !exclusive {
                default = Some(action.clone());
            }
        }
        let name = label
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if auto {
                    format!("AutoProxy · {line}")
                } else {
                    format!("SwitchyOmega · {line}")
                }
            });
        let rule = RouteRule::new(name, m, action);
        if auto && exclusive {
            exceptions.push(rule);
        } else {
            if exclusive && with_result {
                exclusive_indices.push(rules.len());
            }
            rules.push(rule);
        }
        if rules.len() + exceptions.len() > 50_000 {
            return Err(Error::config("单个文件超过 50000 条规则，请分批导入"));
        }
    }
    if with_result {
        let action = default.ok_or_else(|| Error::config("缺少末尾的默认规则：* +情景模式"))?;
        for idx in exclusive_indices {
            rules[idx].action = action.clone();
        }
    }
    if summary.skipped > 0
        && summary.catch_all
        && rules
            .iter()
            .all(|r| r.matchers.network == Some(Network::TcpUdp))
        && exceptions.is_empty()
    {
        return Err(Error::config(
            "除默认规则外没有可转换的条件，请检查文件后重试",
        ));
    }
    if rules.is_empty() && exceptions.is_empty() {
        return Err(Error::config(format!(
            "没有可转换的域名或 IP 规则（跳过 {} 条）；URL、路径及浏览器条件无法用于内核路由",
            summary.skipped
        )));
    }
    exceptions.extend(rules);
    summary.fallback_profiles = fallback.len();
    let group = RouteRuleGroup {
        name: summary.format.clone(),
        rules: exceptions,
        ..Default::default()
    };
    Ok((
        Bundle {
            rule_groups: vec![group],
            ..Default::default()
        },
        summary,
    ))
}

fn literal_ip(value: &str) -> Option<IpAddr> {
    let raw = value
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(value);
    raw.parse().ok()
}

fn ip(value: &str) -> Option<Matchers> {
    let cidr = value
        .parse::<ipnet::IpNet>()
        .map(|n| n.trunc().to_string())
        .ok()
        .or_else(|| literal_ip(value).map(|a| ipnet::IpNet::from(a).to_string()))?;
    Some(Matchers {
        ip_cidrs: vec![cidr],
        ..Default::default()
    })
}

fn parse_condition(value: &str) -> std::result::Result<Matchers, SkipReason> {
    let (first, rest) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    let (kind, arg) = match first.strip_suffix(':') {
        Some(kind) => (kind.to_ascii_lowercase(), rest.trim()),
        None => (String::new(), value),
    };
    match kind.as_str() {
        ""
        | "h"
        | "w"
        | "hw"
        | "wild"
        | "wildcard"
        | "host"
        | "hostw"
        | "hwild"
        | "hwildcard"
        | "hostwild"
        | "hostwildcard"
        | "hostwildcardcondition" => host(arg),
        "hr" | "r" | "regex" | "hostr" | "hregex" | "hostregex" | "hostregexcondition" => {
            (!arg.is_empty() && regex::Regex::new(arg).is_ok()).then(|| Matchers {
                domain_regexes: vec![arg.into()],
                ..Default::default()
            })
        }
        "ip" | "ipcondition" => ip(arg),
        "u"
        | "uw"
        | "url"
        | "urlw"
        | "uwild"
        | "uwildcard"
        | "urlwild"
        | "urlwildcard"
        | "urlwildcardcondition"
        | "ur"
        | "uregex"
        | "urlr"
        | "urlregex"
        | "urlregexcondition"
        | "keyword"
        | "keywordcondition" => return Err(SkipReason::Url),
        _ => return Err(SkipReason::BrowserCondition),
    }
    .ok_or(SkipReason::InvalidPattern)
}

fn host(value: &str) -> Option<Matchers> {
    if value == "*" {
        return Some(Matchers {
            network: Some(Network::TcpUdp),
            ..Default::default()
        });
    }
    if let Some(addr) = literal_ip(value) {
        return ip(&addr.to_string());
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_*?|".contains(&c))
    {
        return None;
    }
    let plain = |s: &str| {
        !s.is_empty()
            && s.split('.').all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            })
    };
    if plain(value) {
        return Some(Matchers {
            domains: vec![value.into()],
            ..Default::default()
        });
    }
    if let Some(suffix) = value.strip_prefix("*.").or_else(|| value.strip_prefix('.'))
        && plain(suffix)
    {
        return Some(Matchers {
            domain_suffixes: vec![suffix.into()],
            ..Default::default()
        });
    }

    // Alternatives stay in one matcher category (OR). A leading *. also
    // includes the apex, whereas **. requires a preceding dot.
    let mut regexes = Vec::new();
    for part in value.split('|').filter(|s| !s.is_empty()) {
        let (prefix, rest) = if let Some(s) = part.strip_prefix("**.") {
            (r"^.*\.", s)
        } else if let Some(s) = part.strip_prefix("*.").or_else(|| part.strip_prefix('.')) {
            (r"(^|\.)", s)
        } else {
            ("^", part)
        };
        if rest.is_empty() {
            return None;
        }
        let mut re = String::from(prefix);
        for c in rest.chars() {
            match c {
                '*' => re.push_str(".*"),
                '?' => re.push('.'),
                '.' => re.push_str(r"\."),
                _ => re.push(c),
            }
        }
        re.push('$');
        regexes.push(re);
    }
    (!regexes.is_empty()).then_some(Matchers {
        domain_regexes: regexes,
        ..Default::default()
    })
}
