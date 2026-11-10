//! Portable, additive routing exchange. Only rule data crosses this boundary.
mod omega;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};
use crate::model::{AppModel, RoutePreset, RouteRule, RouteRuleGroup, RuleSet};
use crate::util::new_id;
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const FORMAT: &str = "xrose-routing";

#[derive(Debug, Clone)]
pub enum ExportScope {
    All,
    RuleSets,
    Group(String),
    Rule(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    format: String,
    version: u32,
    #[serde(default)]
    rules: Vec<RouteRule>,
    #[serde(default)]
    rule_groups: Vec<RouteRuleGroup>,
    #[serde(default)]
    rule_sets: Vec<RuleSet>,
    /// Names allow references to be resolved on a different installation.
    #[serde(default)]
    outbound_names: BTreeMap<String, String>,
}

impl Bundle {
    fn rules(&self) -> impl Iterator<Item = &RouteRule> {
        self.rules
            .iter()
            .chain(self.rule_groups.iter().flat_map(|g| &g.rules))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ImportSummary {
    pub format: String,
    pub rules: usize,
    pub groups: usize,
    pub rule_sets: usize,
    pub skipped: usize,
    pub fallback_profiles: usize,
    pub catch_all: bool,
    /// Sample diagnostics contain positions and categories, never source contents.
    pub skipped_lines: Vec<SkippedCondition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedCondition {
    pub line: usize,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Url,
    BrowserCondition,
    InvalidPattern,
}

pub struct PreparedImport {
    bundle: Bundle,
    pub summary: ImportSummary,
}

pub fn read_text(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::config("读取规则文件失败，请检查文件权限"))?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::config("规则文件超过 8 MiB，请拆分后重试"));
    }
    String::from_utf8(bytes).map_err(|_| Error::config("规则文件需为 UTF-8 文本"))
}

pub fn read_file(path: &Path) -> Result<String> {
    read_text(
        std::fs::File::open(path).map_err(|_| Error::config("无法打开规则文件，请检查文件权限"))?,
    )
}

pub fn export(model: &AppModel, scope: &ExportScope) -> Result<String> {
    let mut bundle = Bundle::default();
    let config = &model.routing;
    match scope {
        ExportScope::All => {
            bundle.rules = config.rules.clone();
            bundle.rule_groups = config.rule_groups.clone();
            bundle.rule_sets = config
                .rule_sets
                .iter()
                .filter_map(|set| crate::adapters::resolve_rule_set(model, &set.tag))
                .collect();
        }
        ExportScope::RuleSets => {
            bundle.rule_sets = crate::adapters::downloadable_rule_sets(model);
        }
        ExportScope::Group(id) => bundle.rule_groups.push(
            config
                .rule_groups
                .iter()
                .find(|g| &g.id == id)
                .ok_or_else(|| Error::config("分组已不存在，请重新选择"))?
                .clone(),
        ),
        ExportScope::Rule(id) => {
            let (rule, group_enabled) = config
                .rules
                .iter()
                .map(|r| (r, true))
                .chain(
                    config
                        .rule_groups
                        .iter()
                        .flat_map(|g| g.rules.iter().map(move |r| (r, g.enabled))),
                )
                .find(|(r, _)| &r.id == id)
                .ok_or_else(|| Error::config("规则已不存在，请重新选择"))?;
            let mut rule = rule.clone();
            rule.enabled &= group_enabled;
            bundle.rules.push(rule);
        }
    }
    encode_bundle(bundle, model)
}

/// Convert only the supplied rules; no profile changes or node data are exported.
pub fn convert(text: &str, model: &AppModel) -> Result<(String, ImportSummary)> {
    let plan = prepare(text, model)?;
    Ok((encode_bundle(plan.bundle, model)?, plan.summary))
}

fn encode_bundle(mut bundle: Bundle, model: &AppModel) -> Result<String> {
    bundle.format = FORMAT.into();
    bundle.version = 1;
    let referenced: BTreeSet<_> = bundle
        .rules()
        .flat_map(|r| &r.matchers.rule_sets)
        .cloned()
        .collect();
    for tag in referenced {
        if !bundle.rule_sets.iter().any(|set| set.tag == tag) {
            let set = crate::adapters::resolve_rule_set(model, &tag)
                .ok_or_else(|| Error::config("规则引用了未定义的规则集，请先补齐来源"))?;
            bundle.rule_sets.push(set);
        }
    }
    bundle.outbound_names = bundle
        .rules()
        .filter_map(|r| {
            let g = model.group(r.action.group.as_deref()?)?;
            Some((g.id.clone(), g.name.clone()))
        })
        .collect();
    validate_sets(&bundle, model)?;
    let output = serde_json::to_string_pretty(&bundle)?;
    if output.len() > MAX_BYTES {
        return Err(Error::config("导出超过 8 MiB，请分组导出"));
    }
    Ok(output)
}

pub(crate) fn safe_tag(tag: &str) -> bool {
    let stem = tag.split('.').next().unwrap_or("").to_ascii_uppercase();
    !tag.is_empty()
        && tag.len() <= 200
        && !tag.starts_with('.')
        && !tag.ends_with('.')
        && !matches!(stem.as_str(), "CON" | "NUL" | "PRN" | "AUX")
        && !((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
        && tag
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.!@".contains(c))
}

fn validate_sets(bundle: &Bundle, model: &AppModel) -> Result<()> {
    // Tags are also local cache filenames, including implicitly generated geo tags.
    let defined: BTreeSet<_> = bundle
        .rule_sets
        .iter()
        .chain(&model.routing.rule_sets)
        .map(|set| set.tag.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    let by_tag: BTreeMap<_, _> = model
        .routing
        .rule_sets
        .iter()
        .map(|s| (s.tag.to_lowercase(), s))
        .collect();
    for set in &bundle.rule_sets {
        let key = set.tag.to_lowercase();
        if !safe_tag(&set.tag) || !seen.insert(key.clone()) {
            return Err(Error::config(
                "规则集标签无效或重复，请使用不同的文件名且不含路径",
            ));
        }
        let url = url::Url::parse(&set.url).ok().filter(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.has_host()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
        });
        if url.is_none() {
            return Err(Error::config(
                "规则集来源需为不含认证信息、查询参数或片段的 HTTP(S) 地址",
            ));
        }
        if by_tag.get(&key).is_some_and(|existing| {
            **existing != *set
                && crate::adapters::resolve_rule_set(model, &existing.tag).as_ref() != Some(set)
        }) {
            return Err(Error::config(
                "已有同名规则集使用不同配置，请先统一标签和来源",
            ));
        }
    }
    for rule in bundle.rules() {
        let m = &rule.matchers;
        if m.rule_sets.iter().any(|s| !safe_tag(s))
            || m.geoips.iter().chain(&m.geosites).any(|s| !safe_tag(s))
        {
            return Err(Error::config(
                "规则包含无效的规则集或 Geo 标签，请移除路径字符",
            ));
        }
        for tag in &m.rule_sets {
            if !defined.contains(tag.as_str()) && crate::adapters::default_rule_set(tag).is_none() {
                return Err(Error::config("缺少引用的规则集，请同时导入规则集定义"));
            }
        }
    }
    Ok(())
}

pub fn prepare(text: &str, model: &AppModel) -> Result<PreparedImport> {
    if text.len() > MAX_BYTES {
        return Err(Error::config("规则文件超过 8 MiB，请拆分后重试"));
    }
    let text = text.trim_start_matches('\u{feff}').trim();
    let (mut bundle, mut summary) = if text.starts_with('{') {
        let mut bundle: Bundle = serde_json::from_str(text).map_err(|e| {
            Error::config(format!(
                "第 {} 行：不是有效的 Xrose 路由文件，请检查格式",
                e.line()
            ))
        })?;
        if bundle.format != FORMAT || bundle.version != 1 {
            return Err(Error::config(
                "不支持此路由文件版本，请使用 Xrose 路由格式 v1",
            ));
        }
        let mut fallback = BTreeSet::new();
        for rule in bundle
            .rules
            .iter_mut()
            .chain(bundle.rule_groups.iter_mut().flat_map(|g| &mut g.rules))
        {
            if let Some(id) = rule.action.group.clone()
                && model.group(&id).is_none()
            {
                let name = bundle.outbound_names.get(&id);
                rule.action.group = name.and_then(|n| named_group(model, n));
                if rule.action.group.is_none() {
                    fallback.insert(id.clone());
                }
            }
        }
        (
            bundle,
            ImportSummary {
                format: "Xrose".into(),
                fallback_profiles: fallback.len(),
                ..Default::default()
            },
        )
    } else {
        omega::convert(text, model)?
    };
    validate_sets(&bundle, model)?;
    for group in &mut bundle.rule_groups {
        group.id = new_id("rule-group");
    }
    for rule in bundle
        .rules
        .iter_mut()
        .chain(bundle.rule_groups.iter_mut().flat_map(|g| &mut g.rules))
    {
        rule.id = new_id("rule");
    }
    let catch_all = crate::model::Matchers {
        network: Some(crate::model::Network::TcpUdp),
        ..Default::default()
    };
    summary.catch_all |= bundle.rules().any(|r| r.enabled && r.matchers == catch_all);
    summary.rules = bundle.rules().count();
    summary.groups = bundle.rule_groups.len();
    summary.rule_sets = bundle.rule_sets.len();
    if summary.rules + summary.groups + summary.rule_sets == 0 {
        return Err(Error::config("文件中没有可导入的规则、分组或规则集"));
    }
    Ok(PreparedImport { bundle, summary })
}

fn named_group(model: &AppModel, name: &str) -> Option<String> {
    let mut groups = model.groups.iter().filter(|g| g.name == name && g.enabled);
    let first = groups.next()?;
    groups.next().is_none().then(|| first.id.clone())
}

impl PreparedImport {
    pub(crate) fn apply(self, model: &mut AppModel) -> Result<bool> {
        // Recheck references against current state after the user reviews the preview.
        validate_sets(&self.bundle, model)?;
        if self.bundle.rules().any(|r| {
            r.action
                .group
                .as_ref()
                .is_some_and(|id| model.group(id).is_none())
        }) {
            return Err(Error::config("出站组已更改，请重新预览后导入"));
        }
        let routing = &mut model.routing;
        let mut changed = !self.bundle.rules.is_empty() || !self.bundle.rule_groups.is_empty();
        if !self.bundle.rules.is_empty() {
            routing.preset = RoutePreset::Custom;
        }
        routing.rules.extend(self.bundle.rules);
        routing.rule_groups.extend(self.bundle.rule_groups);
        for set in self.bundle.rule_sets {
            if !routing.rule_sets.iter().any(|s| s.tag == set.tag) {
                routing.rule_sets.push(set);
                changed = true;
            }
        }
        if changed {
            model.updated_at = crate::util::now_unix();
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupKind, Matchers, Network, OutboundGroup, RouteAction, RouteActionKind};
    use crate::route::{ConnectionInfo, evaluate};

    fn decision(model: &AppModel, host: &str) -> RouteAction {
        evaluate(
            model,
            &ConnectionInfo {
                domain: Some(host.into()),
                network: Some(Network::Tcp),
                ..Default::default()
            },
        )
        .action
    }

    #[test]
    fn conditions_preserve_order_notes_exceptions_and_default() {
        let mut model = AppModel {
            work_mode: crate::model::WorkMode::Proxy,
            ..Default::default()
        };
        let via = OutboundGroup::new("Work", GroupKind::Selector);
        model.groups.push(via.clone());
        let text = "\u{feff}[SwitchyOmega Conditions]\r\n@with result\r\n; comment\r\n@note Exact\r\napi.example.org +Work\r\n!private.example.org\r\n*.example.org +proxy\r\nUrlWildcard: https://outside.example/* +proxy\r\n* +direct";
        let plan = prepare(text, &model).unwrap();
        assert_eq!(
            plan.summary.skipped_lines,
            [SkippedCondition {
                line: 8,
                reason: SkipReason::Url
            }]
        );
        assert_eq!(plan.summary.rules, 4);
        assert!(plan.summary.catch_all);
        plan.apply(&mut model).unwrap();
        assert_eq!(model.routing.rule_groups[0].rules[0].name, "Exact");
        assert_eq!(
            decision(&model, "api.example.org"),
            RouteAction::proxy_via(via.id)
        );
        assert_eq!(
            decision(&model, "private.example.org"),
            RouteAction::direct()
        );
        assert_eq!(decision(&model, "example.org"), RouteAction::proxy());
        assert_eq!(decision(&model, "www.example.org"), RouteAction::proxy());
        assert_eq!(decision(&model, "badexample.org"), RouteAction::direct());
    }

    #[test]
    fn autoproxy_plain_and_base64_prioritize_exceptions_without_broadening_urls() {
        let src = "[AutoProxy 0.2.9]\n! comment\n||example.org\n@@||safe.example.org\n|https://other.example/path\nkeyword\n/regex/";
        for text in [src.to_owned(), crate::util::encode_base64(src.as_bytes())] {
            let mut model = AppModel {
                work_mode: crate::model::WorkMode::Proxy,
                ..Default::default()
            };
            let plan = prepare(&text, &model).unwrap();
            assert_eq!(
                plan.summary
                    .skipped_lines
                    .iter()
                    .map(|s| s.line)
                    .collect::<Vec<_>>(),
                [5, 6, 7]
            );
            assert_eq!(plan.summary.rules, 2);
            plan.apply(&mut model).unwrap();
            assert_eq!(decision(&model, "safe.example.org"), RouteAction::direct());
            assert_eq!(decision(&model, "test.example.org"), RouteAction::proxy());
        }
    }

    #[test]
    fn wildcard_alternatives_ip_and_incompatible_conditions() {
        let mut model = AppModel {
            work_mode: crate::model::WorkMode::Proxy,
            ..Default::default()
        };
        let text = "[SwitchyOmega Conditions]\n@with results\nHost: **.example.org|api?.example.net +proxy\nIp: 192.0.2.0/24 +proxy\nHR: ^edge[0-9]+$ +proxy\nHostRegex: (?<=a)b +proxy\nUrlRegex: .* +proxy\n* +direct";
        let plan = prepare(text, &model).unwrap();
        assert_eq!(
            plan.summary
                .skipped_lines
                .iter()
                .map(|s| s.line)
                .collect::<Vec<_>>(),
            [6, 7]
        );
        plan.apply(&mut model).unwrap();
        assert_eq!(
            decision(&model, "example.org").kind,
            RouteActionKind::Direct
        );
        for host in ["test.example.org", "api9.example.net", "edge123"] {
            assert_eq!(decision(&model, host), RouteAction::proxy());
        }
        let info = ConnectionInfo {
            destination_ip: Some("192.0.2.5".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(evaluate(&model, &info).action, RouteAction::proxy());
    }

    #[test]
    fn portable_bundle_keeps_order_flags_dependencies_and_remaps_group_ids() {
        let mut src = AppModel::default();
        let set = RuleSet {
            tag: "example-set".into(),
            url: "https://rules.example.org/sites.srs".into(),
            ..src.routing.rule_sets[0].clone()
        };
        src.routing.rule_sets.push(set.clone());
        let proxy = OutboundGroup::new("Work", GroupKind::Selector);
        src.groups.push(proxy.clone());
        let mut a = RouteRule::new(
            "A",
            Matchers {
                rule_sets: vec![set.tag.clone()],
                ..Default::default()
            },
            RouteAction::proxy_via(proxy.id),
        );
        a.enabled = false;
        let b = RouteRule::new(
            "B",
            Matchers {
                domains: vec!["example.org".into()],
                ..Default::default()
            },
            RouteAction::block(),
        );
        let group = RouteRuleGroup {
            enabled: false,
            rules: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        src.routing.rule_groups.push(group.clone());
        let standalone = export(&src, &ExportScope::Rule(b.id.clone())).unwrap();
        let standalone: Bundle = serde_json::from_str(&standalone).unwrap();
        assert!(!standalone.rules[0].enabled);
        let json = export(&src, &ExportScope::Group(group.id.clone())).unwrap();
        assert!(!json.contains("\"nodes\""));
        let mut dst = AppModel::default();
        let dest = OutboundGroup::new("Work", GroupKind::Selector);
        dst.groups.push(dest.clone());
        let original = dst.routing.clone();
        let plan = prepare(&json, &dst).unwrap();
        assert_eq!(
            (
                plan.summary.rules,
                plan.summary.rule_sets,
                plan.summary.fallback_profiles
            ),
            (2, 1, 0)
        );
        plan.apply(&mut dst).unwrap();
        let g = &dst.routing.rule_groups[0];
        assert!(!g.enabled && !g.rules[0].enabled);
        assert_ne!(g.id, group.id);
        assert_ne!(g.rules[0].id, a.id);
        assert_eq!(g.rules[0].action, RouteAction::proxy_via(dest.id));
        assert_eq!(g.rules[0].matchers, a.matchers);
        assert_eq!(g.rules[1].action, b.action);
        assert_eq!(dst.routing.rules, original.rules);
        assert_eq!(dst.routing.final_action, original.final_action);
        assert_eq!(dst.routing.preset, original.preset);
        assert!(dst.routing.rule_sets.contains(&set));
        let sets = export(&dst, &ExportScope::RuleSets).unwrap();
        let mut restored = AppModel::default();
        let plan = prepare(&sets, &restored).unwrap();
        assert_eq!(plan.summary.rules, 0);
        plan.apply(&mut restored).unwrap();
        assert!(
            !prepare(&sets, &restored)
                .unwrap()
                .apply(&mut restored)
                .unwrap()
        );
    }

    #[test]
    fn rejects_unrecognized_files_incomplete_results_and_unsafe_set_sources() {
        let m = AppModel::default();
        for text in [
            "",
            "unknown",
            "{}",
            "[SwitchyOmega Conditions]\n@with result\nexample.org +proxy",
            "[SwitchyOmega Conditions]\n@with result\nexample.org\n* +direct",
            "[SwitchyOmega Conditions]\n@with result\n* +direct\nexample.org +proxy",
            "[SwitchyOmega Conditions]\nUrlWildcard: https://example.org/private",
            r#"{"format":"xrose-routing","version":2,"rules":[]}"#,
        ] {
            assert!(prepare(text, &m).is_err());
        }
        for (tag, url) in [
            ("../outside", "https://example.org/a"),
            ("a", "file:///outside"),
            ("a", "https://example.org/a?key=value"),
            ("geoip-cn", "https://example.org/other"),
        ] {
            let value = serde_json::json!({"format": FORMAT, "version": 1, "rule_sets": [{"tag":tag,"url":url}]});
            assert!(prepare(&value.to_string(), &m).is_err());
        }
    }
}
