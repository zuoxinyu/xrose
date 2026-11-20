//! Live, per-group automatic switching settings.
use super::*;
use crate::ui::dialogs::{dialog_footer, settings_dialog};
use gpui_kit::component::WindowExt;
use std::collections::BTreeMap;
use xrose_core::model::{
    FallbackScope, GroupKind, HealthProfile, NodeSwitchPolicy, OutboundGroup, SwitchMode,
};

const MODES: [SwitchMode; 5] = [
    SwitchMode::Off,
    SwitchMode::Failover,
    SwitchMode::Latency,
    SwitchMode::Throughput,
    SwitchMode::Health,
];

struct SwitchDialogState {
    selected_group_id: String,
    drafts: BTreeMap<String, (NodeSwitchPolicy, u64)>,
    error: Option<String>,
}

fn changed_switch_policies(
    groups: &[OutboundGroup],
    drafts: &BTreeMap<String, (NodeSwitchPolicy, u64)>,
) -> Vec<(String, NodeSwitchPolicy, u64)> {
    groups
        .iter()
        .filter_map(|group| {
            let (policy, interval) = drafts.get(&group.id)?;
            let changed =
                *policy != group.switch_policy || *interval != group.interval_secs.max(30);
            changed.then(|| (group.id.clone(), *policy, *interval))
        })
        .collect()
}

impl AppView {
    pub(in crate::ui) fn outbound_group_label(&self, group: &OutboundGroup) -> SharedString {
        crate::i18n::outbound_group_label(self.lang(), group)
    }

    pub(in crate::ui) fn set_group_switch_mode(
        &mut self,
        group_id: &str,
        mode: SwitchMode,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.snapshot.model.group(group_id) else {
            return;
        };
        let mut policy = group.switch_policy;
        policy.mode = mode;
        policy.consecutive_failures = policy.consecutive_failures.clamp(1, 10);
        let interval = group.interval_secs.max(30);
        if let Err(err) = self
            .service
            .set_node_switch_policy(group_id, policy, interval)
        {
            self.service
                .toast(xrose_core::service::ToastLevel::Error, err.to_string());
        }
        self.sync(cx);
    }

    pub(in crate::ui) fn open_node_switch_dialog(
        &self,
        initial_group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(initial) = initial_group
            .filter(|id| self.snapshot.model.group(id).is_some())
            .or_else(|| self.snapshot.model.default_group_id())
        else {
            return;
        };
        let drafts = self
            .snapshot
            .model
            .groups
            .iter()
            .map(|group| {
                (
                    group.id.clone(),
                    (group.switch_policy, group.interval_secs.max(30)),
                )
            })
            .collect();
        let editor = cx.new(|_| SwitchDialogState {
            selected_group_id: initial,
            drafts,
            error: None,
        });
        let profile = self.snapshot.active_profile.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let busy = this_busy(&this.snapshot.busy, "core");
                let state = editor.read(cx);
                let selected_id = state.selected_group_id.clone();
                let dialog_error = state.error.clone();
                let group = this
                    .snapshot
                    .model
                    .group(&selected_id)
                    .cloned()
                    .map(|mut g| {
                        if let Some((policy, interval)) = state.drafts.get(&g.id) {
                            g.switch_policy = *policy;
                            g.interval_secs = *interval;
                        }
                        g
                    });
                let body = match group {
                    Some(group) => this.node_switch_controls(&group, editor.clone(), &p, cx),
                    None => empty_hint(
                        &p,
                        this.tr(
                            "出站组已移除，请重新打开设置。",
                            "This group was removed. Reopen settings.",
                        ),
                    )
                    .into_any_element(),
                };
                let save_editor = editor.clone();
                let profile = profile.clone();
                let actions = div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-node-switch")
                            .label(this.tr("取消", "Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-node-switch")
                            .primary()
                            .label(this.tr("保存", "Save"))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let result = if this.snapshot.active_profile != profile {
                                    Err(xrose_core::error::Error::config(
                                        this.tr(
                                            "当前配置已切换，请重新打开后编辑",
                                            "Configuration changed. Reopen to edit.",
                                        )
                                        .to_string(),
                                    ))
                                } else {
                                    let updates = changed_switch_policies(
                                        &this.snapshot.model.groups,
                                        &save_editor.read(cx).drafts,
                                    );
                                    updates.into_iter().try_for_each(|(id, policy, interval)| {
                                        this.service.set_node_switch_policy(&id, policy, interval)
                                    })
                                };
                                match result {
                                    Ok(()) => {
                                        this.sync(cx);
                                        window.close_dialog(cx);
                                    }
                                    Err(err) => {
                                        save_editor.update(cx, |state, _| {
                                            state.error = Some(err.to_string());
                                        });
                                        cx.notify();
                                    }
                                }
                            })),
                    );
                settings_dialog(
                    dialog,
                    this.tr("自动切换设置", "Auto-switch settings"),
                    body,
                    &p,
                    window,
                )
                .overlay_closable(false)
                .footer(dialog_footer(&p, dialog_error, actions))
            })
        });
    }

    fn node_switch_controls(
        &self,
        group: &OutboundGroup,
        editor: Entity<SwitchDialogState>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut controls = div().flex().flex_col().gap_2().pb_1();
        let groups = &self.snapshot.model.groups;
        if groups.len() > 1 {
            let ids: Vec<_> = groups.iter().map(|g| g.id.clone()).collect();
            let group_editor = editor.clone();
            let choices = ChoiceGroup::new(
                "switch-policy-group",
                groups
                    .iter()
                    .map(|g| self.outbound_group_label(g))
                    .collect(),
                groups.iter().position(|g| g.id == group.id),
            )
            .small()
            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                let id = &ids[*index];
                if this.snapshot.model.group(id).is_some() {
                    group_editor.update(cx, |state, _| {
                        state.selected_group_id = id.clone();
                        state.error = None;
                    });
                    cx.notify();
                }
            }));
            controls = controls.child(dialog_control_row(
                p,
                self.tr("配置出站组", "Configure group"),
                "",
                choices,
            ));
        }
        let id = group.id.clone();
        controls
            .child(self.node_policy_fields(
                group,
                move |_, policy, interval, cx| {
                    editor.update(cx, |state, _| {
                        state.drafts.insert(id.clone(), (policy, interval));
                        state.error = None;
                    });
                    cx.notify();
                },
                cx,
            ))
            .into_any_element()
    }

    pub(super) fn node_policy_fields(
        &self,
        group: &OutboundGroup,
        on_change: impl Fn(&mut Self, NodeSwitchPolicy, u64, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = palette(cx);
        let disabled = this_busy(&self.snapshot.busy, "core");
        let policy = group.switch_policy;
        let interval = group.interval_secs.max(30);
        let mode = match (group.switch_mode(), group.kind) {
            (SwitchMode::GroupDefault, GroupKind::UrlTest) => SwitchMode::Latency,
            (mode, _) => mode,
        };
        let mode_handler = on_change.clone();
        let modes = ChoiceGroup::new(
            "node-switch-mode",
            vec![
                self.tr("关闭", "Off"),
                self.tr("仅故障切换", "Failover only"),
                self.tr("延迟优先", "Lowest latency"),
                self.tr("吞吐优先", "Highest throughput"),
                self.tr("健康评分", "Health score"),
            ],
            MODES.iter().position(|m| *m == mode),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            mode_handler(
                this,
                NodeSwitchPolicy {
                    mode: MODES[*i],
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let policy = NodeSwitchPolicy { mode, ..policy };
        let profiles = HealthProfile::group_choices();
        let profile_handler = on_change.clone();
        let health_profile = ChoiceGroup::new(
            "node-health-profile",
            vec![
                self.tr("综合", "General"),
                self.tr("AI 优先", "AI"),
                self.tr("流媒体", "Streaming"),
                self.tr("低延迟", "Low latency"),
                self.tr("大流量", "Bulk"),
            ],
            profiles.iter().position(|p| *p == policy.health_profile),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            let next_profile = profiles[*i];
            profile_handler(
                this,
                NodeSwitchPolicy {
                    health_profile: next_profile,
                    sticky_session: next_profile.default_sticky_session(),
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let sticky_handler = on_change.clone();
        let sticky = toggle(
            "node-sticky-session",
            self.tr(
                "粘性会话（仅连通失败时切换）",
                "Sticky session (switch only on connectivity failure)",
            ),
            policy.sticky_session,
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, checked: &bool, _, cx| {
            sticky_handler(
                this,
                NodeSwitchPolicy {
                    sticky_session: *checked,
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let budgets = [50u32, 100, 250, 0];
        let current_budget = self.snapshot.settings.daily_probe_budget_mb;
        let budget_choices = ChoiceGroup::new(
            "node-probe-budget",
            vec![
                "50 MiB".into(),
                "100 MiB".into(),
                "250 MiB".into(),
                self.tr("不限", "Unlimited"),
            ],
            budgets.iter().position(|b| *b == current_budget),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            if let Err(err) = this.service.set_daily_probe_budget_mb(budgets[*i]) {
                let _ = err;
            }
            this.sync(cx);
        }));
        let scopes = [
            FallbackScope::Group,
            FallbackScope::SameSubscription,
            FallbackScope::SameRegion,
        ];
        let scope_handler = on_change.clone();
        let scope = ChoiceGroup::new(
            "node-fallback-scope",
            vec![
                self.tr("组内全部", "Entire group"),
                self.tr("同订阅", "Same subscription"),
                self.tr("同地区", "Same region"),
            ],
            scopes.iter().position(|s| *s == policy.fallback),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            scope_handler(
                this,
                NodeSwitchPolicy {
                    fallback: scopes[*i],
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let count = policy.consecutive_failures.max(1);
        let mut counts = vec![1, 2, 3];
        if !counts.contains(&count) {
            counts.push(count);
        }
        let count_handler = on_change.clone();
        let failures = ChoiceGroup::new(
            "node-failure-count",
            counts.iter().map(|n| n.to_string().into()).collect(),
            counts.iter().position(|n| *n == count),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            count_handler(
                this,
                NodeSwitchPolicy {
                    consecutive_failures: counts[*i],
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let mut intervals = vec![30, 60, 300];
        if !intervals.contains(&interval) {
            intervals.push(interval);
            intervals.sort_unstable();
        }

        let timing = ChoiceGroup::new(
            "node-check-interval",
            intervals.iter().map(|s| format!("{s} s").into()).collect(),
            intervals.iter().position(|s| *s == interval),
        )
        .small()
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            on_change(this, policy, intervals[*i], cx);
        }));
        let mode_note = if mode == SwitchMode::GroupDefault {
            self.tr(
                "此组沿用导入的负载均衡设置。选择切换模式可更改策略。",
                "This group uses imported load balancing. Choose a mode to change its policy.",
            )
        } else if mode == SwitchMode::Throughput {
            self.tr(
                "每轮每个节点最多下载 1 MiB，下载速度至少提高 10% 时切换。",
                "Each round downloads up to 1 MiB per node. Switching requires a speed gain of at least 10%.",
            )
        } else if mode == SwitchMode::Health {
            self.tr(
                "候选短期分领先 ≥12 分并连续 3 轮保持时切换；仅连续连通失败时回退；冷却 5 分钟。",
                "Switches after 3 rounds leading by >= 12 short points; recovers only on consecutive connectivity failures; 5 min cooldown.",
            )
        } else if mode == SwitchMode::Off {
            self.tr(
                "启用自动切换后使用这些条件。",
                "These conditions take effect when auto switch is enabled.",
            )
        } else if group.managed() {
            self.tr(
                "仅检查路由使用的出站组。切换时会重载内核，现有连接将中断。",
                "Only groups used by routing are checked. Reloading the core interrupts active connections.",
            )
        } else {
            self.tr(
                "此组沿用内核测速。修改回退条件或检查间隔后，将使用这里设置的条件。",
                "This group uses native latency checks. Editing a fallback condition or the interval enables the settings shown here.",
            )
        };
        let scope_note = if policy.fallback == FallbackScope::SameRegion {
            self.tr(
                "按服务器 IP 匹配国家或地区，归属地未知时跳过。",
                "Regions are matched by server IP. Nodes with an unknown region are skipped.",
            )
        } else {
            "".into()
        };
        let interval_note = if group.managed() && this_busy(&self.snapshot.busy, "auto-probe") {
            self.tr("正在检查节点…", "Checking nodes…")
        } else {
            "".into()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(dialog_control_row(
                &p,
                self.tr("自动切换", "Auto switch"),
                mode_note,
                modes,
            ))
            .when(mode == SwitchMode::Health, |d| {
                d.child(dialog_control_row(
                    &p,
                    self.tr("评估场景", "Health profile"),
                    self.tr(
                        "切换场景时会同步推荐的粘性会话默认值。",
                        "Applies the recommended sticky-session default when switched.",
                    ),
                    health_profile,
                ))
                .child(dialog_control_row(
                    &p,
                    self.tr("粘性会话", "Sticky session"),
                    self.tr(
                        "开启后仅在连续连通性失败时切换，绝不因分差或门限中断使用中的节点。",
                        "Switch only on consecutive connectivity failures, never interrupting a working node due to score.",
                    ),
                    sticky,
                ))
                .child(dialog_control_row(
                    &p,
                    self.tr("每日探测上限", "Daily probe cap"),
                    self.tr(
                        "超限后暂停自动探测与切换，不影响手动测速。",
                        "Pauses background auto-probing once reached; manual tests still run.",
                    ),
                    budget_choices,
                ))
            })
            .child(dialog_control_row(
                &p,
                self.tr("故障回退范围", "Fallback scope"),
                scope_note,
                scope,
            ))
            .child(dialog_control_row(
                &p,
                self.tr("连续失败次数", "Consecutive failures"),
                "",
                failures,
            ))
            .child(dialog_control_row(
                &p,
                self.tr("检查间隔", "Check interval"),
                interval_note,
                timing,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::changed_switch_policies;
    use std::collections::BTreeMap;
    use xrose_core::model::{
        FallbackScope, GroupKind, NodeSwitchPolicy, OutboundGroup, SwitchMode,
    };

    #[test]
    fn changed_switch_policies_returns_only_modified_groups() {
        let mut a = OutboundGroup::new("A", GroupKind::Selector);
        a.id = "a".into();
        a.switch_policy = NodeSwitchPolicy {
            mode: SwitchMode::Off,
            fallback: FallbackScope::Group,
            consecutive_failures: 2,
            ..Default::default()
        };
        a.interval_secs = 300;

        let mut b = OutboundGroup::new("B", GroupKind::Selector);
        b.id = "b".into();
        b.switch_policy = NodeSwitchPolicy {
            mode: SwitchMode::Failover,
            fallback: FallbackScope::SameSubscription,
            consecutive_failures: 1,
            ..Default::default()
        };
        b.interval_secs = 15; // Below minimum 30s normalized in dialog drafts.

        let mut drafts = BTreeMap::new();
        drafts.insert("a".into(), (a.switch_policy, 60));
        drafts.insert("b".into(), (b.switch_policy, 30));

        let updates = changed_switch_policies(&[a.clone(), b], &drafts);
        assert_eq!(updates, vec![("a".into(), a.switch_policy, 60)]);
    }
}
