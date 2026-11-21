//! Failed, stalled, and rejected connection observation and batch rule fixes.

use std::collections::HashSet;

use super::*;
use crate::diagnostics::DiagnosticRuleKind;
use crate::failed_connections::{
    AppliedRuleSummary, BatchRuleApplyOutcome, DomainMatchGranularity, FailedRuleDraft,
    matchers_for_record_kind, rule_name_for_expression,
};
use crate::model::{Matchers, RouteAction, RouteActionKind};

impl AppService {
    pub(super) fn install_failed_connection_log_observer(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.logs().set_core_line_observer(move |_stream, line| {
            let Some(inner) = weak.upgrade() else { return };
            let service = AppService { inner };
            let core = service
                .process_state()
                .core
                .unwrap_or_else(|| service.settings().core);
            let model = service
                .inner
                .core
                .statistics_context()
                .map(|(m, _, _)| m)
                .unwrap_or_else(|| service.model());
            let now = now_unix();
            let offline = service.inner.health.lock().is_host_offline(now);
            let view = {
                let mut tracker = service.inner.failed_connections.lock();
                tracker.set_external_host_offline(offline);
                tracker.observe_log_line(core, line, &model, now);
                tracker.snapshot(now)
            };
            *service.inner.failed_connections_view.write() = Arc::new(view);
        });
    }

    pub(super) fn observe_failed_clash_connections(
        &self,
        epoch_changed: bool,
        connections: &[ClashConnection],
        model: &AppModel,
    ) {
        let now = now_unix();
        let offline = self.inner.health.lock().is_host_offline(now);
        let view = {
            let mut tracker = self.inner.failed_connections.lock();
            if epoch_changed {
                tracker.on_core_epoch_changed();
            }
            tracker.set_external_host_offline(offline);
            tracker.observe_clash_connections(connections, model, now);
            tracker.snapshot(now)
        };
        *self.inner.failed_connections_view.write() = Arc::new(view);
    }

    pub(super) fn publish_failed_connections(&self) {
        let now = now_unix();
        let offline = self.inner.health.lock().is_host_offline(now);
        let view = {
            let mut tracker = self.inner.failed_connections.lock();
            tracker.set_external_host_offline(offline);
            tracker.snapshot(now)
        };
        *self.inner.failed_connections_view.write() = Arc::new(view);
    }

    pub fn clear_failed_connections(&self) {
        {
            self.inner.failed_connections.lock().clear();
        }
        self.publish_failed_connections();
    }

    pub fn dismiss_failed_connection(&self, id: u64) {
        let removed = self.inner.failed_connections.lock().dismiss(id);
        if removed {
            self.publish_failed_connections();
        }
    }

    /// One-click fix used by the tray flyout and single-row quick action.
    /// Uses registerable root-domain suffix (via PSL) when available and the
    /// record's recommended action, debounced through `mark_dirty()` so rapid
    /// fixes merge into a single reload.
    pub fn quick_fix_failed_connection(&self, id: u64) {
        let Some(record) = self.inner.failed_connections.lock().record(id) else {
            return;
        };
        let work_mode = self.model().work_mode;
        let action = record.suggested_action(work_mode);
        let draft = FailedRuleDraft {
            record_id: id,
            granularity: DomainMatchGranularity::RootDomainSuffix,
        };
        self.dispatch_failed_rule_batch(vec![draft], action, false);
    }

    /// Saves rules for one or more failed records in a single SQLite commit
    /// and triggers at most one core reload when hot reload is active.
    pub fn apply_failed_connection_rules(&self, drafts: Vec<FailedRuleDraft>, action: RouteAction) {
        if drafts.is_empty() {
            return;
        }
        self.dispatch_failed_rule_batch(drafts, action, true);
    }

    /// Single-record menu shortcut for a specific matcher kind (exact domain, IP, or process).
    pub fn add_failed_connection_rule(
        &self,
        id: u64,
        kind: DiagnosticRuleKind,
        action: RouteAction,
    ) {
        if !self.inner.busy.write().insert("failed-rule".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let worker = service.clone();
            let result = tokio::task::spawn_blocking(move || {
                let record = worker
                    .inner
                    .failed_connections
                    .lock()
                    .record(id)
                    .ok_or_else(|| Error::config("该失败记录已过期或被清空"))?;
                let (expr, matchers) = matchers_for_record_kind(&record, kind)?;
                worker.commit_failed_rules_sync(&[id], vec![(expr, matchers)], action, false)
            })
            .await;
            service.finish_failed_rule_result(result);
            service.set_busy("failed-rule", false);
        });
    }

    /// Single-record menu shortcut using root-domain suffix with an explicit action.
    pub fn add_failed_connection_suffix_rule(&self, id: u64, action: RouteAction) {
        self.dispatch_failed_rule_batch(
            vec![FailedRuleDraft {
                record_id: id,
                granularity: DomainMatchGranularity::RootDomainSuffix,
            }],
            action,
            false,
        );
    }

    fn dispatch_failed_rule_batch(
        &self,
        drafts: Vec<FailedRuleDraft>,
        action: RouteAction,
        immediate_reload: bool,
    ) {
        if !self.inner.busy.write().insert("failed-rule".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let worker = service.clone();
            let result = tokio::task::spawn_blocking(move || {
                worker.apply_failed_rules_sync(&drafts, action, immediate_reload)
            })
            .await;
            service.finish_failed_rule_result(result);
            service.set_busy("failed-rule", false);
        });
    }

    fn finish_failed_rule_result(
        &self,
        result: std::result::Result<
            Result<(BatchRuleApplyOutcome, String)>,
            tokio::task::JoinError,
        >,
    ) {
        match result {
            Ok(Ok((_, message))) => self.toast(ToastLevel::Success, message),
            Ok(Err(err)) => self.toast(ToastLevel::Error, err.to_string()),
            Err(_) => self.toast(ToastLevel::Error, "保存失败连接规则时发生错误，请重试"),
        }
    }

    pub(crate) fn apply_failed_rules_sync(
        &self,
        drafts: &[FailedRuleDraft],
        action: RouteAction,
        immediate_reload: bool,
    ) -> Result<(BatchRuleApplyOutcome, String)> {
        let mut explicit_ids = Vec::with_capacity(drafts.len());
        let mut specs = Vec::new();
        let mut seen_exprs = HashSet::new();
        {
            let tracker = self.inner.failed_connections.lock();
            for draft in drafts {
                let Some(record) = tracker.record(draft.record_id) else {
                    continue;
                };
                explicit_ids.push(draft.record_id);
                let (expr, matchers) = record.build_matchers(draft.granularity)?;
                if seen_exprs.insert(expr.clone()) {
                    specs.push((expr, matchers));
                }
            }
        }
        if specs.is_empty() {
            return Err(Error::config("所选失败记录已过期或缺少可用的域名/IP"));
        }
        self.commit_failed_rules_sync(&explicit_ids, specs, action, immediate_reload)
    }

    fn commit_failed_rules_sync(
        &self,
        explicit_ids: &[u64],
        specs: Vec<(String, Matchers)>,
        action: RouteAction,
        immediate_reload: bool,
    ) -> Result<(BatchRuleApplyOutcome, String)> {
        for (_, matchers) in &specs {
            if crate::flow::blocks_protected_process(matchers, &action) {
                return Err(Error::config(
                    "不能阻止 Xrose 或提权助手联网，否则会导致代理与组网异常",
                ));
            }
        }
        let (work_mode, outbound_label, applied_entries, rules_saved) = {
            let mut model = self.inner.model.write();
            let outbound_label = match action.kind {
                RouteActionKind::Proxy => {
                    if let Some(group_id) = &action.group {
                        let Some(group) = model.group(group_id).filter(|g| g.enabled) else {
                            return Err(Error::config("所选出站组已不存在或已停用，请重新选择"));
                        };
                        group.name.clone()
                    } else {
                        "默认出站组".to_owned()
                    }
                }
                RouteActionKind::Direct => "直连".to_owned(),
                RouteActionKind::Block => "阻止".to_owned(),
            };
            let mut next = model.clone();
            let mut applied_entries = Vec::with_capacity(specs.len());
            let mut inserted_rules = Vec::with_capacity(specs.len());
            for (expr, matchers) in specs {
                let rule = RouteRule::new(
                    rule_name_for_expression(&expr),
                    matchers.clone(),
                    action.clone(),
                );
                inserted_rules.push(rule);
                applied_entries.push((
                    matchers,
                    AppliedRuleSummary {
                        expression: expr,
                        action: action.clone(),
                        outbound_label: outbound_label.clone(),
                    },
                ));
            }
            let rules_saved = inserted_rules.len();
            if !next.routing.rule_groups.is_empty() {
                let index = next
                    .routing
                    .rule_groups
                    .iter()
                    .position(|g| g.id == "diagnostic-overrides");
                let mut group = index
                    .map(|i| next.routing.rule_groups.remove(i))
                    .unwrap_or_else(|| crate::model::RouteRuleGroup {
                        id: "diagnostic-overrides".into(),
                        name: "诊断规则".into(),
                        ..Default::default()
                    });
                group.enabled = true;
                for rule in inserted_rules.into_iter().rev() {
                    group.rules.retain(|r| r.matchers != rule.matchers);
                    group.rules.insert(0, rule);
                }
                next.routing.rule_groups.insert(0, group);
            } else {
                for rule in inserted_rules.into_iter().rev() {
                    next.routing.rules.retain(|r| r.matchers != rule.matchers);
                    next.routing.rules.insert(0, rule);
                }
            }
            next.routing.preset = RoutePreset::Custom;
            next.updated_at = now_unix();
            let profile = self.inner.store.save_active_profile(&next)?;
            *self.inner.active_profile.write() = Some(profile);
            let mode = next.work_mode;
            *model = next;
            (mode, outbound_label, applied_entries, rules_saved)
        };

        let records_marked = {
            let mut tracker = self.inner.failed_connections.lock();
            tracker.mark_rules_applied(explicit_ids, &applied_entries)
        };
        self.publish_failed_connections();
        self.mark_dirty();

        let running = self.is_running();
        let hot_reload = self.settings().hot_reload;
        let reloaded = running && hot_reload;
        if immediate_reload && reloaded {
            self.apply_config_silent();
        }

        let action_desc = match action.kind {
            RouteActionKind::Proxy => format!("代理（{outbound_label}）"),
            RouteActionKind::Direct => "直连".to_owned(),
            RouteActionKind::Block => "阻止".to_owned(),
        };
        let message = if work_mode == WorkMode::Mesh {
            format!("已保存 {rules_saved} 条规则至本机（{action_desc}）；组网模式暂停本地代理分流")
        } else if work_mode == WorkMode::Direct && action.kind == RouteActionKind::Proxy {
            format!(
                "已保存 {rules_saved} 条代理规则（{action_desc}）；当前为本机直连模式，切回代理后生效"
            )
        } else if reloaded {
            format!("已置顶保存 {rules_saved} 条规则（{action_desc}），正在应用配置")
        } else if running {
            format!("已置顶保存 {rules_saved} 条规则（{action_desc}），应用配置后生效")
        } else {
            format!("已置顶保存 {rules_saved} 条规则（{action_desc}）")
        };

        Ok((
            BatchRuleApplyOutcome {
                rules_saved,
                records_marked,
                reloaded,
            },
            message,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failed_connections::{DomainMatchGranularity, FailedRuleDraft, FailureKind};

    #[test]
    fn core_log_observer_populates_snapshot_and_batch_apply_deduplicates_rules() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        service.logs().core_line(
            LogStream::Stderr,
            "+0800 2026-11-21 17:00:00 ERROR [101 5.0s] connection: open connection to api.docs.example.org:443 using outbound/direct[direct]: dial tcp 203.0.113.18:443: i/o timeout",
        );
        service.logs().core_line(
            LogStream::Stderr,
            "+0800 2026-11-21 17:00:01 ERROR [102 5.0s] connection: open connection to cdn.docs.example.org:443 using outbound/direct[direct]: dial tcp 203.0.113.19:443: i/o timeout",
        );

        let snap = service.snapshot();
        assert_eq!(snap.failed_connections.records.len(), 2);
        assert_eq!(snap.failed_connections.unfixed_count, 2);
        assert_eq!(
            snap.failed_connections.records[0].failure_kind,
            FailureKind::Timeout
        );
        assert_eq!(service.tray_status().unfixed_failed_connections, 2);

        let drafts: Vec<_> = snap
            .failed_connections
            .records
            .iter()
            .map(|r| FailedRuleDraft {
                record_id: r.id,
                granularity: DomainMatchGranularity::RootDomainSuffix,
            })
            .collect();
        let (outcome, _) = service
            .apply_failed_rules_sync(&drafts, RouteAction::proxy(), false)
            .unwrap();
        assert_eq!(outcome.rules_saved, 1);
        assert_eq!(outcome.records_marked, 2);

        let after = service.snapshot();
        assert_eq!(after.failed_connections.unfixed_count, 0);
        assert_eq!(service.tray_status().unfixed_failed_connections, 0);
        let first_rule = after.model.routing.effective_rules().next().unwrap();
        assert_eq!(first_rule.matchers.domain_suffixes, ["example.org"]);
        assert_eq!(first_rule.action, RouteAction::proxy());
    }

    #[test]
    fn dismiss_and_clear_update_failed_connections_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        service.logs().core_line(
            LogStream::Stdout,
            "ERROR [11 2.0s] dns: exchange failed for one.example.io. IN A: i/o timeout",
        );
        service.logs().core_line(
            LogStream::Stdout,
            "ERROR [12 2.0s] dns: exchange failed for two.example.io. IN A: i/o timeout",
        );
        let snap = service.snapshot();
        assert_eq!(snap.failed_connections.records.len(), 2);
        let id = snap.failed_connections.records[0].id;
        service.dismiss_failed_connection(id);
        assert_eq!(service.snapshot().failed_connections.records.len(), 1);
        service.clear_failed_connections();
        assert!(service.snapshot().failed_connections.records.is_empty());
    }
}
