//! Core-agnostic application model.

pub mod dns;
pub mod inbound;
pub mod node;
pub mod route;
pub mod vpn;
pub use vpn::{OpenConnectOptions, OpenVpnOptions, VpnCertificates};

pub use dns::{DnsConfig, DnsServer, DnsServerType};
pub use inbound::{Inbound, InboundKind, TunOptions, TunStack};
pub use node::{
    AnytlsOptions, Credentials, FallbackScope, GroupKind, Network, Node, NodeDraft, NodeSource,
    NodeSwitchPolicy, OutboundGroup, Protocol, RealityOptions, SwitchMode, TlsOptions, Transport,
    TransportKind, WireguardOptions,
};
pub use route::{
    DomainStrategy, Matchers, Placement, RouteAction, RouteActionKind, RoutePreset, RouteRule,
    RouteRuleGroup, RoutingConfig, RuleSet, RuleSetFormat, RuleSetSource,
};

use serde::{Deserialize, Serialize};

use crate::util::{new_id, now_unix};

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoreKind {
    Xray,
    #[default]
    SingBox,
}

impl CoreKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CoreKind::Xray => "xray",
            CoreKind::SingBox => "sing-box",
        }
    }
    pub fn binary_name(&self) -> &'static str {
        match self {
            CoreKind::Xray => "xray",
            CoreKind::SingBox => "sing-box",
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            CoreKind::Xray => "Xray",
            CoreKind::SingBox => "sing-box",
        }
    }
    pub fn all() -> [CoreKind; 2] {
        [CoreKind::Xray, CoreKind::SingBox]
    }
    pub fn capabilities(&self) -> Capabilities {
        match self {
            CoreKind::Xray => Capabilities {
                native_tun: true,
                native_selector: false,
                native_urltest: true,
                native_rule_set: true,
                hysteria2: false,
                tuic: false,
                anytls: false,
                wireguard: true,
                openvpn: false,
                openconnect: false,
                clash_connections: false,
                fake_ip: false,
                dns_split: true,
                process_rules: true,
            },
            CoreKind::SingBox => Capabilities {
                native_tun: true,
                native_selector: true,
                native_urltest: true,
                native_rule_set: true,
                hysteria2: true,
                tuic: true,
                anytls: true,
                wireguard: true,
                openvpn: true,
                openconnect: true,
                clash_connections: true,
                fake_ip: true,
                dns_split: true,
                process_rules: true,
            },
        }
    }
}

impl std::fmt::Display for CoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub native_tun: bool,
    pub native_selector: bool,
    pub native_urltest: bool,
    pub native_rule_set: bool,
    pub hysteria2: bool,
    pub tuic: bool,
    #[serde(default)]
    pub anytls: bool,
    #[serde(default)]
    pub wireguard: bool,
    #[serde(default)]
    pub openvpn: bool,
    #[serde(default)]
    pub openconnect: bool,
    pub clash_connections: bool,
    pub fake_ip: bool,
    pub dns_split: bool,
    pub process_rules: bool,
}

/// Reserved UI identity. The local monitor is never a stored proxy node.
pub const DIRECT_VIRTUAL_NODE_ID: &str = "__xrose_direct__";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    #[default]
    Direct,
    Proxy,
}

/// The editable state. Everything the compiler needs is here; secrets are
/// stored encrypted by [`crate::storage`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppModel {
    #[serde(default)]
    pub work_mode: WorkMode,
    #[serde(default = "default_profile_name")]
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub groups: Vec<OutboundGroup>,
    #[serde(default)]
    pub inbounds: Vec<inbound::Inbound>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub dns: DnsConfig,
    /// Default group used for `Proxy` actions without an explicit group.
    #[serde(default)]
    pub default_group: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_profile_name() -> String {
    "默认配置".into()
}

/// Keep the initial direct monitor free of downloadable assets. Explicit
/// preset selection still installs all its rules, including ad blocking.
pub(crate) fn starter_rules() -> Vec<RouteRule> {
    let mut rules = preset_rules(RoutePreset::BypassMainland);
    for rule in &mut rules {
        if rule.action.kind == RouteActionKind::Block {
            rule.enabled = false;
        }
    }
    rules
}

impl Default for AppModel {
    fn default() -> Self {
        let mut model = Self {
            work_mode: WorkMode::Direct,
            name: default_profile_name(),
            nodes: Vec::new(),
            groups: Vec::new(),
            inbounds: inbound::default_inbounds(),
            routing: RoutingConfig::default(),
            dns: DnsConfig::default(),
            default_group: None,
            updated_at: now_unix(),
        };
        model.ensure_default_groups();
        model.routing.rules = starter_rules();
        model.routing.bypass_china_ips = true;
        crate::dns::apply_preset(&mut model.dns, crate::dns::DnsPreset::System);
        model
    }
}

impl AppModel {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Creates the initial manual proxy group for a new profile.
    pub fn ensure_default_groups(&mut self) {
        if self.groups.is_empty() {
            let mut select = OutboundGroup::new("代理", GroupKind::Selector);
            select.id = "proxy".into();
            select.switch_policy.mode = SwitchMode::Off;
            self.groups.push(select);
        }
        if self.default_group.is_none() {
            self.default_group = self
                .groups
                .iter()
                .find(|g| g.kind == GroupKind::Selector && g.enabled)
                .or_else(|| self.groups.first())
                .map(|g| g.id.clone());
        }
        for group in &mut self.groups {
            group.repair();
        }
    }

    pub fn group(&self, id: &str) -> Option<&OutboundGroup> {
        self.groups.iter().find(|g| g.id == id)
    }
    pub fn group_mut(&mut self, id: &str) -> Option<&mut OutboundGroup> {
        self.groups.iter_mut().find(|g| g.id == id)
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn default_group_id(&self) -> Option<String> {
        self.default_group
            .clone()
            .or_else(|| self.groups.iter().find(|g| g.enabled).map(|g| g.id.clone()))
    }

    pub fn has_usable_default_group(&self, core: CoreKind) -> bool {
        self.default_group_id()
            .and_then(|id| self.group(&id))
            .is_some_and(|g| {
                g.enabled
                    && g.members
                        .iter()
                        .any(|id| self.node(id).is_some_and(|n| n.usable_with(core)))
            })
    }

    /// Checks every stored reference, including disabled rules.
    pub fn outbound_group_deletion_reason(&self, id: &str) -> Option<&'static str> {
        let Some(group) = self.group(id) else {
            return Some("出站组不存在，请刷新后重试");
        };
        if self.default_group_id().as_deref() == Some(id) {
            return Some("此组为默认代理出站，请先选择其他默认出站组");
        }
        if self
            .routing
            .rule_groups
            .iter()
            .flat_map(|g| &g.rules)
            .chain(&self.routing.rules)
            .map(|r| &r.action)
            .chain(std::iter::once(&self.routing.final_action))
            .any(|action| action.group.as_deref() == Some(id))
        {
            return Some("此组被路由规则引用，请先修改规则的出站组");
        }
        if self
            .dns
            .servers
            .iter()
            .filter_map(|s| s.detour.as_deref())
            .any(|detour| {
                detour == group.tag()
                    || (detour == id && !matches!(detour, "proxy" | "default" | "direct"))
            })
        {
            return Some("此组被 DNS 引用，请先修改 DNS 的出站设置");
        }
        None
    }

    /// Resolves a `Proxy` action to a group, falling back to the default group.
    pub fn resolve_proxy_group(&self, action: &RouteAction) -> Option<String> {
        action
            .group
            .clone()
            .filter(|id| self.group(id).map(|g| g.enabled).unwrap_or(false))
            .or_else(|| self.default_group_id())
    }

    /// Only probe groups used by a proxy route, including implicit default routes.
    pub(crate) fn managed_route_groups(&self) -> Vec<OutboundGroup> {
        let used: std::collections::HashSet<_> = self
            .routing
            .effective_rules()
            .map(|r| &r.action)
            .chain(std::iter::once(&self.routing.final_action))
            .filter(|a| a.kind == RouteActionKind::Proxy)
            .filter_map(|a| self.resolve_proxy_group(a))
            .collect();
        self.groups
            .iter()
            .filter(|g| g.enabled && g.managed() && used.contains(&g.id))
            .cloned()
            .collect()
    }

    /// Adds nodes, de-duplicating by fingerprint. Returns the number inserted.
    pub fn add_nodes(&mut self, incoming: impl IntoIterator<Item = Node>) -> usize {
        let mut seen: std::collections::HashSet<String> =
            self.nodes.iter().map(|n| n.fingerprint()).collect();
        let mut added = 0;
        for mut node in incoming {
            if node.id == DIRECT_VIRTUAL_NODE_ID {
                continue;
            }
            let fp = node.fingerprint();
            if !seen.insert(fp) {
                continue;
            }
            node.sort = self.nodes.len() as i64;
            self.nodes.push(node.clone());
            added += 1;
            // Mesh gateways also join native selector and latency groups when first installed.
            let mesh_gateway = node.is_mesh_gateway();
            for group in &mut self.groups {
                let native_mesh_group = mesh_gateway
                    && !group.managed()
                    && (group.manually_selectable() || group.kind == GroupKind::UrlTest);
                if matches!(group.id.as_str(), "proxy" | "auto") || native_mesh_group {
                    if !group.members.contains(&node.id) {
                        group.members.push(node.id.clone());
                    }
                }
            }
        }
        if added > 0 {
            for group in &mut self.groups {
                group.repair();
            }
            self.updated_at = now_unix();
        }
        added
    }

    /// Moves a node to another row's position without changing any group's active choice.
    pub fn move_node_to(&mut self, node_id: &str, target_id: &str) -> bool {
        let Some(from) = self.nodes.iter().position(|n| n.id == node_id) else {
            return false;
        };
        let Some(to) = self.nodes.iter().position(|n| n.id == target_id) else {
            return false;
        };
        if from == to {
            return false;
        }
        let node = self.nodes.remove(from);
        self.nodes.insert(to, node);
        let order: std::collections::HashMap<_, _> = self
            .nodes
            .iter_mut()
            .enumerate()
            .map(|(index, node)| {
                node.sort = index as i64;
                (node.id.clone(), index)
            })
            .collect();
        for group in &mut self.groups {
            let selected = group.active_node().cloned();
            group
                .members
                .sort_by_key(|id| order.get(id).copied().unwrap_or(usize::MAX));
            group.selected = selected;
        }
        self.updated_at = now_unix();
        true
    }

    pub fn remove_node(&mut self, node_id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.id != node_id);
        for group in &mut self.groups {
            group.members.retain(|m| m != node_id);
            if group.selected.as_deref() == Some(node_id) {
                group.selected = group.members.first().cloned();
            }
        }
        self.updated_at = now_unix();
        before != self.nodes.len()
    }

    pub fn nodes_of_subscription(&self, subscription_id: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.subscription_id.as_deref() == Some(subscription_id))
            .collect()
    }

    /// Removes every node that came from a subscription (used on refresh).
    pub fn remove_subscription_nodes(&mut self, subscription_id: &str) -> usize {
        let ids: Vec<String> = self
            .nodes
            .iter()
            .filter(|n| n.subscription_id.as_deref() == Some(subscription_id))
            .map(|n| n.id.clone())
            .collect();
        for id in &ids {
            self.remove_node(id);
        }
        ids.len()
    }

    /// New gateways join selector groups on insertion. Respect a saved gateway's
    /// enabled state and any subsequent edits to its group membership.
    pub fn can_select_mesh_exit(&self, group_id: &str, gateway: &str) -> bool {
        self.group(group_id).is_some_and(|g| {
            g.enabled
                && g.manually_selectable()
                && self
                    .node(gateway)
                    .is_none_or(|n| n.enabled && g.members.contains(&n.id))
        })
    }

    /// Switches the selector's active node. Returns true if the model changed.
    pub fn select_node(&mut self, group_id: &str, node_id: &str) -> bool {
        let Some(group) = self.group_mut(group_id) else {
            return false;
        };
        if !group.members.iter().any(|m| m == node_id) {
            return false;
        }
        let changed = group.selected.as_deref() != Some(node_id);
        group.selected = Some(node_id.to_string());
        if changed {
            self.updated_at = now_unix();
        }
        changed
    }

    /// Replaces routing rules with one of the intent presets.
    pub fn apply_preset(&mut self, preset: RoutePreset) {
        self.routing.preset = preset;
        self.routing.rules = preset_rules(preset);
        self.routing.bypass_lan = preset != RoutePreset::Global;
        self.routing.bypass_china_ips =
            matches!(preset, RoutePreset::BypassMainland | RoutePreset::GfwList);
        self.updated_at = now_unix();
    }

    /// Issues that should block starting a core; shown verbatim in the UI.
    pub fn validate(&self, core: CoreKind) -> Vec<String> {
        let mut issues = Vec::new();
        if self.work_mode == WorkMode::Proxy {
            if self.nodes.iter().all(|n| !n.usable_with(core)) {
                issues.push(format!(
                    "没有可用于 {} 的节点（检查协议支持或节点是否被禁用）",
                    core.display_name()
                ));
            }
            for group in &self.groups {
                if !group.enabled {
                    continue;
                }
                if group.members.is_empty() && group.kind != GroupKind::Selector {
                    issues.push(format!("出站组“{}”没有任何成员", group.name));
                }
            }
            if let Some(gid) = &self.default_group {
                if self.group(gid).is_none() {
                    issues.push("默认代理组不存在".into());
                }
            }
        }
        for inbound in &self.inbounds {
            if !inbound.enabled {
                continue;
            }
            if inbound.kind != InboundKind::Tun && inbound.port == 0 {
                issues.push(format!("入站 {} 的端口不能为 0", inbound.tag));
            }
            if inbound.kind == InboundKind::Tun && !core.capabilities().native_tun {
                issues.push(format!("{} 不支持 TUN，请更新内核", core.display_name()));
            }
        }
        for (idx, server) in self.dns.servers.iter().enumerate() {
            if self.work_mode == WorkMode::Direct && server.requires_proxy() {
                continue;
            }
            if server.tag.trim().is_empty() {
                issues.push(format!("DNS 服务器 #{} 缺少 tag", idx + 1));
            }
            if server.address.trim().is_empty() && !server.is_fakeip() {
                issues.push(format!("DNS 服务器“{}”缺少地址", server.tag));
            }
        }
        if self.work_mode == WorkMode::Proxy && self.dns.enabled && self.dns.servers.is_empty() {
            issues.push("DNS 已启用但没有配置服务器".into());
        }
        if self.work_mode == WorkMode::Proxy
            && self.routing.effective_rules().next().is_none()
            && self.routing.final_action.kind == RouteActionKind::Proxy
            && self.groups.iter().all(|g| g.members.is_empty())
        {
            issues.push("所有出站组都为空，规则匹配后会无可用代理".into());
        }
        issues
    }

    /// De-duplicates identical nodes and returns the number removed.
    pub fn dedupe_nodes(&mut self) -> usize {
        let mut seen = std::collections::HashSet::new();
        let before = self.nodes.len();
        self.nodes.retain(|n| seen.insert(n.fingerprint()));
        let removed = before - self.nodes.len();
        if removed > 0 {
            self.updated_at = now_unix();
        }
        removed
    }

    /// Creates a fresh id for a rule (UI helper).
    pub fn new_rule_id() -> String {
        new_id("rule")
    }
}

/// Intent-level presets. Note that geo data is expressed semantically
/// (`geosites: ["cn"]`), letting each adapter pick native geo fields or
/// generated rule-sets.
pub fn preset_rules(preset: RoutePreset) -> Vec<RouteRule> {
    match preset {
        RoutePreset::BypassMainland => {
            let mut lan = Matchers::default();
            lan.ip_cidrs = vec![
                "127.0.0.0/8".into(),
                "10.0.0.0/8".into(),
                "172.16.0.0/12".into(),
                "192.168.0.0/16".into(),
                "169.254.0.0/16".into(),
                "::1/128".into(),
                "fc00::/7".into(),
                "fe80::/10".into(),
            ];
            lan.domain_suffixes = vec![
                "lan".into(),
                "local".into(),
                "localhost".into(),
                "localdomain".into(),
            ];
            let mut cn_domains = Matchers::default();
            cn_domains.geosites = vec!["cn".into()];
            let mut cn_ips = Matchers::default();
            cn_ips.geoips = vec!["cn".into()];
            let mut foreign = Matchers::default();
            foreign.geosites = vec!["geolocation-!cn".into()];
            let mut ads = Matchers::default();
            ads.geosites = vec!["category-ads-all".into()];
            vec![
                RouteRule::new("广告拦截", ads, RouteAction::block()),
                RouteRule::new("局域网直连", lan, RouteAction::direct()),
                RouteRule::new("中国大陆域名直连", cn_domains, RouteAction::direct()),
                RouteRule::new("中国大陆 IP 直连", cn_ips, RouteAction::direct()),
                RouteRule::new("境外网站代理", foreign, RouteAction::proxy()),
            ]
        }
        RoutePreset::GfwList => {
            let mut gfw = Matchers::default();
            gfw.geosites = vec!["gfw".into()];
            let mut ads = Matchers::default();
            ads.geosites = vec!["category-ads-all".into()];
            let mut cn = Matchers::default();
            cn.geosites = vec!["cn".into()];
            vec![
                RouteRule::new("广告拦截", ads, RouteAction::block()),
                RouteRule::new("GFWList 代理", gfw, RouteAction::proxy()),
                RouteRule::new("中国大陆域名直连", cn, RouteAction::direct()),
            ]
        }
        RoutePreset::Global => Vec::new(),
        RoutePreset::ProxyApps => {
            let mut apps = Matchers::default();
            apps.process_names = vec!["telegram".into(), "chrome".into()];
            vec![RouteRule::new("指定应用代理", apps, RouteAction::proxy())]
        }
        RoutePreset::Custom => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_nodes_do_not_expand_custom_policy_groups() {
        let mut model = AppModel::default();
        // Existing profiles still contain the former built-in latency group.
        let mut legacy = OutboundGroup::new("自动选择", GroupKind::UrlTest);
        legacy.id = "auto".into();
        model.groups.push(legacy);
        let mut restricted = OutboundGroup::new("限定组", GroupKind::Selector);
        restricted.switch_policy.mode = SwitchMode::Latency;
        model.groups.push(restricted);
        let node = Node::new("节点", Protocol::Http, "node.example", 80);
        model.add_nodes([node.clone()]);
        assert_eq!(model.groups[0].members, [node.id.clone()]);
        assert_eq!(model.groups[1].members, [node.id]);
        assert!(model.groups[2].members.is_empty());
    }

    #[test]
    fn mesh_exit_selection_respects_managed_groups() {
        let mut model = AppModel::default();
        let manual = OutboundGroup::new("选择组", GroupKind::Selector);
        let mut managed = OutboundGroup::new("策略组", GroupKind::Selector);
        managed.switch_policy.mode = SwitchMode::Latency;
        let mut gateway = Node::new("出口", Protocol::Socks, "127.0.0.1", 1080);
        gateway.id = "mesh-exit-fixture".into();
        gateway.tags.push("mesh-exit".into());
        model.groups.extend([manual.clone(), managed.clone()]);
        assert!(model.can_select_mesh_exit(&manual.id, &gateway.id));
        assert!(!model.can_select_mesh_exit(&managed.id, &gateway.id));
        model.add_nodes([gateway.clone()]);
        assert!(model.can_select_mesh_exit(&manual.id, &gateway.id));
        assert!(model.group(&managed.id).unwrap().members.is_empty());
    }

    #[test]
    fn default_model_is_usable() {
        let model = AppModel::default();
        assert_eq!(model.groups.len(), 1);
        assert_eq!(model.groups[0].switch_mode(), SwitchMode::Off);
        assert!(model.default_group.is_some());
        assert!(!model.inbounds.is_empty());
        assert_eq!(CoreKind::default(), CoreKind::SingBox);
        assert_eq!(crate::Settings::default().core, CoreKind::SingBox);
        assert_eq!(model.work_mode, WorkMode::Direct);
        assert!(model.validate(CoreKind::SingBox).is_empty());
        let mut model = model;
        model.work_mode = WorkMode::Proxy;
        assert!(
            model
                .validate(CoreKind::SingBox)
                .iter()
                .any(|i| i.contains("没有可用"))
        );
    }

    #[test]
    fn adding_nodes_dedupes_and_joins_groups() {
        let mut model = AppModel::default();
        let node = Node::new("a", Protocol::Vless, "example.com", 443);
        assert_eq!(model.add_nodes(vec![node.clone(), node]), 1);
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.groups[0].members.len(), 1);
        assert_eq!(
            model.groups[0].selected.as_deref(),
            Some(model.nodes[0].id.as_str())
        );
    }

    #[test]
    fn selecting_requires_membership() {
        let mut model = AppModel::default();
        let node = Node::new("a", Protocol::Vmess, "example.com", 443);
        model.add_nodes(vec![node.clone()]);
        assert!(model.select_node("proxy", &node.id) || model.groups[0].selected.is_some());
        assert!(!model.select_node("proxy", "missing"));
    }

    #[test]
    fn presets_are_semantic() {
        let model = AppModel::default();
        assert!(
            model
                .routing
                .rules
                .iter()
                .any(|r| r.matchers.geosites.contains(&"cn".to_string()))
        );
        assert!(
            model
                .routing
                .rules
                .iter()
                .any(|r| !r.matchers.geoips.is_empty())
        );
    }

    #[test]
    fn reordering_nodes_preserves_active_choices_and_group_members() {
        let mut model = AppModel::default();
        let a = Node::new("A", Protocol::Vless, "a.example.com", 443);
        let b = Node::new("B", Protocol::Vless, "b.example.com", 443);
        let c = Node::new("C", Protocol::Vless, "c.example.com", 443);
        model.add_nodes(vec![a.clone(), b.clone(), c.clone()]);
        let selected: Vec<_> = model
            .groups
            .iter()
            .map(|g| g.active_node().cloned())
            .collect();
        assert!(model.move_node_to(&a.id, &c.id));
        assert_eq!(
            model
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec![b.id.as_str(), c.id.as_str(), a.id.as_str()]
        );
        assert_eq!(
            model.nodes.iter().map(|n| n.sort).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        for (group, selection) in model.groups.iter().zip(&selected) {
            assert_eq!(group.active_node(), selection.as_ref());
            assert_eq!(group.members.len(), 3);
        }
        assert!(model.move_node_to(&a.id, &b.id));
        assert_eq!(model.nodes[0].id, a.id);
        assert!(!model.move_node_to(&a.id, &a.id));
        assert!(!model.move_node_to("missing", &a.id));
    }
}
