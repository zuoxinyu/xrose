//! Atomic outbound-group edits and node membership changes.

use super::*;
use crate::model::OutboundGroup;

fn same_configuration(left: &OutboundGroup, right: &OutboundGroup) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.selected = None;
    right.selected = None;
    left == right
}

fn repair_selection(model: &AppModel, group: &mut OutboundGroup, core: CoreKind) {
    let usable = |id: &String| model.node(id).is_some_and(|n| n.usable_with(core));
    if !group
        .selected
        .as_ref()
        .is_some_and(|id| group.members.contains(id) && usable(id))
    {
        group.selected = group
            .members
            .iter()
            .find(|id| usable(id))
            .cloned()
            .or_else(|| group.members.first().cloned());
    }
}

impl AppService {
    /// The original configuration prevents a stale form from overwriting concurrent edits.
    pub fn save_outbound_group(
        &self,
        original: Option<&OutboundGroup>,
        mut group: OutboundGroup,
    ) -> Result<()> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        group.name = group.name.trim().to_owned();
        if group.name.is_empty() {
            return Err(Error::config("请输入出站组名称"));
        }
        if group.members.is_empty() {
            return Err(Error::config("请至少选择一个成员节点"));
        }
        if !(1..=10).contains(&group.switch_policy.consecutive_failures) || group.interval_secs < 30
        {
            return Err(Error::config(
                "连续失败次数须为 1 至 10 次，检查间隔至少为 30 秒",
            ));
        }
        let core = self.settings().core;
        self.commit_model_change(|model| {
            if model
                .groups
                .iter()
                .any(|g| g.id != group.id && g.name == group.name)
            {
                return Err(Error::config("出站组名称已存在，请使用其他名称"));
            }
            let mut seen = BTreeSet::new();
            group.members.retain(|id| seen.insert(id.clone()));
            if group.members.iter().any(|id| model.node(id).is_none()) {
                return Err(Error::config("成员节点已变化，请重新打开出站组编辑"));
            }
            let current = model.group(&group.id);
            let selection_changed = original.is_none_or(|old| old.selected != group.selected);
            if current.is_some_and(|current| {
                same_configuration(current, &group)
                    && (!selection_changed || current.selected == group.selected)
            }) {
                return Ok(false);
            }
            match (original, current) {
                (None, None) => {}
                (Some(original), Some(current))
                    if original.id == group.id && same_configuration(original, current) =>
                {
                    if !selection_changed {
                        group.selected = current.selected.clone();
                    }
                }
                _ => return Err(Error::config("出站组已变化，请重新打开后编辑")),
            }
            repair_selection(model, &mut group, core);
            if let Some(current) = model.group_mut(&group.id) {
                *current = group;
            } else {
                model.groups.push(group);
            }
            model.updated_at = now_unix();
            Ok(true)
        })?;
        Ok(())
    }

    pub fn delete_outbound_group(&self, id: &str) -> Result<()> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        self.commit_model_change(|model| {
            if model.group(id).is_none() {
                return Ok(false);
            }
            if let Some(reason) = model.outbound_group_deletion_reason(id) {
                return Err(Error::config(reason));
            }
            model.groups.retain(|group| group.id != id);
            model.updated_at = now_unix();
            Ok(true)
        })?;
        Ok(())
    }

    /// Changes one node's memberships while preserving other nodes and group settings.
    pub fn set_node_outbound_groups(
        &self,
        node_id: &str,
        original: &BTreeSet<String>,
        selected: &BTreeSet<String>,
    ) -> Result<()> {
        if self.is_busy("core") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        let core = self.settings().core;
        self.commit_model_change(|model| {
            if model.node(node_id).is_none() {
                return Err(Error::config("节点已移除，请刷新后重试"));
            }
            if selected.iter().any(|id| model.group(id).is_none()) {
                return Err(Error::config("出站组已变化，请重新打开所属出站组"));
            }
            let current: BTreeSet<_> = model
                .groups
                .iter()
                .filter(|g| g.members.iter().any(|id| id == node_id))
                .map(|g| g.id.clone())
                .collect();
            if &current == selected {
                return Ok(false);
            }
            if &current != original {
                return Err(Error::config("节点的所属出站组已变化，请重新打开后编辑"));
            }
            for index in 0..model.groups.len() {
                let mut group = model.groups[index].clone();
                if current.contains(&group.id) == selected.contains(&group.id) {
                    continue;
                }
                if selected.contains(&group.id) {
                    group.members.push(node_id.to_owned());
                } else {
                    group.members.retain(|id| id != node_id);
                    if group.members.is_empty() {
                        return Err(Error::config(format!(
                            "出站组“{}”须保留至少一个节点，请先添加其他成员或删除此组",
                            group.name
                        )));
                    }
                }
                repair_selection(model, &mut group, core);
                model.groups[index] = group;
            }
            model.updated_at = now_unix();
            Ok(true)
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{CompileContext, adapter_for};
    use crate::model::{
        FallbackScope, GroupKind, Matchers, Protocol, RouteAction, RouteRuleGroup, SwitchMode,
    };

    fn fixture() -> (AppService, tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let first = Node::new("Alpha", Protocol::Http, "alpha.example", 8080);
        let second = Node::new("Beta", Protocol::Http, "beta.example", 8080);
        let ids = (first.id.clone(), second.id.clone());
        service
            .commit_model_change(|model| {
                model.add_nodes([first, second]);
                Ok(true)
            })
            .unwrap();
        (service, dir, ids.0, ids.1)
    }

    fn group(name: &str, members: &[String]) -> OutboundGroup {
        let mut group = OutboundGroup::new(name, GroupKind::Selector);
        group.members = members.to_vec();
        group.switch_policy.mode = SwitchMode::Off;
        group
    }

    #[test]
    fn group_edits_persist_and_preserve_live_selection() {
        let (service, dir, first, second) = fixture();
        let initial = group(
            " Regional ",
            &[first.clone(), second.clone(), first.clone()],
        );
        service.save_outbound_group(None, initial.clone()).unwrap();
        let original = service.model().group(&initial.id).unwrap().clone();
        assert_eq!(original.name, "Regional");
        assert_eq!(original.members, [first.clone(), second.clone()]);
        service
            .commit_model_change(|model| {
                model.group_mut(&original.id).unwrap().selected = Some(second.clone());
                Ok(true)
            })
            .unwrap();
        let mut edited = original.clone();
        edited.name = "Preferred".into();
        edited.switch_policy.mode = SwitchMode::Throughput;
        edited.switch_policy.fallback = FallbackScope::SameSubscription;
        edited.interval_secs = 60;
        service
            .save_outbound_group(Some(&original), edited.clone())
            .unwrap();
        assert_eq!(
            service
                .model()
                .group(&original.id)
                .unwrap()
                .selected
                .as_ref(),
            Some(&second)
        );
        service
            .save_outbound_group(Some(&original), edited)
            .unwrap();
        let current = service.model().group(&original.id).unwrap().clone();
        let mut removed = current.clone();
        removed.members = vec![first.clone()];
        service
            .save_outbound_group(Some(&current), removed)
            .unwrap();
        assert_eq!(
            service
                .model()
                .group(&original.id)
                .unwrap()
                .selected
                .as_ref(),
            Some(&first)
        );
        let reopened = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        assert_eq!(service.model().groups, reopened.model().groups);
        let original = service.model().group(&initial.id).unwrap().clone();
        let mut preferred = original.clone();
        preferred.members.push(second.clone());
        preferred.selected = Some(second.clone());
        service
            .save_outbound_group(Some(&original), preferred)
            .unwrap();
        assert_eq!(
            service
                .model()
                .group(&initial.id)
                .unwrap()
                .selected
                .as_ref(),
            Some(&second)
        );
        service.delete_outbound_group(&original.id).unwrap();
        service.delete_outbound_group(&original.id).unwrap();
        assert!(service.model().group(&original.id).is_none());
        assert_eq!(service.model().nodes.len(), 2);
    }

    #[test]
    fn group_edits_reject_stale_forms_and_invalid_members() {
        let (service, _dir, first, _) = fixture();
        let initial = group("Regional", std::slice::from_ref(&first));
        service.save_outbound_group(None, initial.clone()).unwrap();
        let original = service.model().group(&initial.id).unwrap().clone();
        let mut changed = original.clone();
        changed.name = "Updated".into();
        service
            .save_outbound_group(Some(&original), changed)
            .unwrap();
        let before = service.model();
        let mut stale = original.clone();
        stale.switch_policy.mode = SwitchMode::Latency;
        assert!(service.save_outbound_group(Some(&original), stale).is_err());
        assert!(
            service
                .save_outbound_group(None, group("Updated", std::slice::from_ref(&first)))
                .is_err()
        );
        assert!(
            service
                .save_outbound_group(None, group("Empty", &[]))
                .is_err()
        );
        assert!(
            service
                .save_outbound_group(None, group(" ", std::slice::from_ref(&first)))
                .is_err()
        );
        assert!(
            service
                .save_outbound_group(None, group("Missing", &["missing".into()]))
                .is_err()
        );
        let mut invalid = group("Invalid", &[first]);
        invalid.interval_secs = 1;
        assert!(service.save_outbound_group(None, invalid).is_err());
        assert_eq!(service.model(), before);
        service.set_busy("core", true);
        assert!(service.delete_outbound_group(&original.id).is_err());
        assert_eq!(service.model(), before);
    }

    #[test]
    fn deletion_checks_default_disabled_rules_final_action_and_dns() {
        let (service, _dir, first, _) = fixture();
        assert!(service.delete_outbound_group("proxy").is_err());
        let group = group("Regional", &[first]);
        service.save_outbound_group(None, group.clone()).unwrap();
        let mut rule = RouteRule::new(
            "Regional route",
            Matchers::default(),
            RouteAction::proxy_via(&group.id),
        );
        rule.enabled = false;
        service
            .commit_model_change(|model| {
                model.routing.rule_groups.push(RouteRuleGroup {
                    id: "sample-rules".into(),
                    name: "Rules".into(),
                    enabled: false,
                    rules: vec![rule],
                });
                Ok(true)
            })
            .unwrap();
        assert!(
            service
                .delete_outbound_group(&group.id)
                .unwrap_err()
                .to_string()
                .contains("路由")
        );
        service
            .commit_model_change(|model| {
                model.routing.rule_groups.clear();
                model.routing.final_action = RouteAction::proxy_via(&group.id);
                Ok(true)
            })
            .unwrap();
        assert!(service.delete_outbound_group(&group.id).is_err());
        service
            .commit_model_change(|model| {
                model.routing.final_action = RouteAction::proxy();
                model.dns.servers[0].detour = Some(group.tag());
                Ok(true)
            })
            .unwrap();
        assert!(
            service
                .delete_outbound_group(&group.id)
                .unwrap_err()
                .to_string()
                .contains("DNS")
        );
        service
            .commit_model_change(|model| {
                model.dns.servers[0].detour = Some(group.id.clone());
                Ok(true)
            })
            .unwrap();
        assert!(service.delete_outbound_group(&group.id).is_err());
        service
            .commit_model_change(|model| {
                model.dns.servers[0].detour = Some("direct".into());
                Ok(true)
            })
            .unwrap();
        service.set_default_outbound_group(&group.id).unwrap();
        assert!(service.delete_outbound_group(&group.id).is_err());
        service.set_default_outbound_group("proxy").unwrap();
        service.delete_outbound_group(&group.id).unwrap();
    }

    #[test]
    fn node_membership_edits_are_atomic_repair_selection_and_detect_conflicts() {
        let (service, _dir, first, second) = fixture();
        let group = group("Regional", std::slice::from_ref(&first));
        service.save_outbound_group(None, group.clone()).unwrap();
        let original: BTreeSet<String> = ["proxy".into(), group.id.clone()].into();
        let before = service.model();
        assert!(
            service
                .set_node_outbound_groups(&first, &original, &BTreeSet::new())
                .is_err()
        );
        assert_eq!(service.model(), before);
        service
            .set_node_outbound_groups(&second, &["proxy".into()].into(), &original)
            .unwrap();
        service
            .set_node_outbound_groups(&first, &original, &BTreeSet::new())
            .unwrap();
        service
            .set_node_outbound_groups(&first, &original, &BTreeSet::new())
            .unwrap();
        assert!(
            service
                .set_node_outbound_groups(&first, &original, &["proxy".into()].into())
                .is_err()
        );
        for group in &service.model().groups {
            assert_eq!(group.members, [second.clone()]);
            assert_eq!(group.selected.as_ref(), Some(&second));
        }
        assert!(
            service
                .set_node_outbound_groups(&first, &BTreeSet::new(), &["missing".into()].into())
                .is_err()
        );
        assert!(
            service
                .set_node_outbound_groups("missing", &BTreeSet::new(), &BTreeSet::new())
                .is_err()
        );
    }

    #[test]
    fn managed_group_edits_compile_for_both_cores() {
        let (service, dir, first, second) = fixture();
        let mut group = group("Regional", &[first.clone(), second.clone()]);
        group.switch_policy.mode = SwitchMode::Latency;
        service.save_outbound_group(None, group.clone()).unwrap();
        service.set_default_outbound_group(&group.id).unwrap();
        service
            .set_node_outbound_groups(
                &first,
                &["proxy".into(), group.id.clone()].into(),
                &["proxy".into()].into(),
            )
            .unwrap();
        let model = service.model();
        let settings = service.settings();
        let ctx = CompileContext::new(
            &settings,
            dir.path().join("data"),
            dir.path().join("runtime"),
        );
        for core in [CoreKind::Xray, CoreKind::SingBox] {
            let compiled = adapter_for(core).compile(&model, &settings, &ctx).unwrap();
            let managed = compiled
                .managed_groups
                .iter()
                .find(|g| g.id == group.id)
                .unwrap();
            assert_eq!(managed.members, [second.clone()]);
            assert_eq!(managed.selected.as_ref(), Some(&second));
            assert_eq!(managed.switch_mode(), SwitchMode::Latency);
            assert_eq!(
                compiled.default_outbound,
                match core {
                    CoreKind::Xray => model.node(&second).unwrap().outbound_tag(),
                    CoreKind::SingBox => group.tag(),
                }
            );
        }
    }

    #[test]
    fn storage_failures_publish_no_group_or_membership_changes() {
        let (service, dir, first, second) = fixture();
        let group = group("Regional", &[first.clone(), second]);
        service.save_outbound_group(None, group.clone()).unwrap();
        let before = service.model();
        let db = rusqlite::Connection::open(AppPaths::from_root(dir.path()).database()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_profile_update BEFORE UPDATE ON profiles BEGIN SELECT RAISE(FAIL, 'test storage failure'); END;").unwrap();
        assert!(service.delete_outbound_group(&group.id).is_err());
        assert!(
            service
                .save_outbound_group(None, self::group("New group", std::slice::from_ref(&first)))
                .is_err()
        );
        let original = before.group(&group.id).unwrap();
        let mut edit = original.clone();
        edit.name = "Changed".into();
        assert!(service.save_outbound_group(Some(original), edit).is_err());
        assert!(
            service
                .set_node_outbound_groups(
                    &first,
                    &["proxy".into(), group.id].into(),
                    &BTreeSet::new()
                )
                .is_err()
        );
        assert_eq!(service.model(), before);
    }
}
