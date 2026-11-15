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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetSource {
    #[default]
    Upstream,
    MetaCubeX,
    JsDelivr,
    Custom,
}

/// Accept a directory or a template whose substitutions are confined to the URL path.
pub fn rule_set_source_template(value: &str) -> crate::Result<String> {
    let value = value.trim();
    let error = || {
        crate::Error::config(
            "请输入 HTTP(S) 目录地址，或在路径中使用 {tag} 或 {name}；地址不能包含认证信息、查询参数或片段",
        )
    };
    let (_, after_scheme) = value.split_once("://").ok_or_else(error)?;
    let authority = after_scheme.split('/').next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains(['{', '}', '@'])
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
    {
        return Err(error());
    }
    let template = if value.contains(['{', '}']) {
        if !value.contains("{tag}") && !value.contains("{name}") {
            return Err(error());
        }
        value.to_owned()
    } else {
        format!("{}/{{tag}}.srs", value.trim_end_matches('/'))
    };
    let sample = expand_rule_set_template(&template, "geoip", "cn");
    let url = url::Url::parse(&sample).map_err(|_| error())?;
    if sample.contains(['{', '}'])
        || !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error());
    }
    Ok(template)
}

pub(crate) fn expand_rule_set_template(template: &str, kind: &str, name: &str) -> String {
    template
        .replace("{tag}", &format!("{kind}-{name}"))
        .replace("{kind}", kind)
        .replace("{name}", name)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingConfig {
    #[serde(default)]
    pub preset: RoutePreset,
    #[serde(default)]
    pub rules: Vec<RouteRule>,
    /// Ordered custom groups, evaluated before the preset/ungrouped rules.
    #[serde(default)]
    pub rule_groups: Vec<RouteRuleGroup>,
    #[serde(default)]
    pub final_action: RouteAction,
    #[serde(default)]
    pub rule_sets: Vec<RuleSet>,
    #[serde(default)]
    pub rule_set_source: RuleSetSource,
    #[serde(default)]
    pub custom_rule_set_template: String,
    #[serde(default)]
    pub domain_strategy: DomainStrategy,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            preset: RoutePreset::BypassMainland,
            rules: Vec::new(),
            rule_groups: Vec::new(),
            final_action: RouteAction::proxy(),
            rule_sets: default_rule_sets(),
            rule_set_source: RuleSetSource::Upstream,
            custom_rule_set_template: String::new(),
            domain_strategy: DomainStrategy::PreferIpv4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RouteRuleGroup {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub rules: Vec<RouteRule>,
}

impl Default for RouteRuleGroup {
    fn default() -> Self {
        Self {
            id: new_id("rule-group"),
            name: "自定义分组".into(),
            enabled: true,
            rules: Vec::new(),
        }
    }
}

impl RoutingConfig {
    /// Move one rule inside its existing group without changing rule semantics.
    pub fn move_rule_to(
        &mut self,
        id: &str,
        group: Option<&str>,
        target: &str,
        placement: Placement,
    ) -> crate::error::Result<bool> {
        let rules = match group {
            Some(group) => {
                &mut self
                    .rule_groups
                    .iter_mut()
                    .find(|g| g.id == group)
                    .ok_or_else(|| crate::error::Error::config("规则分组已不存在，请刷新后重试"))?
                    .rules
            }
            None => &mut self.rules,
        };
        move_relative(rules, id, target, placement, |rule| rule.id.as_str())
    }

    pub fn move_group_to(
        &mut self,
        id: &str,
        target: &str,
        placement: Placement,
    ) -> crate::error::Result<bool> {
        move_relative(&mut self.rule_groups, id, target, placement, |group| {
            group.id.as_str()
        })
    }

    /// A single ordering shared by compilers and route previews; PAC collects enabled direct rules.
    pub fn effective_rules(&self) -> impl Iterator<Item = &RouteRule> {
        self.rule_groups
            .iter()
            .filter(|g| g.enabled)
            .flat_map(|g| g.rules.iter())
            .chain(self.rules.iter())
            .filter(|r| r.enabled)
    }

    /// Direct mode keeps the firewall and light-weight matchers. Proxy-only
    /// geo assets must never become a prerequisite for starting the monitor.
    pub fn rules_for_mode(&self, mode: super::WorkMode) -> impl Iterator<Item = &RouteRule> {
        self.effective_rules().filter(move |r| {
            mode != super::WorkMode::Mesh
                && (mode == super::WorkMode::Proxy
                    || r.action.kind == RouteActionKind::Block
                    || (r.matchers.geosites.is_empty()
                        && r.matchers.geoips.is_empty()
                        && r.matchers.rule_sets.is_empty()))
        })
    }

    pub fn all_rules_mut(&mut self) -> impl Iterator<Item = &mut RouteRule> {
        self.rule_groups
            .iter_mut()
            .flat_map(|g| g.rules.iter_mut())
            .chain(self.rules.iter_mut())
    }
}

/// Insertion side, shared by drag targets and keyboard reorder commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Before,
    After,
}

fn move_relative<T>(
    items: &mut Vec<T>,
    id: &str,
    target: &str,
    placement: Placement,
    key: impl Fn(&T) -> &str,
) -> crate::error::Result<bool> {
    let from = items
        .iter()
        .position(|item| key(item) == id)
        .ok_or_else(|| {
            crate::error::Error::config("待移动项目已不存在或不在当前分组，请刷新后重试")
        })?;
    let to = items
        .iter()
        .position(|item| key(item) == target)
        .ok_or_else(|| {
            crate::error::Error::config("目标项目已不存在或不在当前分组，请刷新后重试")
        })?;
    if from == to {
        return Ok(false);
    }
    let insertion = to + usize::from(placement == Placement::After);
    let insertion = insertion - usize::from(from < insertion);
    if from == insertion {
        return Ok(false);
    }
    let item = items.remove(from);
    items.insert(insertion, item);
    Ok(true)
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

#[cfg(test)]
mod reorder_tests {
    use super::*;

    #[test]
    fn insertion_preserves_intermediate_order_and_checks_boundaries() {
        let mut items = vec!["a", "b", "c", "d"];
        assert!(move_relative(&mut items, "a", "c", Placement::After, |x| *x).unwrap());
        assert_eq!(items, ["b", "c", "a", "d"]);
        assert!(move_relative(&mut items, "d", "b", Placement::Before, |x| *x).unwrap());
        assert_eq!(items, ["d", "b", "c", "a"]);
        assert!(!move_relative(&mut items, "b", "c", Placement::Before, |x| *x).unwrap());
        assert!(!move_relative(&mut items, "b", "b", Placement::After, |x| *x).unwrap());
        assert!(move_relative(&mut items, "missing", "b", Placement::After, |x| *x).is_err());
        assert!(move_relative(&mut items, "b", "missing", Placement::After, |x| *x).is_err());
        assert_eq!(items, ["d", "b", "c", "a"]);
        let mut empty: Vec<&str> = vec![];
        assert!(move_relative(&mut empty, "a", "a", Placement::Before, |x| *x).is_err());
        let mut single = vec!["a"];
        assert!(!move_relative(&mut single, "a", "a", Placement::After, |x| *x).unwrap());
    }

    #[test]
    fn group_scope_and_rule_semantics_survive_reordering() {
        let mut config = RoutingConfig::default();
        let mut a = RouteRule::new("a", Matchers::default(), RouteAction::direct());
        a.enabled = false;
        let b = RouteRule::new("b", Matchers::default(), RouteAction::block());
        config.rules = vec![a.clone(), b.clone()];
        let group = RouteRuleGroup {
            rules: vec![a.clone()],
            ..Default::default()
        };
        config.rule_groups.push(group.clone());
        assert!(
            config
                .move_rule_to(&a.id, Some(&group.id), &b.id, Placement::After)
                .is_err()
        );
        assert!(
            config
                .move_rule_to(&a.id, None, &b.id, Placement::After)
                .unwrap()
        );
        assert_eq!(config.rules, [b, a]);
        assert_eq!(config.rule_groups, [group]);
        assert_eq!(config.preset, RoutePreset::BypassMainland);
    }
}
