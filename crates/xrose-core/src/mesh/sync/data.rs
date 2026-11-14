//! Portable data and item-level reconciliation. The receiving device owns its listeners and identity.
use crate::{
    Error, Result,
    mesh::MeshProfile,
    model::{AppModel, Node, NodeSource, RouteRule, RouteRuleGroup, RuleSet},
    subscription::Subscription,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Nodes,
    Subscriptions,
    Networks,
    Rules,
}
impl Category {
    pub const ALL: [Self; 4] = [
        Self::Nodes,
        Self::Subscriptions,
        Self::Networks,
        Self::Rules,
    ];
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum Item {
    Node(Node),
    Subscription(Subscription),
    Network(MeshProfile),
    Rule(RouteRule),
    Group(RouteRuleGroup),
    Set(RuleSet),
}
impl Item {
    fn parts(&self) -> (Category, &str, &str, &str) {
        match self {
            Self::Node(n) => (Category::Nodes, "node", &n.id, &n.name),
            Self::Subscription(s) => (Category::Subscriptions, "sub", &s.id, &s.name),
            Self::Network(p) => (Category::Networks, "mesh", &p.id, &p.name),
            Self::Rule(r) => (Category::Rules, "rule", &r.id, &r.name),
            Self::Group(g) => (Category::Rules, "group", &g.id, &g.name),
            Self::Set(s) => (Category::Rules, "set", &s.tag, &s.tag),
        }
    }
    pub fn key(&self) -> String {
        let (_, kind, id, _) = self.parts();
        format!("{kind}:{id}")
    }
}
#[derive(Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Bundle {
    pub items: Vec<Item>,
    #[serde(default)]
    outbound_names: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Difference {
    New,
    Conflict,
    Identical,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewRow {
    pub key: String,
    pub category: Category,
    pub name: String,
    pub difference: Difference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareRow {
    pub key: String,
    pub category: Category,
    pub name: String,
}

fn portable_mesh(p: &MeshProfile) -> MeshProfile {
    MeshProfile {
        id: p.id.clone(),
        name: p.name.clone(),
        provider: p.provider,
        // The device hostname is local and must not be sent with portable settings.
        hostname: "xrose".into(),
        network_name: p.network_name.clone(),
        easytier: super::super::EasyTierOptions {
            device: Default::default(),
            ..p.easytier.clone()
        },
        network_secret: p.network_secret.clone(),
        peers: p.peers.clone(),
        remote_subnets: p.remote_subnets.clone(),
        control_url: p.control_url.clone(),
        ..Default::default()
    }
}
fn portable_sub(s: &Subscription) -> Subscription {
    let mut copy = Subscription::new(&s.name, &s.url);
    copy.id = s.id.clone();
    copy.enabled = s.enabled;
    copy.user_agent = s.user_agent.clone();
    copy.headers = s.headers.clone();
    copy.update_interval_secs = s.update_interval_secs;
    copy
}
fn same_rule(a: &RouteRule, b: &RouteRule) -> bool {
    a.name == b.name && a.matchers == b.matchers && a.enabled == b.enabled && a.action == b.action
}

impl Bundle {
    pub fn capture(
        model: &AppModel,
        subs: &[Subscription],
        networks: &[MeshProfile],
        categories: &BTreeSet<Category>,
    ) -> Self {
        Self::collect(model, subs, networks, categories, true)
    }
    pub fn local(model: &AppModel, subs: &[Subscription], networks: &[MeshProfile]) -> Self {
        Self::collect(model, subs, networks, &Category::ALL.into(), false)
    }
    fn collect(
        model: &AppModel,
        subs: &[Subscription],
        networks: &[MeshProfile],
        categories: &BTreeSet<Category>,
        sharing: bool,
    ) -> Self {
        let mut items = Vec::new();
        let mut outbound_names = BTreeMap::new();
        if categories.contains(&Category::Nodes) {
            items.extend(
                model
                    .nodes
                    .iter()
                    .filter(|n| !n.id.starts_with("mesh-exit-"))
                    .cloned()
                    .map(|mut n| {
                        n.sort = 0;
                        Item::Node(n)
                    }),
            );
        }
        if categories.contains(&Category::Subscriptions) {
            items.extend(subs.iter().map(|s| Item::Subscription(portable_sub(s))));
        }
        if categories.contains(&Category::Networks) {
            items.extend(networks.iter().map(|p| Item::Network(portable_mesh(p))));
        }
        if categories.contains(&Category::Rules) {
            let preset = [
                crate::model::RoutePreset::BypassMainland,
                crate::model::RoutePreset::GfwList,
                crate::model::RoutePreset::ProxyApps,
            ]
            .into_iter()
            .flat_map(crate::model::preset_rules)
            .chain(crate::model::starter_rules())
            .collect::<Vec<_>>();
            let defaults = crate::model::route::default_rule_sets();
            items.extend(
                model
                    .routing
                    .rule_sets
                    .iter()
                    .filter(|s| !sharing || !defaults.contains(s))
                    .cloned()
                    .map(Item::Set),
            );
            items.extend(model.routing.rule_groups.iter().cloned().map(Item::Group));
            items.extend(
                model
                    .routing
                    .rules
                    .iter()
                    .filter(|r| !sharing || !preset.iter().any(|p| same_rule(p, r)))
                    .cloned()
                    .map(Item::Rule),
            );
            for rule in model
                .routing
                .rules
                .iter()
                .chain(model.routing.rule_groups.iter().flat_map(|g| &g.rules))
            {
                if let Some(g) = rule.action.group.as_deref().and_then(|id| model.group(id)) {
                    outbound_names.insert(g.id.clone(), g.name.clone());
                }
            }
        }
        if !sharing {
            outbound_names = model
                .groups
                .iter()
                .filter(|g| g.enabled)
                .map(|g| (g.id.clone(), g.name.clone()))
                .collect();
        }
        Self {
            items,
            outbound_names,
        }
    }
    pub fn catalog(&self) -> Vec<ShareRow> {
        self.items
            .iter()
            .map(|i| ShareRow {
                key: i.key(),
                category: i.parts().0,
                name: i.parts().3.chars().take(128).collect(),
            })
            .collect()
    }
    pub fn selected(&self, keys: &BTreeSet<String>) -> Result<Self> {
        let items: Vec<_> = self
            .items
            .iter()
            .filter(|i| keys.contains(&i.key()))
            .cloned()
            .collect();
        if items.is_empty() || items.len() != keys.len() {
            return Err(Error::config("请选择当前列表中的共享项目"));
        }
        let mut outbound_names = BTreeMap::new();
        let mut add = |r: &RouteRule| {
            if let Some(id) = &r.action.group
                && let Some(name) = self.outbound_names.get(id)
            {
                outbound_names.insert(id.clone(), name.clone());
            }
        };
        for i in &items {
            match i {
                Item::Rule(r) => add(r),
                Item::Group(g) => g.rules.iter().for_each(&mut add),
                _ => {}
            }
        }
        let selected = Self {
            items,
            outbound_names,
        };
        selected.validate()?;
        Ok(selected)
    }
    pub fn validate(&self) -> Result<()> {
        if self.items.is_empty() || self.items.len() > 10000 {
            return Err(Error::config("请选择包含数据的类别，每次最多同步一万项"));
        }
        let mut seen = BTreeSet::new();
        for item in &self.items {
            let (_, _, id, _) = item.parts();
            if id.is_empty()
                || id.len() > 256
                || id.contains(['/', '\\'])
                || id.chars().any(char::is_control)
                || !seen.insert(item.key())
            {
                return Err(Error::config("共享配置包含无效或重复的标识"));
            }
            match item {
                Item::Network(p) => {
                    portable_mesh(p).validate()?;
                }
                Item::Node(n)
                    if n.id == crate::model::DIRECT_VIRTUAL_NODE_ID
                        || n.id.starts_with("mesh-exit-")
                        || n.tags.iter().any(|t| t == "mesh-exit")
                        || n.server.trim().is_empty()
                        || n.port == 0 =>
                {
                    return Err(Error::config("共享配置包含无效节点"));
                }
                Item::Subscription(s)
                    if !url::Url::parse(&s.url)
                        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host()) =>
                {
                    return Err(Error::config("同步的节点订阅须使用 HTTP(S) 地址"));
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Translate references to an existing destination identity before comparing or merging.
    pub fn resolve_local_ids(&mut self, local: &Self) {
        let subscriptions: BTreeMap<_, _> = local
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Subscription(s) => Some((&s.url, &s.id)),
                _ => None,
            })
            .collect();
        let ids: BTreeSet<_> = subscriptions.values().copied().collect();
        let aliases: BTreeMap<_, _> = self
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Subscription(s) if !ids.contains(&s.id) => subscriptions
                    .get(&s.url)
                    .map(|id| (s.id.clone(), (*id).clone())),
                _ => None,
            })
            .collect();
        let resolve = |r: &mut RouteRule| {
            if let Some(id) = &r.action.group
                && !local.outbound_names.contains_key(id)
                && let Some(name) = self.outbound_names.get(id)
            {
                let mut matched = local.outbound_names.iter().filter(|(_, n)| *n == name);
                if let Some((target, _)) = matched.next()
                    && matched.next().is_none()
                {
                    r.action.group = Some(target.clone());
                }
            }
        };
        for item in &mut self.items {
            match item {
                Item::Node(n) => {
                    if let Some(id) = n.subscription_id.as_ref().and_then(|id| aliases.get(id)) {
                        n.subscription_id = Some(id.clone());
                    }
                }
                Item::Rule(r) => resolve(r),
                Item::Group(g) => g.rules.iter_mut().for_each(&resolve),
                _ => {}
            }
        }
    }
    pub fn review(&self, local: &Bundle) -> Vec<ReviewRow> {
        let existing: BTreeMap<_, _> = local.items.iter().map(|i| (i.key(), i)).collect();
        let node_fps: BTreeMap<_, _> = local
            .items
            .iter()
            .filter_map(|i| {
                if let Item::Node(n) = i {
                    Some((n.fingerprint(), n))
                } else {
                    None
                }
            })
            .collect();
        let urls: BTreeMap<_, _> = local
            .items
            .iter()
            .filter_map(|i| {
                if let Item::Subscription(s) = i {
                    Some((&s.url, s))
                } else {
                    None
                }
            })
            .collect();
        let known_subs: BTreeSet<_> = urls.values().map(|s| &s.id).collect();
        self.items
            .iter()
            .map(|i| {
                let key = i.key();
                let old = existing.get(&key).copied();
                let same = match i {
                    Item::Node(n) => {
                        let other = if let Some(Item::Node(old)) = old {
                            Some(old)
                        } else {
                            node_fps.get(&n.fingerprint()).copied()
                        };
                        other.map(|other| {
                            let mut n = n.clone();
                            n.id = other.id.clone();
                            n.sort = other.sort;
                            if n.subscription_id
                                .as_ref()
                                .is_some_and(|id| !known_subs.contains(id))
                                && other.subscription_id.is_none()
                                && other.source == NodeSource::Import
                            {
                                n.subscription_id = None;
                                n.source = NodeSource::Import;
                            }
                            n == *other
                        })
                    }
                    Item::Subscription(s) => {
                        let other = if let Some(Item::Subscription(old)) = old {
                            Some(old)
                        } else {
                            urls.get(&s.url).copied()
                        };
                        other.map(|other| {
                            let mut s = s.clone();
                            s.id = other.id.clone();
                            s == *other
                        })
                    }
                    Item::Group(g) => old.map(|o| match o {
                        Item::Group(local) => {
                            let mut value = local.clone();
                            let ids: BTreeSet<_> = g.rules.iter().map(|r| &r.id).collect();
                            value.rules.retain(|r| ids.contains(&r.id));
                            value == *g
                        }
                        _ => false,
                    }),
                    _ => old.map(|o| o == i),
                };
                let difference = match same {
                    Some(true) => Difference::Identical,
                    Some(false) => Difference::Conflict,
                    None => Difference::New,
                };
                ReviewRow {
                    key,
                    category: i.parts().0,
                    name: i.parts().3.chars().take(128).collect(),
                    difference,
                }
            })
            .collect()
    }
    pub fn merge(
        &self,
        keys: &BTreeSet<String>,
        model: &mut AppModel,
        subs: &mut Vec<Subscription>,
        networks: &mut Vec<MeshProfile>,
        active_mesh: &BTreeSet<String>,
    ) -> Result<()> {
        let selected: Vec<_> = self
            .items
            .iter()
            .filter(|i| keys.contains(&i.key()))
            .collect();
        if selected.is_empty() || selected.len() != keys.len() {
            return Err(Error::config("请选择需要合并的项目"));
        }
        let mut adds = Vec::new();
        // Resolve subscription references even when an equivalent local subscription has a different ID.
        let sub_urls: BTreeMap<_, _> = self
            .items
            .iter()
            .filter_map(|i| {
                if let Item::Subscription(s) = i {
                    Some((&s.id, &s.url))
                } else {
                    None
                }
            })
            .collect();
        let resolves = |rule: &mut RouteRule| -> Result<()> {
            if let Some(id) = rule.action.group.as_ref()
                && !model.groups.iter().any(|g| &g.id == id && g.enabled)
            {
                let name = self.outbound_names.get(id);
                let mut groups = model
                    .groups
                    .iter()
                    .filter(|g| g.enabled && Some(&g.name) == name);
                let dest = groups
                    .next()
                    .filter(|_| groups.next().is_none())
                    .ok_or_else(|| {
                        Error::config(
                            "规则引用的出站组不存在或名称不唯一，请先在本机配置对应出站组",
                        )
                    })?;
                rule.action.group = Some(dest.id.clone());
            }
            Ok(())
        };
        let mut extra_rules = Vec::new();
        let mut changed_rules = false;
        for item in selected {
            match item {
                Item::Subscription(s) => {
                    let value = portable_sub(s);
                    let index = subs
                        .iter()
                        .position(|x| x.id == s.id)
                        .or_else(|| subs.iter().position(|x| x.url == s.url));
                    if let Some(index) = index {
                        let old = &mut subs[index];
                        let sort = old.sort;
                        let id = old.id.clone();
                        *old = value;
                        old.sort = sort;
                        old.id = id;
                    } else if !subs.iter().any(|x| x.url == s.url) {
                        let mut value = value;
                        value.sort = subs.len() as i64;
                        subs.push(value);
                    }
                }
                Item::Network(p) => {
                    if active_mesh.contains(&p.id) {
                        return Err(Error::config("请先断开要更新的组网，再合并配置"));
                    }
                    let mut value = portable_mesh(p);
                    if let Some(old) = networks.iter_mut().find(|x| x.id == p.id) {
                        if old.provider != p.provider {
                            return Err(Error::config("同一组网标识对应不同类型，无法合并"));
                        }
                        value.auto_reconnect = old.auto_reconnect;
                        value.easytier.device = old.easytier.device.clone();
                        value.hostname = old.hostname.clone();
                        value.ipv4 = old.ipv4.clone();
                        value.exit_node = old.exit_node.clone();
                        value.outbound_port = old.outbound_port;
                        value.outbound_secret = old.outbound_secret.clone();
                        value.advertised_subnets = old.advertised_subnets.clone();
                        value.advertise_exit_node = old.advertise_exit_node;
                        value.exit_routing = old.exit_routing;
                        value.port_forwards = old.port_forwards.clone();
                        value.service_forwards = old.service_forwards.clone();
                        value.validate()?;
                        *old = value;
                    } else {
                        value.hostname = super::super::local_device_name();
                        value.validate()?;
                        networks.push(value);
                    }
                }
                Item::Set(s) => {
                    upsert(&mut model.routing.rule_sets, s.clone(), |v| v.tag == s.tag);
                    changed_rules = true;
                }
                Item::Group(g) => {
                    let mut group = g.clone();
                    for r in &mut group.rules {
                        resolves(r)?;
                    }
                    // Import the group's metadata and incoming rules, retaining local-only children.
                    if let Some(old) = model.routing.rule_groups.iter().find(|v| v.id == g.id) {
                        let ids: BTreeSet<_> = group.rules.iter().map(|r| r.id.clone()).collect();
                        group
                            .rules
                            .extend(old.rules.iter().filter(|r| !ids.contains(&r.id)).cloned());
                    }
                    upsert(&mut model.routing.rule_groups, group, |v| v.id == g.id);
                    changed_rules = true;
                }
                Item::Rule(r) => {
                    let mut r = r.clone();
                    resolves(&mut r)?;
                    model.routing.preset = crate::model::RoutePreset::Custom;
                    if let Some(old) = model.routing.rules.iter_mut().find(|v| v.id == r.id) {
                        *old = r
                    } else {
                        extra_rules.push(r);
                    }
                    changed_rules = true;
                }
                Item::Node(n) => adds.push(n.clone()),
            }
        }
        model.routing.rules.splice(0..0, extra_rules);
        if changed_rules {
            let mut ids = BTreeSet::new();
            if model
                .routing
                .rules
                .iter()
                .chain(model.routing.rule_groups.iter().flat_map(|g| &g.rules))
                .any(|r| r.id.is_empty() || !ids.insert(&r.id))
            {
                return Err(Error::config("规则已位于其他分组，请先处理重复的规则标识"));
            }
            crate::route::transfer::export(model, &crate::route::transfer::ExportScope::All)?;
        }
        for mut n in adds {
            if let Some(id) = &n.subscription_id
                && !subs.iter().any(|s| &s.id == id)
            {
                n.subscription_id = sub_urls
                    .get(id)
                    .and_then(|url| subs.iter().find(|s| &s.url == *url))
                    .map(|s| s.id.clone());
                if n.subscription_id.is_none() {
                    n.source = NodeSource::Import;
                }
            }
            let index = model.nodes.iter().position(|v| v.id == n.id).or_else(|| {
                model
                    .nodes
                    .iter()
                    .position(|v| v.fingerprint() == n.fingerprint())
            });
            if let Some(index) = index {
                let old = &mut model.nodes[index];
                n.sort = old.sort;
                n.id = old.id.clone();
                *old = n;
            } else {
                model.add_nodes([n]);
            }
        }
        Ok(())
    }
}
fn upsert<T>(list: &mut Vec<T>, value: T, matches: impl Fn(&T) -> bool) {
    if let Some(old) = list.iter_mut().find(|v| matches(v)) {
        *old = value;
    } else {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupKind, Matchers, OutboundGroup, Protocol, RouteAction};
    #[test]
    fn syncing_easytier_preferences_does_not_publish_or_override_local_forwarding() {
        let mut source = MeshProfile {
            hostname: "xrose-demo-sender".into(),
            network_name: "fixture".into(),
            network_secret: crate::mesh::MeshSecret("fixture".into()),
            peers: vec!["tcp://192.0.2.1:11010".into()],
            ..Default::default()
        };
        let mut local = source.clone();
        local.hostname = "custom-receiver".into();
        let service = crate::mesh::ServiceForward {
            protocol: crate::mesh::ForwardProtocol::Tcp,
            port: 18080,
            target: "127.0.0.1:8080".parse().unwrap(),
        };
        source.service_forwards.push(service.clone());
        local.service_forwards.push(crate::mesh::ServiceForward {
            port: 18081,
            ..service
        });
        let local_services = local.service_forwards.clone();
        local.easytier.device.tcp_hole_punching = false;
        source.easytier.latency_first = true;
        source.easytier.device.system_forwarding = true;
        let mut model = AppModel::default();
        let bundle = Bundle::capture(&model, &[], &[source], &Category::ALL.into());
        let Item::Network(shared) = &bundle.items[0] else {
            panic!("expected a network")
        };
        assert!(shared.easytier.latency_first && !shared.easytier.device.system_forwarding);
        assert!(shared.service_forwards.is_empty());
        assert_eq!(shared.hostname, "xrose");
        let mut local = vec![local];
        bundle
            .merge(
                &[bundle.items[0].key()].into(),
                &mut model,
                &mut vec![],
                &mut local,
                &BTreeSet::new(),
            )
            .unwrap();
        assert!(local[0].easytier.latency_first);
        assert_eq!(local[0].service_forwards, local_services);
        assert_eq!(local[0].hostname, "custom-receiver");
        assert!(
            !local[0].easytier.device.tcp_hole_punching
                && !local[0].easytier.device.system_forwarding
        );
    }

    #[test]
    fn merges_different_device_ids_and_preserves_local_references_on_repeat() {
        let mut local = AppModel::default();
        let mut remote = AppModel::default();
        let mut a = OutboundGroup::new("fixture", GroupKind::Selector);
        let mut b = a.clone();
        a.id = "local-group".into();
        b.id = "remote-group".into();
        local.groups.push(a.clone());
        remote.groups.push(b.clone());
        let mut sub = Subscription::new("fixture", "https://example.test/list");
        let mut existing = sub.clone();
        existing.id = "local-sub".into();
        sub.id = "remote-sub".into();
        let mut node = Node::new("example", Protocol::Socks, "proxy.example.test", 1080);
        node.source = NodeSource::Subscription;
        node.subscription_id = Some(sub.id.clone());
        remote.add_nodes([node.clone()]);
        let rule = RouteRule::new("example", Matchers::default(), RouteAction::proxy_via(b.id));
        remote.routing.rules.push(rule.clone());
        let mut source = Bundle::capture(&remote, &[sub], &[], &Category::ALL.into());
        let mut subs = vec![existing.clone()];
        let baseline = Bundle::local(&local, &subs, &[]);
        source.resolve_local_ids(&baseline);
        let keys = source
            .review(&baseline)
            .into_iter()
            .filter(|r| r.difference == Difference::New)
            .map(|r| r.key)
            .collect();
        source
            .merge(&keys, &mut local, &mut subs, &mut vec![], &BTreeSet::new())
            .unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(
            local.node(&node.id).unwrap().subscription_id.as_ref(),
            Some(&existing.id)
        );
        assert_eq!(
            local
                .routing
                .rules
                .iter()
                .find(|r| r.id == rule.id)
                .unwrap()
                .action
                .group
                .as_ref(),
            Some(&a.id)
        );
        assert!(
            source
                .review(&Bundle::local(&local, &subs, &[]))
                .iter()
                .all(|r| r.difference == Difference::Identical)
        );
    }
}
