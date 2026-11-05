//! A single switch policy control for the default outbound, with optional per-group settings.
use super::*;
use xross_core::model::{FallbackScope, GroupKind, NodeSwitchPolicy, OutboundGroup, SwitchMode};
use xross_core::service::ToastLevel;

const MODES: [SwitchMode; 4] = [
    SwitchMode::Off,
    SwitchMode::Failover,
    SwitchMode::Latency,
    SwitchMode::Throughput,
];

impl AppView {
    pub(in crate::ui) fn outbound_group_label(&self, group: &OutboundGroup) -> SharedString {
        match (group.id.as_str(), group.name.as_str()) {
            ("proxy", "代理") => self.tr("代理", "Proxy"),
            ("auto", "自动选择") => self.tr("自动选择", "Auto select"),
            _ => group.name.clone().into(),
        }
    }

    pub(super) fn node_switch_controls(
        &self,
        group: &OutboundGroup,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let disabled = this_busy(&self.snapshot.busy, "core") || !group.enabled;
        let expanded = self.expanded_switch_group.is_some();
        let policy = group.switch_policy;
        let interval = group.interval_secs.max(30);
        let mode = match (group.switch_mode(), group.kind) {
            // Native latency groups already choose by latency. Editing their conditions opts
            // into the same client-side measurements as explicitly choosing this mode.
            (SwitchMode::GroupDefault, GroupKind::UrlTest) => SwitchMode::Latency,
            (mode, _) => mode,
        };
        let mode_id = group.id.clone();
        let expand_id = group.id.clone();
        let disclosure = if expanded {
            self.tr("收起高级设置", "Collapse advanced settings")
        } else {
            self.tr("展开高级设置", "Expand advanced settings")
        };
        let expand_button = Button::new("switch-advanced")
            .compact()
            .ghost()
            .icon(if expanded {
                IconName::ChevronUp
            } else {
                IconName::ChevronRight
            })
            .size(px(crate::ui::theme::ICON_BUTTON_SIZE))
            .flex_shrink_0()
            .accessibility_label(format!(
                "{} · {disclosure}",
                self.tr("自动切换", "Auto switch")
            ))
            .tooltip(disclosure)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded_switch_group = if expanded {
                    None
                } else {
                    Some(expand_id.clone())
                };
                cx.notify();
            }));
        let modes = ChoiceGroup::new(
            "node-switch-mode",
            vec![
                self.tr("关闭", "Off"),
                self.tr("仅故障切换", "Failover only"),
                self.tr("延迟优先", "Lowest latency"),
                self.tr("吞吐优先", "Highest throughput"),
            ],
            MODES.iter().position(|m| *m == mode),
        )
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            this.save_node_switch(
                &mode_id,
                NodeSwitchPolicy {
                    mode: MODES[*i],
                    ..policy
                },
                interval,
                cx,
            );
        }));
        let row = |label: SharedString| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(p.muted_fg).child(label))
        };
        let note = |label: SharedString| div().text_xs().text_color(p.muted_fg).child(label);
        let controls = div().flex().flex_col().gap_2().child(
            row(self.tr("自动切换", "Auto switch"))
                .child(modes)
                .child(expand_button),
        );
        if !expanded {
            return controls;
        }
        let groups = &self.snapshot.model.groups;
        let controls = controls.when(groups.len() > 1, |d| {
            let ids: Vec<_> = groups.iter().map(|g| g.id.clone()).collect();
            let choices = ChoiceGroup::new(
                "switch-policy-group",
                groups
                    .iter()
                    .map(|g| self.outbound_group_label(g))
                    .collect(),
                groups.iter().position(|g| g.id == group.id),
            )
            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                let id = &ids[*index];
                if this.expanded_switch_group.is_some() && this.snapshot.model.group(id).is_some() {
                    this.expanded_switch_group = Some(id.clone());
                    cx.notify();
                }
            }));
            d.child(row(self.tr("配置出站组", "Configure group")).child(choices))
        });
        if mode == SwitchMode::GroupDefault {
            return controls.child(note(self.tr(
                "此组沿用导入的负载均衡设置。选择上方任一模式可更改。",
                "This group uses imported load balancing. Choose a mode above to change it.",
            )));
        }
        let policy = NodeSwitchPolicy { mode, ..policy };
        let scopes = [
            FallbackScope::Group,
            FallbackScope::SameSubscription,
            FallbackScope::SameRegion,
        ];
        let scope_id = group.id.clone();
        let scope = ChoiceGroup::new(
            "node-fallback-scope",
            vec![
                self.tr("组内全部", "Entire group"),
                self.tr("同订阅", "Same subscription"),
                self.tr("同地区", "Same region"),
            ],
            scopes.iter().position(|s| *s == policy.fallback),
        )
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            this.save_node_switch(
                &scope_id,
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
        let count_id = group.id.clone();
        let failures = ChoiceGroup::new(
            "node-failure-count",
            counts.iter().map(|n| n.to_string().into()).collect(),
            counts.iter().position(|n| *n == count),
        )
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            this.save_node_switch(
                &count_id,
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
        let interval_id = group.id.clone();
        let timing = ChoiceGroup::new(
            "node-check-interval",
            intervals.iter().map(|s| format!("{s} s").into()).collect(),
            intervals.iter().position(|s| *s == interval),
        )
        .disabled(disabled)
        .on_change(cx.listener(move |this, i: &usize, _, cx| {
            this.save_node_switch(&interval_id, policy, intervals[*i], cx);
        }));
        let explanation = if mode == SwitchMode::Off {
            self.tr(
                "启用自动切换后使用这些条件。",
                "These conditions take effect when auto switch is enabled.",
            )
        } else if group.managed() {
            self.tr("仅检查路由使用的出站组。切换时会重启内核，现有连接将中断。",
                "Only groups used by routing are checked. Switching restarts the core and interrupts active connections.")
        } else {
            self.tr("此组沿用内核测速。修改回退条件或检查间隔后，将使用这里设置的条件。",
                "This group uses native latency checks. Editing a fallback condition or the interval enables the settings shown here.")
        };
        controls
            .child(row(self.tr("故障回退范围", "Fallback scope")).child(scope))
            .child(row(self.tr("连续失败次数", "Consecutive failures")).child(failures))
            .child(row(self.tr("检查间隔", "Check interval")).child(timing))
            .child(note(explanation))
            .when(mode == SwitchMode::Throughput, |d| {
                d.child(note(self.tr(
                    "每轮每个节点最多下载 1 MiB，下载速度至少提高 10% 时切换。",
                    "Each round downloads up to 1 MiB per node. Switching requires a speed gain of at least 10%.",
                )))
            })
            .when(policy.fallback == FallbackScope::SameRegion, |d| {
                d.child(note(self.tr(
                    "按服务器 IP 匹配国家或地区，归属地未知时跳过。",
                    "Regions are matched by server IP. Nodes with an unknown region are skipped.",
                )))
            })
            .when(group.managed() && this_busy(&self.snapshot.busy, "auto-probe"), |d| {
                d.child(note(self.tr("正在检查节点…", "Checking nodes…")))
            })
    }

    fn save_node_switch(
        &mut self,
        group: &str,
        policy: NodeSwitchPolicy,
        interval: u64,
        cx: &mut Context<Self>,
    ) {
        if let Err(err) = self.service.set_node_switch_policy(group, policy, interval) {
            self.service.toast(ToastLevel::Error, err.to_string());
        }
        self.sync(cx);
    }
}
