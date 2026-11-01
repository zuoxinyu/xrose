use serde::{Deserialize, Serialize};

use super::Network;
use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RouteActionKind {
    #[default]
    Proxy,
    Direct,
    Block,
}

impl RouteActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteActionKind::Proxy => "proxy",
            RouteActionKind::Direct => "direct",
            RouteActionKind::Block => "block",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteAction {
    pub kind: RouteActionKind,
    #[serde(default)]
    pub group: Option<String>,
}

impl Default for RouteAction {
    fn default() -> Self {
        Self {
            kind: RouteActionKind::Proxy,
            group: None,
        }
    }
}

impl RouteAction {
    pub fn direct() -> Self {
        Self {
            kind: RouteActionKind::Direct,
            group: None,
        }
    }
    pub fn block() -> Self {
        Self {
            kind: RouteActionKind::Block,
            group: None,
        }
    }
    pub fn proxy() -> Self {
        Self::default()
    }
    pub fn proxy_via(group: impl Into<String>) -> Self {
        Self {
            kind: RouteActionKind::Proxy,
            group: Some(group.into()),
        }
    }
}

/// Matchers within a category are ORed; categories are ANDed. Empty categories
/// are ignored, matching the semantics shared by both cores.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Matchers {
    pub domains: Vec<String>,
    pub domain_suffixes: Vec<String>,
    pub domain_keywords: Vec<String>,
    pub domain_regexes: Vec<String>,
    pub geosites: Vec<String>,
    pub ip_cidrs: Vec<String>,
    pub geoips: Vec<String>,
    pub source_ip_cidrs: Vec<String>,
    pub ports: Vec<String>,
    pub source_ports: Vec<String>,
    pub network: Option<Network>,
    pub process_names: Vec<String>,
    pub process_paths: Vec<String>,
    pub rule_sets: Vec<String>,
    pub inbound_tags: Vec<String>,
    pub protocols: Vec<String>,
}

impl Matchers {
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
            && self.domain_suffixes.is_empty()
            && self.domain_keywords.is_empty()
            && self.domain_regexes.is_empty()
            && self.geosites.is_empty()
            && self.ip_cidrs.is_empty()
            && self.geoips.is_empty()
            && self.source_ip_cidrs.is_empty()
            && self.ports.is_empty()
            && self.source_ports.is_empty()
            && self.network.is_none()
            && self.process_names.is_empty()
            && self.process_paths.is_empty()
            && self.rule_sets.is_empty()
            && self.inbound_tags.is_empty()
            && self.protocols.is_empty()
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let mut push = |label: &str, values: &Vec<String>| {
            if !values.is_empty() {
                parts.push(format!("{label}:{}", values.join(",")));
            }
        };
        push("domain", &self.domains);
        push("suffix", &self.domain_suffixes);
        push("keyword", &self.domain_keywords);
        push("regex", &self.domain_regexes);
        push("geosite", &self.geosites);
        push("ip", &self.ip_cidrs);
        push("geoip", &self.geoips);
        push("src-ip", &self.source_ip_cidrs);
        push("port", &self.ports);
        push("src-port", &self.source_ports);
        push("process", &self.process_names);
        push("rule-set", &self.rule_sets);
        if let Some(net) = self.network {
            parts.push(format!("net:{}", net.as_str()));
        }
        if parts.is_empty() {
            "（无匹配条件）".into()
        } else {
            parts.join("  ")
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteRule {
    pub id: String,
    pub name: String,
    #[serde(default = "crate::model::default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub matchers: Matchers,
    #[serde(default)]
    pub action: RouteAction,
}

impl RouteRule {
    pub fn new(name: impl Into<String>, matchers: Matchers, action: RouteAction) -> Self {
        Self {
            id: new_id("rule"),
            name: name.into(),
            enabled: true,
            matchers,
            action,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutePreset {
    #[default]
    BypassMainland,
    GfwList,
    Global,
    ProxyApps,
    Custom,
}

impl RoutePreset {
    pub fn label(&self) -> &'static str {
        match self {
            RoutePreset::BypassMainland => "绕过中国大陆",
            RoutePreset::GfwList => "GFWList",
            RoutePreset::Global => "全局代理",
            RoutePreset::ProxyApps => "仅代理指定应用",
            RoutePreset::Custom => "自定义",
        }
    }
    pub fn all() -> [RoutePreset; 5] {
        [
            RoutePreset::BypassMainland,
            RoutePreset::GfwList,
            RoutePreset::Global,
            RoutePreset::ProxyApps,
            RoutePreset::Custom,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DomainStrategy {
    #[default]
    AsIs,
    PreferIpv4,
    PreferIpv6,
    IpIfNonMatch,
    IpOnDemand,
}

impl DomainStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            DomainStrategy::AsIs => "AsIs",
            DomainStrategy::PreferIpv4 => "prefer_ipv4",
            DomainStrategy::PreferIpv6 => "prefer_ipv6",
            DomainStrategy::IpIfNonMatch => "IPIfNonMatch",
            DomainStrategy::IpOnDemand => "IPOnDemand",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetFormat {
    #[default]
    Binary,
    Source,
}

impl RuleSetFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleSetFormat::Binary => "binary",
            RuleSetFormat::Source => "source",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSet {
    pub tag: String,
    pub url: String,
    #[serde(default)]
    pub format: RuleSetFormat,
    #[serde(default = "crate::model::default_true")]
    pub download_detour: bool,
    #[serde(default)]
    pub update_interval_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingConfig {
    #[serde(default)]
    pub preset: RoutePreset,
    #[serde(default)]
    pub rules: Vec<RouteRule>,
    #[serde(default)]
    pub final_action: RouteAction,
    #[serde(default)]
    pub rule_sets: Vec<RuleSet>,
    #[serde(default)]
    pub domain_strategy: DomainStrategy,
    #[serde(default = "crate::model::default_true")]
    pub bypass_lan: bool,
    #[serde(default)]
    pub bypass_china_ips: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            preset: RoutePreset::BypassMainland,
            rules: Vec::new(),
            final_action: RouteAction::proxy(),
            rule_sets: default_rule_sets(),
            domain_strategy: DomainStrategy::PreferIpv4,
            bypass_lan: true,
            bypass_china_ips: false,
        }
    }
}

pub fn default_rule_sets() -> Vec<RuleSet> {
    vec![
        RuleSet {
            tag: "geosite-cn".into(),
            url: "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-cn.srs".into(),
            format: RuleSetFormat::Binary,
            download_detour: false,
            update_interval_hours: 168,
        },
        RuleSet {
            tag: "geoip-cn".into(),
            url: "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-cn.srs".into(),
            format: RuleSetFormat::Binary,
            download_detour: false,
            update_interval_hours: 168,
        },
        RuleSet {
            tag: "geosite-geolocation-!cn".into(),
            url: "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-geolocation-!cn.srs".into(),
            format: RuleSetFormat::Binary,
            download_detour: false,
            update_interval_hours: 168,
        },
    ]
}
