//! Create and edit composite proxy chains with DAG validation and safe group attachment.

use super::*;
use crate::model::{ChainHop, Node};

impl AppService {
    /// Creates or updates a proxy chain node.
    ///
    /// On creation, attaches the chain to default/built-in groups unless doing so
    /// would introduce a dependency cycle (for example when the chain's first hop
    /// itself references that group).
    pub fn save_proxy_chain(
        &self,
        editing_id: Option<&str>,
        name: String,
        hops: Vec<ChainHop>,
    ) -> Result<Node> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        let name = name.trim().to_owned();
        let mut saved_node: Option<Node> = None;
        let mut skipped_default_due_to_cycle = false;
        self.commit_model_change(|model| {
            model.validate_chain_draft(editing_id, &name, &hops)?;
            if let Some(id) = editing_id {
                let existing = model
                    .node_mut(id)
                    .filter(|n| n.is_chain())
                    .ok_or_else(|| Error::config("代理链不存在，请刷新后重试"))?;
                if existing.name == name && existing.chain_hops == hops {
                    saved_node = Some(existing.clone());
                    return Ok(false);
                }
                existing.name = name.clone();
                existing.chain_hops = hops.clone();
                saved_node = Some(existing.clone());
            } else {
                let mut chain = Node::new_chain(name.clone(), hops.clone());
                chain.sort = model.nodes.len() as i64;
                let chain_id = chain.id.clone();
                model.nodes.push(chain.clone());

                let default_id = model.default_group_id();
                let group_ids: Vec<String> = model
                    .groups
                    .iter()
                    .filter(|g| {
                        matches!(g.id.as_str(), "proxy" | "auto")
                            || default_id.as_deref() == Some(g.id.as_str())
                    })
                    .map(|g| g.id.clone())
                    .collect();
                for gid in group_ids {
                    if let Some(group) = model.group_mut(&gid)
                        && !group.members.contains(&chain_id)
                    {
                        group.members.push(chain_id.clone());
                        if model.has_chain_cycle() {
                            if let Some(group) = model.group_mut(&gid) {
                                group.members.retain(|m| m != &chain_id);
                            }
                            if default_id.as_deref() == Some(gid.as_str()) {
                                skipped_default_due_to_cycle = true;
                            }
                        } else if let Some(group) = model.group_mut(&gid) {
                            group.repair();
                        }
                    }
                }
                saved_node = Some(chain);
            }
            model.updated_at = now_unix();
            Ok(true)
        })?;
        if skipped_default_due_to_cycle {
            self.toast(
                ToastLevel::Info,
                "代理链已创建；因首跳引用了默认出站组，未自动加入默认组以避免循环，请在其他出站组中使用",
            );
        }
        saved_node.ok_or_else(|| Error::other("保存代理链失败"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupKind, OutboundGroup, Protocol};

    fn make_service() -> (AppService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::AppPaths::from_root(dir.path());
        (AppService::with_paths(paths).unwrap(), dir)
    }

    fn vless(name: &str, server: &str) -> Node {
        let mut node = Node::new(name, Protocol::Vless, server, 443);
        node.credentials.uuid = Some("00000000-0000-4000-8000-000000000001".into());
        node
    }

    #[test]
    fn saves_edits_and_protects_chain_dependencies() {
        let (service, _dir) = make_service();
        let hk = vless("HK-Entry", "203.0.113.10");
        let us = vless("US-Exit", "203.0.113.20");
        {
            let mut model = service.inner.model.write();
            model.add_nodes([hk.clone(), us.clone()]);
        }
        service.persist_model().unwrap();

        let chain = service
            .save_proxy_chain(
                None,
                "美国家宽链".into(),
                vec![ChainHop::Node(hk.id.clone()), ChainHop::Node(us.id.clone())],
            )
            .unwrap();
        assert!(chain.is_chain());
        assert_eq!(chain.chain_hops.len(), 2);

        let model = service.model();
        assert!(model.node(&chain.id).is_some());
        assert!(model.group("proxy").unwrap().members.contains(&chain.id));

        // Editing name and swapping hops succeeds.
        let updated = service
            .save_proxy_chain(
                Some(&chain.id),
                "反向链".into(),
                vec![ChainHop::Node(us.id.clone()), ChainHop::Node(hk.id.clone())],
            )
            .unwrap();
        assert_eq!(updated.id, chain.id);
        assert_eq!(updated.name, "反向链");
        assert_eq!(updated.chain_hops[0], ChainHop::Node(us.id.clone()));

        // Creating a chain whose entry hop is the default group succeeds without cycling it into default group.
        let group_chain = service
            .save_proxy_chain(
                None,
                "默认中转链".into(),
                vec![
                    ChainHop::Group("proxy".into()),
                    ChainHop::Node(us.id.clone()),
                ],
            )
            .unwrap();
        let model = service.model();
        assert!(!model.has_chain_cycle());
        assert!(
            !model
                .group("proxy")
                .unwrap()
                .members
                .contains(&group_chain.id)
        );

        // Trying to manually add group_chain to "proxy" is rejected by cycle check.
        let mut edited_proxy = model.group("proxy").unwrap().clone();
        edited_proxy.members.push(group_chain.id.clone());
        assert!(
            service
                .save_outbound_group(Some(model.group("proxy").unwrap()), edited_proxy)
                .is_err()
        );

        // A custom group referenced by a chain cannot be deleted.
        let mut relay = OutboundGroup::new("HK Relay", GroupKind::Selector);
        relay.members = vec![hk.id.clone()];
        service.save_outbound_group(None, relay.clone()).unwrap();
        service
            .save_proxy_chain(
                None,
                "Relay->US".into(),
                vec![
                    ChainHop::Group(relay.id.clone()),
                    ChainHop::Node(us.id.clone()),
                ],
            )
            .unwrap();
        assert!(service.delete_outbound_group(&relay.id).is_err());
    }
}
