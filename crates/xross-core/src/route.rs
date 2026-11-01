//! Rule evaluation for the UI's "why does this connection go there?" preview.
//!
//! Geo data is intentionally not evaluated here (it lives in downloaded rule
//! sets); those matchers are reported as "needs core" instead of silently
//! returning a wrong answer.

use std::net::IpAddr;

use crate::model::{
    AppModel, Matchers, Network, RouteAction, RouteActionKind, RouteRule, RoutingConfig,
};

#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub domain: Option<String>,
    pub destination_ip: Option<IpAddr>,
    pub destination_port: Option<u16>,
    pub source_ip: Option<IpAddr>,
    pub source_port: Option<u16>,
    pub network: Option<Network>,
    pub process_name: Option<String>,
    pub inbound_tag: Option<String>,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchState {
    Matched,
    NotMatched,
    /// A matcher needs data that is only available inside the core (geoip/geosite/rule-set).
    NeedsCore,
}

#[derive(Debug, Clone)]
pub struct RulePreview {
    pub rule: Option<RouteRule>,
    pub action: RouteAction,
    pub explanation: String,
}

pub fn evaluate(model: &AppModel, info: &ConnectionInfo) -> RulePreview {
    for rule in model.routing.rules.iter().filter(|r| r.enabled) {
        match matches_rule(&rule.matchers, info) {
            MatchState::Matched => {
                return RulePreview {
                    rule: Some(rule.clone()),
                    action: rule.action.clone(),
                    explanation: format!("命中规则“{}”", rule.name),
                }
            }
            MatchState::NeedsCore => {
                return RulePreview {
                    rule: Some(rule.clone()),
                    action: rule.action.clone(),
                    explanation: format!(
                        "规则“{}”依赖 geo/rule-set 数据，需要在 core 内运行才能确定",
                        rule.name
                    ),
                }
            }
            MatchState::NotMatched => {}
        }
    }
    RulePreview {
        rule: None,
        action: model.routing.final_action.clone(),
        explanation: "未命中任何规则，使用默认出站".into(),
    }
}

pub fn action_label(model: &AppModel, action: &RouteAction) -> String {
    match action.kind {
        RouteActionKind::Direct => "直连".into(),
        RouteActionKind::Block => "阻止".into(),
        RouteActionKind::Proxy => {
            let name = action
                .group
                .as_ref()
                .and_then(|id| model.group(id))
                .or_else(|| model.default_group_id().and_then(|id| model.group(&id)))
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "代理".into());
            if name == "代理" || name.eq_ignore_ascii_case("proxy") {
                "代理".into()
            } else {
                format!("代理 → {name}")
            }
        }
    }
}

pub fn matches_rule(matchers: &Matchers, info: &ConnectionInfo) -> MatchState {
    let mut needs_core = false;
    let domain = info
        .domain
        .as_deref()
        .map(|d| d.trim_end_matches('.').to_ascii_lowercase());

    if !matchers.domains.is_empty() || !matchers.domain_suffixes.is_empty() {
        let Some(domain) = domain.as_deref() else {
            return MatchState::NotMatched;
        };
        let exact = matchers
            .domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(domain));
        let suffix = matchers.domain_suffixes.iter().any(|s| {
            let s = s.trim_start_matches('.');
            domain == s || domain.ends_with(&format!(".{s}"))
        });
        if !exact && !suffix {
            return MatchState::NotMatched;
        }
    }
    if !matchers.domain_keywords.is_empty() {
        let Some(domain) = domain.as_deref() else {
            return MatchState::NotMatched;
        };
        if !matchers
            .domain_keywords
            .iter()
            .any(|k| domain.contains(&k.to_ascii_lowercase()))
        {
            return MatchState::NotMatched;
        }
    }
    if !matchers.domain_regexes.is_empty() {
        let Some(domain) = domain.as_deref() else {
            return MatchState::NotMatched;
        };
        let hit = matchers.domain_regexes.iter().any(|pattern| {
            regex::Regex::new(pattern)
                .map(|re| re.is_match(domain))
                .unwrap_or(false)
        });
        if !hit {
            return MatchState::NotMatched;
        }
    }
    if !matchers.geosites.is_empty() || matchers.rule_sets.iter().any(|t| t.starts_with("geosite-"))
    {
        // geosites only affect the result if this rule also matches on other
        // fields; mark that we cannot be definitive.
        needs_core = true;
    }

    if !matchers.ip_cidrs.is_empty() {
        let Some(ip) = info.destination_ip else {
            return MatchState::NotMatched;
        };
        if !matchers.ip_cidrs.iter().any(|cidr| cidr_contains(cidr, ip)) {
            return MatchState::NotMatched;
        }
    }
    if !matchers.geoips.is_empty() || matchers.rule_sets.iter().any(|t| t.starts_with("geoip-")) {
        needs_core = true;
    }
    if !matchers.source_ip_cidrs.is_empty() {
        let Some(ip) = info.source_ip else {
            return MatchState::NotMatched;
        };
        if !matchers
            .source_ip_cidrs
            .iter()
            .any(|cidr| cidr_contains(cidr, ip))
        {
            return MatchState::NotMatched;
        }
    }
    if !matchers.ports.is_empty() {
        let Some(port) = info.destination_port else {
            return MatchState::NotMatched;
        };
        if !port_in_specs(&matchers.ports, port) {
            return MatchState::NotMatched;
        }
    }
    if !matchers.source_ports.is_empty() {
        let Some(port) = info.source_port else {
            return MatchState::NotMatched;
        };
        if !port_in_specs(&matchers.source_ports, port) {
            return MatchState::NotMatched;
        }
    }
    if !matchers.process_names.is_empty() {
        let Some(process) = info.process_name.as_deref() else {
            return MatchState::NotMatched;
        };
        if !matchers
            .process_names
            .iter()
            .any(|p| process.eq_ignore_ascii_case(p))
        {
            return MatchState::NotMatched;
        }
    }
    if !matchers.process_paths.is_empty() {
        let Some(process) = info.process_name.as_deref() else {
            return MatchState::NotMatched;
        };
        if !matchers
            .process_paths
            .iter()
            .any(|p| process.starts_with(p.as_str()))
        {
            return MatchState::NotMatched;
        }
    }
    if !matchers.inbound_tags.is_empty() {
        let Some(tag) = info.inbound_tag.as_deref() else {
            return MatchState::NotMatched;
        };
        if !matchers.inbound_tags.iter().any(|t| t == tag) {
            return MatchState::NotMatched;
        }
    }
    if !matchers.protocols.is_empty() {
        let Some(protocol) = info.protocol.as_deref() else {
            return MatchState::NotMatched;
        };
        if !matchers.protocols.iter().any(|p| p == protocol) {
            return MatchState::NotMatched;
        }
    }
    if let Some(network) = matchers.network {
        if let Some(actual) = info.network {
            let compatible =
                network == Network::TcpUdp || actual == network || actual == Network::TcpUdp;
            if !compatible {
                return MatchState::NotMatched;
            }
        } else {
            return MatchState::NotMatched;
        }
    }

    if needs_core {
        MatchState::NeedsCore
    } else {
        MatchState::Matched
    }
}

pub fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let trimmed = cidr.trim();
    if let Some((addr, prefix)) = trimmed.split_once('/') {
        let Ok(base) = addr.parse::<IpAddr>() else {
            return false;
        };
        let Ok(prefix) = prefix.parse::<u8>() else {
            return false;
        };
        match (base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                let prefix = prefix.min(32);
                let mask = if prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - prefix)
                };
                (u32::from(base) & mask) == (u32::from(ip) & mask)
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                let prefix = prefix.min(128);
                let mask = if prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - prefix)
                };
                (u128::from(base) & mask) == (u128::from(ip) & mask)
            }
            _ => false,
        }
    } else {
        trimmed.parse::<IpAddr>().map(|a| a == ip).unwrap_or(false)
    }
}

fn port_in_specs(specs: &[String], port: u16) -> bool {
    specs.iter().any(|spec| {
        spec.split(',').any(|part| {
            let part = part.trim();
            if let Some((a, b)) = part.split_once('-') {
                match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
                    (Ok(a), Ok(b)) => port >= a && port <= b,
                    _ => false,
                }
            } else {
                part.parse::<u16>().map(|p| p == port).unwrap_or(false)
            }
        })
    })
}

/// Applies the "bypass LAN" switch to a custom routing config.
pub fn apply_bypass_lan(routing: &mut RoutingConfig, enabled: bool) {
    routing.bypass_lan = enabled;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Matchers;

    fn info(domain: &str) -> ConnectionInfo {
        ConnectionInfo {
            domain: Some(domain.into()),
            ..Default::default()
        }
    }

    #[test]
    fn cidr_matching() {
        assert!(cidr_contains("10.0.0.0/8", "10.1.2.3".parse().unwrap()));
        assert!(!cidr_contains("10.0.0.0/8", "11.1.2.3".parse().unwrap()));
        assert!(cidr_contains("::1/128", "::1".parse().unwrap()));
        assert!(cidr_contains("1.1.1.1", "1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn suffix_matching() {
        let mut matchers = Matchers::default();
        matchers.domain_suffixes = vec!["example.com".into()];
        assert_eq!(
            matches_rule(&matchers, &info("a.example.com")),
            MatchState::Matched
        );
        assert_eq!(
            matches_rule(&matchers, &info("example.com")),
            MatchState::Matched
        );
        assert_eq!(
            matches_rule(&matchers, &info("badexample.com")),
            MatchState::NotMatched
        );
    }

    #[test]
    fn first_rule_wins() {
        let mut model = AppModel::default();
        model.routing.rules = vec![
            RouteRule::new(
                "direct example",
                Matchers {
                    domains: vec!["example.com".into()],
                    ..Default::default()
                },
                RouteAction::direct(),
            ),
            RouteRule::new("all", Matchers::default(), RouteAction::proxy()),
        ];
        let preview = evaluate(&model, &info("example.com"));
        assert_eq!(preview.rule.unwrap().name, "direct example");
        assert_eq!(preview.action.kind, RouteActionKind::Direct);
    }

    #[test]
    fn geosites_need_core() {
        let mut matchers = Matchers::default();
        matchers.geosites = vec!["cn".into()];
        assert_eq!(
            matches_rule(&matchers, &info("example.com")),
            MatchState::NeedsCore
        );
    }

    #[test]
    fn ports_and_network() {
        let mut matchers = Matchers::default();
        matchers.ports = vec!["80,443,8000-8100".into()];
        matchers.network = Some(Network::Tcp);
        let mut details = info("example.com");
        details.destination_port = Some(8050);
        details.network = Some(Network::Tcp);
        assert_eq!(matches_rule(&matchers, &details), MatchState::Matched);
        details.destination_port = Some(9090);
        assert_eq!(matches_rule(&matchers, &details), MatchState::NotMatched);
    }
}
