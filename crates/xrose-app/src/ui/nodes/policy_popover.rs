//! Lightweight header trigger and popover for default outbound group & auto-switch policy.

use super::*;
use crate::ui::theme::{CONTROL_HEIGHT, Palette, SEMANTIC_ALPHA, palette};
use crate::ui::{NodeView, tr};
use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::popover::Popover;
use xrose_core::model::{GroupKind, HealthProfile, OutboundGroup, SwitchMode};

const QUICK_MODES: [SwitchMode; 5] = [
    SwitchMode::Off,
    SwitchMode::Health,
    SwitchMode::Latency,
    SwitchMode::Failover,
    SwitchMode::Throughput,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PolicyTriggerSummary {
    pub(super) label: SharedString,
    pub(super) tooltip: SharedString,
    pub(super) auto_active: bool,
}

pub(super) fn short_policy_label(group: &OutboundGroup, lang: Language) -> SharedString {
    match (group.switch_mode(), group.kind) {
        (SwitchMode::Off, _) => tr(lang, "手动选择", "Manual"),
        (SwitchMode::Failover, _) => tr(lang, "故障切换", "Failover"),
        (SwitchMode::Latency, _) | (SwitchMode::GroupDefault, GroupKind::UrlTest) => {
            tr(lang, "延迟优先", "Latency")
        }
        (SwitchMode::Throughput, _) => tr(lang, "吞吐优先", "Throughput"),
        (SwitchMode::Health, _) => tr(lang, "健康评分", "Health"),
        _ => tr(lang, "负载均衡", "Balanced"),
    }
}

pub(super) fn mode_choice_label(mode: SwitchMode, lang: Language) -> SharedString {
    match mode {
        SwitchMode::Off => tr(lang, "手动选择", "Manual"),
        SwitchMode::Health => tr(lang, "健康评分", "Health score"),
        SwitchMode::Latency => tr(lang, "延迟优先", "Lowest latency"),
        SwitchMode::Failover => tr(lang, "仅故障切换", "Failover only"),
        SwitchMode::Throughput => tr(lang, "吞吐优先", "Highest throughput"),
        SwitchMode::GroupDefault => tr(lang, "默认策略", "Default policy"),
    }
}

pub(super) fn effective_quick_mode(group: &OutboundGroup) -> SwitchMode {
    match (group.switch_mode(), group.kind) {
        (SwitchMode::GroupDefault, GroupKind::UrlTest) => SwitchMode::Latency,
        (mode, _) => mode,
    }
}

pub(super) fn policy_trigger_summary(
    group: &OutboundGroup,
    group_count: usize,
    lang: Language,
) -> PolicyTriggerSummary {
    let group_name = crate::i18n::outbound_group_label(lang, group);
    let policy = short_policy_label(group, lang);
    let auto_active = !group.manually_selectable();
    let label: SharedString = if group_count > 1 {
        format!(
            "{}: {} · {}",
            tr(lang, "默认", "Default"),
            group_name,
            policy
        )
        .into()
    } else {
        policy.clone()
    };
    let tooltip: SharedString = if group_count > 1 {
        if lang == Language::Zh {
            format!("默认代理出站：{group_name}（{policy}）。点击切换默认出站组或自动切换策略。")
                .into()
        } else {
            format!(
                "Default proxy outbound: {group_name} ({policy}). Click to change default group or selection policy."
            )
            .into()
        }
    } else if auto_active {
        if lang == Language::Zh {
            format!("当前选点策略：{policy}。点击切换为手动选择或调整策略。").into()
        } else {
            format!("Current selection policy: {policy}. Click to switch back to manual or adjust.")
                .into()
        }
    } else {
        tr(
            lang,
            "当前为手动选点；点击开启自动切换或新建出站组",
            "Manual selection active; click to enable auto-switch or manage outbound groups",
        )
    };
    PolicyTriggerSummary {
        label,
        tooltip,
        auto_active,
    }
}

fn auto_trigger_variant(p: &Palette, cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(p.accent)
        .foreground(p.primary)
        .hover(p.primary.opacity(SEMANTIC_ALPHA + 0.06))
        .active(p.primary.opacity(SEMANTIC_ALPHA + 0.12))
}

fn option_variant(selected: bool, p: &Palette, cx: &App) -> ButtonCustomVariant {
    if selected {
        ButtonCustomVariant::new(cx)
            .color(p.accent)
            .foreground(p.primary)
            .hover(p.primary.opacity(SEMANTIC_ALPHA + 0.06))
            .active(p.primary.opacity(SEMANTIC_ALPHA + 0.12))
    } else {
        ButtonCustomVariant::new(cx)
            .color(p.bg)
            .foreground(p.fg)
            .hover(p.neutral_hover())
            .active(p.neutral_active())
    }
}

impl AppView {
    pub(super) fn nodes_policy_popover(
        &self,
        core_busy: bool,
        cx: &mut Context<Self>,
    ) -> Option<Popover> {
        let model = &self.snapshot.model;
        let default_id = model.default_group_id()?;
        let default_group = model.group(&default_id)?.clone();
        let p = palette(cx);
        let lang = self.lang();
        let summary = policy_trigger_summary(&default_group, model.groups.len(), lang);
        let open = self.nodes_policy_popover_open;
        let view = cx.weak_entity();

        let trigger = {
            let mut btn = Button::new("nodes-policy-trigger")
                .compact()
                .h(px(CONTROL_HEIGHT))
                .px_2p5()
                .tooltip(summary.tooltip.clone())
                .accessibility_label(summary.label.clone());
            if summary.auto_active {
                btn = btn.custom(auto_trigger_variant(&p, cx));
            }
            btn.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if summary.auto_active {
                                p.primary
                            } else {
                                p.muted_fg
                            })
                            .max_w(px(220.))
                            .truncate()
                            .child(summary.label),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(14.))
                            .flex_shrink_0()
                            .text_color(if summary.auto_active {
                                p.primary
                            } else {
                                p.muted_fg
                            })
                            .opacity(0.8),
                    ),
            )
        };

        let core = self.snapshot.settings.core;
        let groups_snapshot: Vec<(String, SharedString, bool, bool)> = model
            .groups
            .iter()
            .map(|group| {
                let usable = group.enabled
                    && group.members.iter().any(|id| {
                        model
                            .node(id)
                            .is_some_and(|n| model.node_usable_with(n, core))
                    });
                (
                    group.id.clone(),
                    self.outbound_group_label(group),
                    group.id == default_id,
                    usable,
                )
            })
            .collect();
        let node_view = self.node_view;
        let active_group = default_group.clone();
        let active_group_label = self.outbound_group_label(&default_group);

        Some(
            Popover::new("nodes-policy-popover")
                .anchor(Anchor::TopRight)
                .open(open)
                .trigger(trigger)
                .on_open_change(cx.listener(|this, next_open, _, cx| {
                    if *next_open {
                        this.dismiss_action_menu(cx);
                        this.refresh_snapshot();
                        this.nodes_policy_popover_open = true;
                    } else {
                        this.nodes_policy_popover_open = false;
                    }
                    cx.notify();
                }))
                .content(move |_, _, cx| {
                    Self::nodes_policy_popover_content(
                        &active_group,
                        &active_group_label,
                        &groups_snapshot,
                        node_view,
                        core_busy,
                        lang,
                        view.clone(),
                        cx,
                    )
                }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn nodes_policy_popover_content(
        default_group: &OutboundGroup,
        default_group_label: &SharedString,
        groups: &[(String, SharedString, bool, bool)],
        node_view: NodeView,
        core_busy: bool,
        lang: Language,
        view: gpui_kit::WeakEntity<Self>,
        cx: &App,
    ) -> Div {
        let p = palette(cx);
        let multi_groups = groups.len() > 1;
        let current_mode = effective_quick_mode(default_group);
        let default_id = default_group.id.clone();

        let mut content = div().w(px(304.)).flex().flex_col().gap_3().p_3();

        if multi_groups {
            let group_buttons = groups.iter().map(|(id, label, is_default, usable)| {
                let id = id.clone();
                let view = view.clone();
                Button::new(SharedString::from(format!("policy-default-group-{id}")))
                    .compact()
                    .h(px(28.))
                    .px_2p5()
                    .border_1()
                    .border_color(if *is_default {
                        p.primary.opacity(0.55)
                    } else {
                        p.border
                    })
                    .custom(option_variant(*is_default, &p, cx))
                    .disabled(core_busy || !*usable)
                    .label(label.clone())
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.nodes_policy_popover_open = false;
                            this.run_command(
                                Command::SetDefaultOutbound(id.clone()),
                                false,
                                window,
                                cx,
                            );
                        });
                    })
            });

            content = content
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.fg)
                                .child(tr(lang, "默认代理出站组", "Default proxy outbound")),
                        )
                        .child(div().text_xs().text_color(p.muted_fg).child(tr(
                            lang,
                            "用于未显式指定出站组的代理规则",
                            "Used by proxy rules without an explicit outbound group",
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1p5()
                                .pt_0p5()
                                .children(group_buttons),
                        ),
                )
                .child(divider(&p));
        }

        let mode_heading: SharedString = if multi_groups {
            format!(
                "{} · {}",
                default_group_label,
                tr(lang, "选点策略", "Selection policy")
            )
            .into()
        } else {
            tr(lang, "节点选择模式", "Selection mode")
        };

        let mode_buttons = QUICK_MODES.iter().map(|&mode| {
            let selected = current_mode == mode;
            let group_id = default_id.clone();
            let view = view.clone();
            Button::new(SharedString::from(format!("policy-mode-{mode:?}")))
                .compact()
                .h(px(28.))
                .px_2p5()
                .border_1()
                .border_color(if selected {
                    p.primary.opacity(0.55)
                } else {
                    p.border
                })
                .custom(option_variant(selected, &p, cx))
                .disabled(core_busy)
                .label(mode_choice_label(mode, lang))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.set_group_switch_mode(&group_id, mode, cx);
                    });
                })
        });

        let policy_detail: Option<SharedString> = match current_mode {
            SwitchMode::Off => Some(tr(
                lang,
                "手动点选列表中的节点作为当前代理出口",
                "Click a node in the list to choose the active proxy exit",
            )),
            SwitchMode::Health => {
                let scene = match default_group.switch_policy.health_profile {
                    HealthProfile::General => tr(lang, "综合", "General"),
                    HealthProfile::Ai => tr(lang, "AI 优先", "AI"),
                    HealthProfile::Streaming => tr(lang, "流媒体", "Streaming"),
                    HealthProfile::LowLatency => tr(lang, "低延迟", "Low latency"),
                    HealthProfile::Bulk => tr(lang, "大流量", "Bulk"),
                    HealthProfile::MeshExit => tr(lang, "组网", "Mesh"),
                };
                let sticky = if default_group.switch_policy.sticky_session {
                    tr(lang, " · 粘性会话", " · Sticky")
                } else {
                    "".into()
                };
                Some(format!("{}：{scene}{sticky}", tr(lang, "评估场景", "Profile")).into())
            }
            SwitchMode::Latency => Some(tr(
                lang,
                "按延迟自动选择最优节点，列表单选呈只读状态",
                "Automatically selects the lowest-latency node; row radio is read-only",
            )),
            SwitchMode::Failover => Some(tr(
                lang,
                "仅在连续连通失败时自动回退至同范围可用节点",
                "Switches automatically only after consecutive connectivity failures",
            )),
            SwitchMode::Throughput => Some(tr(
                lang,
                "按下载吞吐优先切换（每轮每节点最多 1 MiB）",
                "Prefers higher download throughput (up to 1 MiB per node per round)",
            )),
            SwitchMode::GroupDefault => Some(tr(
                lang,
                "沿用导入配置中的默认均衡策略",
                "Uses the imported load-balancing strategy",
            )),
        };

        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.fg)
                        .child(mode_heading),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .pt_0p5()
                        .children(mode_buttons),
                )
                .when_some(policy_detail, |d, note| {
                    d.child(div().text_xs().text_color(p.muted_fg).child(note))
                }),
        );

        let left_action = if multi_groups && node_view != NodeView::OutboundGroups {
            let view = view.clone();
            Button::new("policy-open-groups-view")
                .compact()
                .ghost()
                .h(px(28.))
                .label(tr(lang, "切换至出站组视图", "Open groups view"))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.nodes_policy_popover_open = false;
                        this.node_view = NodeView::OutboundGroups;
                        cx.notify();
                    });
                })
        } else {
            let view = view.clone();
            Button::new("policy-new-outbound-group")
                .compact()
                .ghost()
                .h(px(28.))
                .icon(IconName::Plus)
                .label(tr(lang, "新建出站组", "New group"))
                .disabled(core_busy)
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.nodes_policy_popover_open = false;
                        this.open_outbound_group_editor(None, window, cx);
                    });
                })
        };

        let detail_group_id = default_id;
        let detail_button = Button::new("policy-open-switch-details")
            .compact()
            .ghost()
            .h(px(28.))
            .label(tr(lang, "详细参数…", "Details…"))
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.nodes_policy_popover_open = false;
                    this.open_node_switch_dialog(Some(detail_group_id.clone()), window, cx);
                });
            });

        content.child(divider(&p)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(left_action)
                .child(detail_button),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::policy_trigger_summary;
    use xrose_core::{
        model::{GroupKind, OutboundGroup, SwitchMode},
        settings::Language,
    };

    #[test]
    fn single_group_summary_stays_minimal_for_manual_users() {
        let mut group = OutboundGroup::new("代理", GroupKind::Selector);
        group.switch_policy.mode = SwitchMode::Off;

        let zh = policy_trigger_summary(&group, 1, Language::Zh);
        assert_eq!(zh.label, "手动选择");
        assert!(!zh.auto_active);

        let en = policy_trigger_summary(&group, 1, Language::En);
        assert_eq!(en.label, "Manual");
        assert!(!en.auto_active);

        group.switch_policy.mode = SwitchMode::Health;
        let auto = policy_trigger_summary(&group, 1, Language::Zh);
        assert_eq!(auto.label, "健康评分");
        assert!(auto.auto_active);
    }

    #[test]
    fn multi_group_summary_surfaces_default_group_name_and_policy() {
        let mut group = OutboundGroup::new("AI 专线", GroupKind::Selector);
        group.switch_policy.mode = SwitchMode::Health;

        let zh = policy_trigger_summary(&group, 3, Language::Zh);
        assert_eq!(zh.label, "默认: AI 专线 · 健康评分");
        assert!(zh.auto_active);

        let en = policy_trigger_summary(&group, 3, Language::En);
        assert_eq!(en.label, "Default: AI 专线 · Health");
        assert!(en.auto_active);
    }
}
