//! Proxy chain validation, DAG cycle detection, usability checks, and hop resolution.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};

use super::{AppModel, ChainHop, CoreKind, Node, OutboundGroup};

pub const MAX_CHAIN_HOPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Vertex<'a> {
    Group(&'a str),
    Node(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    Visiting,
    Done,
}

/// Summary entry for rendering a chain's ordered hops in UI and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHopSummary {
    pub label: String,
    pub is_group: bool,
    pub missing: bool,
    pub disabled: bool,
}

impl AppModel {
    pub(crate) fn chain_entry_allows(&self, hops: &[ChainHop], id: &str) -> bool {
        if hops
            .iter()
            .skip(1)
            .any(|hop| matches!(hop, ChainHop::Node(n) if n == id))
        {
            return false;
        }
        let Some(ChainHop::Node(next)) = hops.get(1) else {
            return false;
        };
        self.node(next).is_some_and(|next| {
            !next.uses_udp_transport()
                || self
                    .node(id)
                    .and_then(|n| self.chain_exit_node(n))
                    .is_some_and(Node::can_relay_udp)
        })
    }

    pub(crate) fn needs_filtered_chain_entry(&self, chain: &Node, group: &OutboundGroup) -> bool {
        matches!(chain.chain_hops.first(), Some(ChainHop::Group(id)) if id == &group.id)
            && group
                .members
                .iter()
                .any(|id| !self.chain_entry_allows(&chain.chain_hops, id))
    }

    /// A hop's transport is carried by its immediate predecessor. A TCP tunnel
    /// can itself carry a later hop's UDP datagrams.
    pub(crate) fn validate_chain_transports(&self, hops: &[ChainHop]) -> Result<()> {
        for (i, pair) in hops.windows(2).enumerate() {
            let ChainHop::Node(id) = &pair[1] else {
                continue;
            };
            let Some(next) = self.node(id).filter(|n| n.uses_udp_transport()) else {
                continue;
            };
            let incompatible = match &pair[0] {
                ChainHop::Node(id) => self.node(id).is_some_and(|n| !n.can_relay_udp()),
                ChainHop::Group(id) => self.group(id).is_some_and(|group| {
                    !group.members.iter().any(|mid| {
                        self.node(mid).is_some_and(|n| n.enabled)
                            && self.chain_entry_allows(hops, mid)
                    })
                }),
            };
            if incompatible {
                return Err(Error::config(format!(
                    "第 {} 跳 {} 需要 UDP/{}，请为第 {} 跳选择可转发 UDP 的节点",
                    i + 2,
                    next.protocol.display_name(),
                    next.port,
                    i + 1
                )));
            }
        }
        Ok(())
    }

    /// Returns true if the directed dependency graph between outbound groups and
    /// proxy chains contains any cycle.
    pub fn has_chain_cycle(&self) -> bool {
        let mut colors: HashMap<Vertex<'_>, Color> = HashMap::new();
        for node in &self.nodes {
            if node.is_chain()
                && !colors.contains_key(&Vertex::Node(&node.id))
                && self.dfs_cycle(Vertex::Node(&node.id), &mut colors)
            {
                return true;
            }
        }
        for group in &self.groups {
            if !colors.contains_key(&Vertex::Group(&group.id))
                && self.dfs_cycle(Vertex::Group(&group.id), &mut colors)
            {
                return true;
            }
        }
        false
    }

    /// Returns true if a cycle is reachable starting from `node_id`.
    pub fn node_in_cycle(&self, node_id: &str) -> bool {
        let mut colors: HashMap<Vertex<'_>, Color> = HashMap::new();
        self.dfs_cycle(Vertex::Node(node_id), &mut colors)
    }

    fn dfs_cycle<'a>(&'a self, v: Vertex<'a>, colors: &mut HashMap<Vertex<'a>, Color>) -> bool {
        colors.insert(v, Color::Visiting);
        match v {
            Vertex::Group(gid) => {
                if let Some(group) = self.group(gid) {
                    for member_id in &group.members {
                        let next = Vertex::Node(member_id.as_str());
                        match colors.get(&next) {
                            Some(Color::Visiting) => return true,
                            Some(Color::Done) => {}
                            None => {
                                if self.dfs_cycle(next, colors) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            Vertex::Node(nid) => {
                if let Some(node) = self.node(nid)
                    && node.is_chain()
                {
                    for hop in &node.chain_hops {
                        let next = match hop {
                            ChainHop::Node(id) => Vertex::Node(id.as_str()),
                            ChainHop::Group(gid) => Vertex::Group(gid.as_str()),
                        };
                        match colors.get(&next) {
                            Some(Color::Visiting) => return true,
                            Some(Color::Done) => {}
                            None => {
                                if self.dfs_cycle(next, colors) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        colors.insert(v, Color::Done);
        false
    }

    /// Validates that the entire group/chain graph is acyclic.
    pub fn validate_chain_graph(&self) -> Result<()> {
        if self.has_chain_cycle() {
            return Err(Error::config(
                "代理链与出站组之间存在循环引用，请调整跳点或所属出站组",
            ));
        }
        Ok(())
    }

    /// Validates a candidate proxy chain (new or edited) against the current model.
    pub fn validate_chain_draft(
        &self,
        editing_id: Option<&str>,
        name: &str,
        hops: &[ChainHop],
    ) -> Result<()> {
        if name.trim().is_empty() {
            return Err(Error::config("请输入代理链名称"));
        }
        if hops.len() < 2 {
            return Err(Error::config("代理链至少需要 2 个跳点"));
        }
        if hops.len() > MAX_CHAIN_HOPS {
            return Err(Error::config(format!(
                "代理链最多支持 {MAX_CHAIN_HOPS} 个跳点"
            )));
        }
        let mut seen_nodes = HashSet::new();
        for (index, hop) in hops.iter().enumerate() {
            match hop {
                ChainHop::Group(gid) => {
                    if index > 0 {
                        return Err(Error::config(
                            "仅第 1 跳（入口）支持出站组，后续跳点请选择具体节点",
                        ));
                    }
                    let group = self
                        .group(gid)
                        .ok_or_else(|| Error::config("所选前置出站组不存在，请重新选择"))?;
                    if !group.enabled {
                        return Err(Error::config(
                            "所选前置出站组已禁用，请先启用或选择其他出站组",
                        ));
                    }
                    if group.members.is_empty() {
                        return Err(Error::config("所选前置出站组没有成员节点"));
                    }
                }
                ChainHop::Node(id) => {
                    if editing_id == Some(id.as_str()) {
                        return Err(Error::config("代理链不能引用自身"));
                    }
                    let node = self
                        .node(id)
                        .ok_or_else(|| Error::config("所选跳点节点不存在，请重新选择"))?;
                    if node.is_chain() {
                        return Err(Error::config(
                            "代理链暂不支持嵌套其他代理链，请直接添加跳点",
                        ));
                    }
                    if index > 0 && node.is_mesh_gateway() {
                        return Err(Error::config(
                            "组网出口网关仅支持作为第 1 跳（入口），不能作为后续跳点",
                        ));
                    }
                    if !seen_nodes.insert(id.as_str()) {
                        return Err(Error::config("代理链中不能重复包含同一节点"));
                    }
                }
            }
        }
        self.validate_chain_transports(hops)?;
        let mut candidate = self.clone();
        if let Some(id) = editing_id
            && let Some(existing) = candidate.node_mut(id)
        {
            existing.name = name.trim().to_owned();
            existing.chain_hops = hops.to_vec();
        } else {
            let mut chain = Node::new_chain(name.trim(), hops.to_vec());
            if let Some(id) = editing_id {
                chain.id = id.to_owned();
            }
            candidate.nodes.push(chain);
        }
        candidate.validate_chain_graph()?;
        Ok(())
    }

    /// Model-aware usability check covering both single-hop nodes and proxy chains.
    pub fn node_usable_with(&self, node: &Node, core: CoreKind) -> bool {
        let mut visiting = HashSet::new();
        self.node_usable_inner(node, core, &mut visiting)
    }

    fn node_usable_inner<'a>(
        &'a self,
        node: &'a Node,
        core: CoreKind,
        visiting: &mut HashSet<Vertex<'a>>,
    ) -> bool {
        if !node.enabled {
            return false;
        }
        if !node.is_chain() {
            return node.usable_with(core);
        }
        if node.chain_hops.len() < 2
            || self.node_in_cycle(&node.id)
            || self.validate_chain_transports(&node.chain_hops).is_err()
        {
            return false;
        }
        if !visiting.insert(Vertex::Node(&node.id)) {
            return false;
        }
        let ok = node
            .chain_hops
            .iter()
            .enumerate()
            .all(|(idx, hop)| match hop {
                ChainHop::Node(id) => self.node(id).is_some_and(|n| {
                    !n.is_chain() && (idx == 0 || !n.is_mesh_gateway()) && n.usable_with(core)
                }),
                ChainHop::Group(gid) => {
                    idx == 0 && visiting.insert(Vertex::Group(gid.as_str())) && {
                        let group_ok = self.group(gid).is_some_and(|group| {
                            group.enabled
                                && group.members.iter().any(|mid| {
                                    self.chain_entry_allows(&node.chain_hops, mid)
                                        && self.node(mid).is_some_and(|member| {
                                            self.node_usable_inner(member, core, visiting)
                                        })
                                })
                        });
                        visiting.remove(&Vertex::Group(gid.as_str()));
                        group_ok
                    }
                }
            });
        visiting.remove(&Vertex::Node(&node.id));
        ok
    }

    /// Returns the landing exit node of a proxy chain (or the node itself for single-hop nodes).
    pub fn chain_exit_node<'a>(&'a self, node: &'a Node) -> Option<&'a Node> {
        if !node.is_chain() {
            return Some(node);
        }
        match node.chain_hops.last()? {
            ChainHop::Node(id) => self.node(id).filter(|n| !n.is_chain()),
            ChainHop::Group(_) => None,
        }
    }

    /// All proxy chains referencing `node_id` as one of their hops.
    pub fn node_chain_references(&self, node_id: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| {
                n.is_chain()
                    && n.id != node_id
                    && n.chain_hops
                        .iter()
                        .any(|hop| matches!(hop, ChainHop::Node(id) if id == node_id))
            })
            .collect()
    }

    /// All proxy chains referencing `group_id` as their entry hop.
    pub fn group_chain_references(&self, group_id: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| {
                n.is_chain()
                    && n.chain_hops
                        .iter()
                        .any(|hop| matches!(hop, ChainHop::Group(id) if id == group_id))
            })
            .collect()
    }

    /// Human-readable hop summaries for UI display and search matching.
    pub fn chain_hop_summaries(&self, node: &Node) -> Vec<ChainHopSummary> {
        node.chain_hops
            .iter()
            .map(|hop| match hop {
                ChainHop::Node(id) => match self.node(id) {
                    Some(target) => ChainHopSummary {
                        label: target.name.clone(),
                        is_group: false,
                        missing: false,
                        disabled: !target.enabled,
                    },
                    None => ChainHopSummary {
                        label: "缺失节点".into(),
                        is_group: false,
                        missing: true,
                        disabled: true,
                    },
                },
                ChainHop::Group(gid) => match self.group(gid) {
                    Some(group) => ChainHopSummary {
                        label: group.name.clone(),
                        is_group: true,
                        missing: false,
                        disabled: !group.enabled,
                    },
                    None => ChainHopSummary {
                        label: "缺失出站组".into(),
                        is_group: true,
                        missing: true,
                        disabled: true,
                    },
                },
            })
            .collect()
    }

    /// Resolves a node (single-hop or proxy chain) into an ordered list of
    /// concrete single-hop nodes `[entry, ..., exit]` for isolated probing.
    pub fn resolve_probe_hops(&self, node: &Node, core: CoreKind) -> Result<Vec<Node>> {
        let mut visiting = HashSet::new();
        self.resolve_probe_hops_inner(node, core, &mut visiting)
    }

    fn resolve_probe_hops_inner<'a>(
        &'a self,
        node: &'a Node,
        core: CoreKind,
        visiting: &mut HashSet<Vertex<'a>>,
    ) -> Result<Vec<Node>> {
        if !node.enabled {
            return Err(Error::config("节点已禁用，请先启用节点"));
        }
        if !node.is_chain() {
            if !node.usable_with(core) {
                return Err(Error::config("该节点未启用或当前内核不支持该协议"));
            }
            return Ok(vec![node.clone()]);
        }
        if node.chain_hops.len() < 2 {
            return Err(Error::config("代理链至少需要 2 个跳点"));
        }
        if !visiting.insert(Vertex::Node(&node.id)) {
            return Err(Error::config("代理链存在循环引用，无法测速"));
        }
        self.validate_chain_transports(&node.chain_hops)?;
        let mut resolved = Vec::new();
        for (idx, hop) in node.chain_hops.iter().enumerate() {
            match hop {
                ChainHop::Node(id) => {
                    let hop_node = self
                        .node(id)
                        .ok_or_else(|| Error::config("代理链引用的跳点节点不存在"))?;
                    if hop_node.is_chain() {
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config("代理链暂不支持嵌套其他代理链"));
                    }
                    if idx > 0 && hop_node.is_mesh_gateway() {
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config(
                            "组网出口网关仅支持作为第 1 跳（入口），请编辑代理链",
                        ));
                    }
                    if !hop_node.usable_with(core) {
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config(format!(
                            "代理链跳点“{}”已禁用或当前内核不支持",
                            hop_node.name
                        )));
                    }
                    resolved.push(hop_node.clone());
                }
                ChainHop::Group(gid) => {
                    if idx > 0 {
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config(
                            "仅第 1 跳（入口）支持出站组，请检查代理链配置",
                        ));
                    }
                    if !visiting.insert(Vertex::Group(gid.as_str())) {
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config("代理链与出站组存在循环引用，无法测速"));
                    }
                    let group = self
                        .group(gid)
                        .filter(|g| g.enabled)
                        .ok_or_else(|| Error::config("代理链前置出站组不存在或已禁用"))?;
                    let active = group
                        .selected
                        .as_deref()
                        .filter(|id| {
                            group.members.iter().any(|mid| mid.as_str() == *id)
                                && self.chain_entry_allows(&node.chain_hops, id)
                        })
                        .and_then(|id| self.node(id))
                        .filter(|n| self.node_usable_with(n, core))
                        .or_else(|| {
                            group
                                .members
                                .iter()
                                .filter(|id| self.chain_entry_allows(&node.chain_hops, id))
                                .filter_map(|id| self.node(id))
                                .find(|n| self.node_usable_with(n, core))
                        });
                    let Some(active) = active else {
                        let any_usable = group
                            .members
                            .iter()
                            .filter_map(|id| self.node(id))
                            .any(|n| self.node_usable_with(n, core));
                        visiting.remove(&Vertex::Group(gid.as_str()));
                        visiting.remove(&Vertex::Node(&node.id));
                        return Err(Error::config(if any_usable {
                            format!(
                                "代理链前置出站组“{}”中没有可连接下一跳的可用成员",
                                group.name
                            )
                        } else {
                            format!("代理链前置出站组“{}”没有当前内核可用的节点", group.name)
                        }));
                    };
                    let prefix = self.resolve_probe_hops_inner(active, core, visiting);
                    visiting.remove(&Vertex::Group(gid.as_str()));
                    let prefix = prefix?;
                    resolved.extend(prefix);
                }
            }
        }
        visiting.remove(&Vertex::Node(&node.id));
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupKind, OutboundGroup, Protocol};

    fn sample_node(name: &str, protocol: Protocol, server: &str) -> Node {
        let mut n = Node::new(name, protocol, server, 443);
        n.credentials.uuid = Some("00000000-0000-4000-8000-000000000001".into());
        n.credentials.password = Some("secret".into());
        n
    }

    #[test]
    fn validates_basic_and_group_entry_chains_and_blocks_invalid_hops() {
        let mut model = AppModel::new("fixture");
        let hk = sample_node("HK-1", Protocol::Vless, "203.0.113.10");
        let us = sample_node("US-Home", Protocol::Socks, "203.0.113.20");
        model.add_nodes([hk.clone(), us.clone()]);

        let mut relay = OutboundGroup::new("HK Relay", GroupKind::Selector);
        relay.members = vec![hk.id.clone()];
        relay.selected = Some(hk.id.clone());
        model.groups.push(relay.clone());

        assert!(
            model
                .validate_chain_draft(
                    None,
                    "HK->US",
                    &[ChainHop::Node(hk.id.clone()), ChainHop::Node(us.id.clone())],
                )
                .is_ok()
        );
        assert!(
            model
                .validate_chain_draft(
                    None,
                    "Relay->US",
                    &[
                        ChainHop::Group(relay.id.clone()),
                        ChainHop::Node(us.id.clone()),
                    ],
                )
                .is_ok()
        );
        // Group is only allowed as hop 0.
        assert!(
            model
                .validate_chain_draft(
                    None,
                    "Bad",
                    &[
                        ChainHop::Node(us.id.clone()),
                        ChainHop::Group(relay.id.clone()),
                    ],
                )
                .is_err()
        );
        // Duplicate node in chain is rejected.
        assert!(
            model
                .validate_chain_draft(
                    None,
                    "Dup",
                    &[ChainHop::Node(hk.id.clone()), ChainHop::Node(hk.id.clone())],
                )
                .is_err()
        );
        // Fewer than 2 hops rejected.
        assert!(
            model
                .validate_chain_draft(None, "One", &[ChainHop::Node(hk.id.clone())])
                .is_err()
        );
    }

    #[test]
    fn detects_direct_and_indirect_group_chain_cycles() {
        let mut model = AppModel::new("fixture");
        let a = sample_node("A", Protocol::Vless, "203.0.113.10");
        let b = sample_node("B", Protocol::Vless, "203.0.113.11");
        model.add_nodes([a.clone(), b.clone()]);

        let mut g1 = OutboundGroup::new("G1", GroupKind::Selector);
        let mut g2 = OutboundGroup::new("G2", GroupKind::Selector);

        // Chain 1: G2 -> A, placed in G1.
        let c1 = Node::new_chain(
            "C1",
            vec![ChainHop::Group(g2.id.clone()), ChainHop::Node(a.id.clone())],
        );
        // Chain 2: G1 -> B, placed in G2 => indirect cycle C1 -> G2 -> C2 -> G1 -> C1.
        let c2 = Node::new_chain(
            "C2",
            vec![ChainHop::Group(g1.id.clone()), ChainHop::Node(b.id.clone())],
        );
        g1.members = vec![a.id.clone(), c1.id.clone()];
        g2.members = vec![b.id.clone()];
        model.groups.push(g1.clone());
        model.groups.push(g2.clone());
        model.nodes.push(c1.clone());

        assert!(!model.has_chain_cycle());
        // Adding c2 to g2 creates an indirect cycle.
        model.nodes.push(c2.clone());
        model.group_mut(&g2.id).unwrap().members.push(c2.id.clone());
        assert!(model.has_chain_cycle());
        assert!(model.node_in_cycle(&c1.id));
        assert!(model.node_in_cycle(&c2.id));
        assert!(!model.node_usable_with(&c1, CoreKind::SingBox));
    }

    #[test]
    fn node_usable_with_checks_hop_core_support_and_resolves_probe_hops() {
        let mut model = AppModel::new("fixture");
        let hy2 = sample_node("Hy2", Protocol::Hysteria2, "203.0.113.10");
        let vless = sample_node("VLESS", Protocol::Vless, "203.0.113.11");
        let socks = sample_node("US-SOCKS", Protocol::Socks, "203.0.113.12");
        model.add_nodes([hy2.clone(), vless.clone(), socks.clone()]);

        let mut relay = OutboundGroup::new("Relay", GroupKind::Selector);
        relay.members = vec![hy2.id.clone(), vless.id.clone()];
        relay.selected = Some(hy2.id.clone());
        model.groups.push(relay.clone());

        let chain_hy2 = Node::new_chain(
            "Hy2->SOCKS",
            vec![
                ChainHop::Node(hy2.id.clone()),
                ChainHop::Node(socks.id.clone()),
            ],
        );
        let chain_group = Node::new_chain(
            "Relay->SOCKS",
            vec![
                ChainHop::Group(relay.id.clone()),
                ChainHop::Node(socks.id.clone()),
            ],
        );
        model.nodes.push(chain_hy2.clone());
        model.nodes.push(chain_group.clone());

        assert!(model.node_usable_with(&chain_hy2, CoreKind::SingBox));
        assert!(!model.node_usable_with(&chain_hy2, CoreKind::Xray));

        // Group hop falls back to VLESS under Xray because Hy2 isn't supported there.
        assert!(model.node_usable_with(&chain_group, CoreKind::Xray));
        let hops_xray = model
            .resolve_probe_hops(&chain_group, CoreKind::Xray)
            .unwrap();
        assert_eq!(hops_xray.len(), 2);
        assert_eq!(hops_xray[0].id, vless.id);
        assert_eq!(hops_xray[1].id, socks.id);

        let hops_sb = model
            .resolve_probe_hops(&chain_group, CoreKind::SingBox)
            .unwrap();
        assert_eq!(hops_sb[0].id, hy2.id);
        assert_eq!(hops_sb[1].id, socks.id);
    }

    #[test]
    fn checks_udp_at_each_hop_and_filters_incompatible_entry_group_members() {
        let mut model = AppModel::new("fixture");
        let http = Node::new("HTTP", Protocol::Http, "proxy.test", 8080);
        let vision = Node::new("Vision", Protocol::Vless, "entry.test", 443);
        let tuic = Node::new("TUIC", Protocol::Tuic, "exit.test", 8443);
        model.add_nodes([http.clone(), vision.clone(), tuic.clone()]);
        let via = |node: &Node| ChainHop::Node(node.id.clone());
        let forward = [via(&vision), via(&tuic)];
        let reverse = [via(&tuic), via(&vision)];
        let core = CoreKind::SingBox;
        for hops in [
            &forward[..],
            &reverse,
            &[via(&http), via(&vision), via(&tuic)],
        ] {
            assert!(model.validate_chain_draft(None, "chain", hops).is_ok());
        }
        let error = model
            .validate_chain_draft(None, "chain", &[via(&http), via(&tuic)])
            .unwrap_err();
        assert!(error.to_string().contains("第 2 跳 TUIC 需要 UDP/8443"));

        let mut group = OutboundGroup::new("Entry", GroupKind::Selector);
        group.members = vec![http.id.clone(), tuic.id.clone(), vision.id.clone()];
        group.selected = Some(http.id.clone());
        let grouped = Node::new_chain("chain", vec![ChainHop::Group(group.id.clone()), via(&tuic)]);
        model.groups.push(group);
        let resolved = model.resolve_probe_hops(&grouped, core).unwrap();
        let ids: Vec<_> = resolved.iter().map(|n| &n.id).collect();
        assert_eq!(ids, [&vision.id, &tuic.id]);
        model.node_mut(&vision.id).unwrap().udp = false;
        for chain in [grouped, Node::new_chain("saved", forward.to_vec())] {
            assert!(!model.node_usable_with(&chain, core));
            assert!(
                model
                    .resolve_probe_hops(&chain, core)
                    .unwrap_err()
                    .to_string()
                    .contains("UDP/8443")
            );
        }
        assert!(
            model
                .validate_chain_draft(None, "reverse", &reverse)
                .is_ok()
        );
    }

    #[test]
    fn group_entry_skips_duplicate_trailing_hop_and_blocks_mesh_gateway_trailing_hop() {
        let mut model = AppModel::new("fixture");
        let a = sample_node("Edge-A", Protocol::Vless, "203.0.113.10");
        let b = sample_node("Edge-B", Protocol::Vless, "203.0.113.11");
        let mut mesh_gw = Node::new("Office Mesh", Protocol::Socks, "127.0.0.1", 19081);
        mesh_gw.id = "mesh-exit-office".into();
        mesh_gw.tags = vec!["mesh-exit".into()];
        model.add_nodes([a.clone(), b.clone(), mesh_gw.clone()]);

        // Mesh gateway is allowed as entry hop 0, but rejected as a trailing hop.
        assert!(
            model
                .validate_chain_draft(
                    None,
                    "Mesh->A",
                    &[
                        ChainHop::Node(mesh_gw.id.clone()),
                        ChainHop::Node(a.id.clone()),
                    ],
                )
                .is_ok()
        );
        assert!(
            model
                .validate_chain_draft(
                    None,
                    "A->Mesh",
                    &[
                        ChainHop::Node(a.id.clone()),
                        ChainHop::Node(mesh_gw.id.clone()),
                    ],
                )
                .is_err()
        );

        // Group currently selects A, while trailing hop is also A: probe falls back to B.
        let mut group = OutboundGroup::new("Proxy", GroupKind::Selector);
        group.members = vec![a.id.clone(), b.id.clone()];
        group.selected = Some(a.id.clone());
        model.groups.push(group.clone());

        let chain = Node::new_chain(
            "Group->A",
            vec![
                ChainHop::Group(group.id.clone()),
                ChainHop::Node(a.id.clone()),
            ],
        );
        model.nodes.push(chain.clone());

        let resolved = model.resolve_probe_hops(&chain, CoreKind::SingBox).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].id, b.id);
        assert_eq!(resolved[1].id, a.id);

        // When the group ONLY contains A, chain becomes unusable and probe errors clearly.
        model.group_mut(&group.id).unwrap().members = vec![a.id.clone()];
        assert!(!model.node_usable_with(&chain, CoreKind::SingBox));
        assert!(model.resolve_probe_hops(&chain, CoreKind::SingBox).is_err());
    }
}
