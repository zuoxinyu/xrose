use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{
    DomainStrategy, Matchers, RouteAction, RouteActionKind, RoutePreset, RouteRule,
};
use xross_core::route::action_label;

use super::theme::palette;
use super::widgets::*;
use super::{AppView, MatcherKind};

impl AppView {
    pub(crate) fn page_routing(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = self.snapshot.model.clone();
        let routing = model.routing.clone();
        let selected_group = routing
            .rule_groups
            .iter()
            .find(|g| Some(&g.id) == self.rule_group_id.as_ref());
        let visible_rules = selected_group.map(|g| &g.rules).unwrap_or(&routing.rules);
        let rule_sets = self.snapshot.rule_sets();
        let dirty = self.snapshot.config_dirty;
        let running = self.snapshot.process.phase.is_alive();

        let mut body = div().flex().flex_col().gap_4().w_full();

        if dirty {
            body = body.child(
                card(&p).border_color(p.warning).child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.fg)
                                        .child(self.tr(
                                            "路由已修改，尚未生效",
                                            "Routing changed, not applied",
                                        )),
                                )
                                .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                                    "应用路由变更将重启内核以生效",
                                    "Applying routing changes will restart the core.",
                                ))),
                        )
                        .child(
                            Button::new("routing-apply")
                                .compact()
                                .warning()
                                .label(self.tr("应用配置", "Apply"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.service.apply_config();
                                })),
                        ),
                ),
            );
        }

        // Presets.
        body = body.child(
            card(&p)
                .child(section_title(
                    &p,
                    self.tr("路由模板", "Routing presets"),
                    self.tr(
                        "选择预设分流模式，或配置自定义分流规则",
                        "Select a routing preset or configure custom rules",
                    ),
                ))
                .child(div().flex().flex_wrap().gap_2().children(
                    RoutePreset::all().into_iter().map(|preset| {
                        let selected = routing.preset == preset;
                        div()
                            .id(SharedString::from(format!("preset-{preset:?}")))
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(if selected { p.primary } else { p.border })
                            .bg(if selected { p.accent } else { p.bg })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.set_route_preset(preset);
                                this.sync(cx);
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if selected { p.fg } else { p.muted_fg })
                                    .child(self.route_preset_label(preset)),
                            )
                    }),
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    toggle(
                                        "bypass-lan",
                                        self.tr("局域网直连", "Bypass LAN"),
                                        routing.bypass_lan,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            let strategy =
                                                this.snapshot.model.routing.domain_strategy;
                                            let china =
                                                this.snapshot.model.routing.bypass_china_ips;
                                            this.service
                                                .set_routing_flags(*checked, china, strategy);
                                            this.sync(cx);
                                        },
                                    )),
                                )
                                .child(
                                    toggle(
                                        "bypass-china",
                                        self.tr("中国大陆 IP 直连", "Bypass China IPs"),
                                        routing.bypass_china_ips,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            let strategy =
                                                this.snapshot.model.routing.domain_strategy;
                                            let lan = this.snapshot.model.routing.bypass_lan;
                                            this.service.set_routing_flags(lan, *checked, strategy);
                                            this.sync(cx);
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("域名策略", "Domain strategy")),
                                )
                                .child(
                                    button_group(&p).children(
                                        [
                                            (DomainStrategy::AsIs, "AsIs"),
                                            (DomainStrategy::PreferIpv4, "IPv4"),
                                            (DomainStrategy::PreferIpv6, "IPv6"),
                                            (DomainStrategy::IpIfNonMatch, "IPIfNonMatch"),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(strategy, label)| {
                                                let selected = routing.domain_strategy == strategy;
                                                button_group_item(
                                                    &p,
                                                    SharedString::from(format!("strategy-{label}")),
                                                    label,
                                                    selected,
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    let model = &this.snapshot.model.routing;
                                                    this.service.set_routing_flags(
                                                        model.bypass_lan,
                                                        model.bypass_china_ips,
                                                        strategy,
                                                    );
                                                    this.sync(cx);
                                                }))
                                            },
                                        ),
                                    ),
                                ),
                        ),
                ),
        );

        // Custom groups survive preset changes and precede the ungrouped rules.
        body = body.child(
            card(&p)
                .child(section_title(
                    &p,
                    self.tr("规则分组", "Rule groups"),
                    self.tr(
                        "自定义分组按顺序优先匹配，随后匹配未分组与预设规则",
                        "Custom groups run in order before ungrouped and preset rules",
                    ),
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("ungrouped-rules")
                                .compact()
                                .label(self.tr("未分组 / 预设", "Ungrouped / preset"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.rule_group_id = None;
                                    this.sync(cx);
                                })),
                        )
                        .children(routing.rule_groups.iter().map(|group| {
                            let id = group.id.clone();
                            let name = group.name.clone();
                            Button::new(SharedString::from(format!("select-{}", group.id)))
                                .compact()
                                .label(format!(
                                    "{} · {}{}",
                                    group.name,
                                    group.rules.len(),
                                    if group.enabled { "" } else { " (—)" }
                                ))
                                .when(self.rule_group_id.as_deref() == Some(&group.id), |b| {
                                    b.primary()
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.rule_group_id = Some(id.clone());
                                    this.rule_group_name.update(cx, |input, cx| {
                                        input.set_value(name.clone(), window, cx)
                                    });
                                    this.sync(cx);
                                }))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(Input::new(&self.rule_group_name)),
                        )
                        .child(
                            Button::new("create-rule-group")
                                .compact()
                                .label(self.tr("新建分组", "New group"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let name = this.rule_group_name.read(cx).value().to_string();
                                    match this.service.save_rule_group(None, &name) {
                                        Ok(id) => this.rule_group_id = Some(id),
                                        Err(err) => this.service.toast(
                                            xross_core::service::ToastLevel::Error,
                                            err.to_string(),
                                        ),
                                    }
                                    this.sync(cx);
                                })),
                        )
                        .when_some(selected_group, |d, group| {
                            let id = group.id.clone();
                            let toggle_id = id.clone();
                            let up_id = id.clone();
                            let down_id = id.clone();
                            let delete_id = id.clone();
                            d.child(
                                Button::new("rename-rule-group")
                                    .compact()
                                    .label(self.tr("重命名", "Rename"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let name =
                                            this.rule_group_name.read(cx).value().to_string();
                                        if let Err(err) =
                                            this.service.save_rule_group(Some(&id), &name)
                                        {
                                            this.service.toast(
                                                xross_core::service::ToastLevel::Error,
                                                err.to_string(),
                                            );
                                        }
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                toggle(
                                    "enable-rule-group",
                                    self.tr("启用分组", "Enable group"),
                                    group.enabled,
                                )
                                .on_change(cx.listener(
                                    move |this, value, _, cx| {
                                        this.service.set_rule_group_enabled(&toggle_id, *value);
                                        this.sync(cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new("group-up")
                                    .compact()
                                    .label(self.tr("上移", "Move up"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.move_rule_group(&up_id, -1);
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                Button::new("group-down")
                                    .compact()
                                    .label(self.tr("下移", "Move down"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.move_rule_group(&down_id, 1);
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                Button::new("group-remove")
                                    .compact()
                                    .label(self.tr("解散分组", "Ungroup rules"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.delete_rule_group(&delete_id);
                                        this.rule_group_id = None;
                                        this.sync(cx);
                                    })),
                            )
                        }),
                ),
        );

        // Rules.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("路由规则", "Rules"),
                            self.tr(
                                "按顺序匹配，命中即停",
                                "Evaluated top-down, first match wins",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("validate-route")
                                        .compact()
                                        .label(self.tr("校验", "Validate"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "validate"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.validate_config();
                                        })),
                                )
                                .child(
                                    Button::new("toggle-rule-form")
                                        .compact()
                                        .primary()
                                        .icon(if self.show_rule_form {
                                            IconName::X
                                        } else {
                                            IconName::Plus
                                        })
                                        .label(if self.show_rule_form {
                                            self.tr("收起", "Close")
                                        } else {
                                            self.tr("添加规则", "Add rule")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_rule_form = !this.show_rule_form;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .when(self.show_rule_form, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_3()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(div().w(px(180.)).child(Input::new(&self.rule_name)))
                                    .child(div().flex_1().child(Input::new(&self.rule_value))),
                            )
                            .child(
                                div().flex().items_center().gap_2().flex_wrap().children(
                                    [
                                        MatcherKind::Domain,
                                        MatcherKind::Suffix,
                                        MatcherKind::Geo,
                                        MatcherKind::Ip,
                                        MatcherKind::Process,
                                        MatcherKind::Port,
                                    ]
                                    .into_iter()
                                    .map(|kind| {
                                        let selected = self.matcher_kind == kind;
                                        div()
                                            .id(SharedString::from(format!("matcher-{kind:?}")))
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .text_xs()
                                            .border_1()
                                            .border_color(if selected {
                                                p.primary
                                            } else {
                                                p.border
                                            })
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.matcher_kind = kind;
                                                cx.notify();
                                            }))
                                            .child(kind.label(lang))
                                    }),
                                ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(p.muted_fg)
                                                    .child(self.tr("动作", "Action")),
                                            )
                                            .children(
                                                [
                                                    (
                                                        RouteActionKind::Proxy,
                                                        self.tr("代理", "Proxy"),
                                                    ),
                                                    (
                                                        RouteActionKind::Direct,
                                                        self.tr("直连", "Direct"),
                                                    ),
                                                    (
                                                        RouteActionKind::Block,
                                                        self.tr("阻止", "Block"),
                                                    ),
                                                ]
                                                .into_iter()
                                                .map(|(kind, label)| {
                                                    let selected = self.rule_action == kind;
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "rule-action-{kind:?}"
                                                        )))
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_md()
                                                        .text_xs()
                                                        .border_1()
                                                        .border_color(if selected {
                                                            p.primary
                                                        } else {
                                                            p.border
                                                        })
                                                        .bg(if selected { p.accent } else { p.bg })
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.rule_action = kind;
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child(label)
                                                }),
                                            ),
                                    )
                                    .child(
                                        Button::new("save-rule")
                                            .compact()
                                            .primary()
                                            .label(self.tr("保存规则", "Save rule"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let name =
                                                    this.rule_name.read(cx).value().to_string();
                                                let raw =
                                                    this.rule_value.read(cx).value().to_string();
                                                let values: Vec<String> = raw
                                                    .split(',')
                                                    .map(|s| s.trim().to_string())
                                                    .filter(|s| !s.is_empty())
                                                    .collect();
                                                if values.is_empty() {
                                                    this.service.toast(
                                                        xross_core::service::ToastLevel::Warning,
                                                        "请填写匹配值",
                                                    );
                                                    return;
                                                }
                                                let mut matchers = Matchers::default();
                                                match this.matcher_kind {
                                                    MatcherKind::Domain => {
                                                        matchers.domains = values
                                                    }
                                                    MatcherKind::Suffix => {
                                                        matchers.domain_suffixes = values
                                                    }
                                                    MatcherKind::Geo => matchers.geosites = values,
                                                    MatcherKind::Ip => matchers.ip_cidrs = values,
                                                    MatcherKind::Process => {
                                                        matchers.process_names = values
                                                    }
                                                    MatcherKind::Port => matchers.ports = values,
                                                }
                                                let rule = RouteRule::new(
                                                    if name.trim().is_empty() {
                                                        "自定义规则".into()
                                                    } else {
                                                        name
                                                    },
                                                    matchers,
                                                    RouteAction {
                                                        kind: this.rule_action,
                                                        group: None,
                                                    },
                                                );
                                                this.service.upsert_rule_in_group(
                                                    rule,
                                                    this.rule_group_id.as_deref(),
                                                );
                                                this.show_rule_form = false;
                                                this.sync(cx);
                                            })),
                                    ),
                            ),
                    )
                })
                .when(visible_rules.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr("当前模板没有额外规则", "No extra rules in this preset"),
                    ))
                })
                .children(visible_rules.iter().map(|rule| {
                    let rule_id = rule.id.clone();
                    let rule_id_toggle = rule.id.clone();
                    let rule_id_delete = rule.id.clone();
                    let enabled = rule.enabled;
                    let up_id = rule.id.clone();
                    let down_id = rule.id.clone();
                    let assign_id = rule.id.clone();
                    div()
                        .id(SharedString::from(format!("rule-{}", rule.id)))
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .w_full()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .rounded_md()
                        .bg(p.bg)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div().flex_shrink_0().child(
                                toggle(
                                    SharedString::from(format!("rule-enabled-{}", rule.id)),
                                    "",
                                    enabled,
                                )
                                .on_change(cx.listener(
                                    move |this, checked, _, cx| {
                                        this.service.set_rule_enabled(&rule_id_toggle, *checked);
                                        this.sync(cx);
                                    },
                                )),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .flex_1()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(if enabled { p.fg } else { p.muted_fg })
                                        .truncate()
                                        .child(rule.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .line_clamp(2)
                                        .child(rule.matchers.summary().replace(',', ", ")),
                                ),
                        )
                        .child(div().flex_shrink_0().child(pill(
                            action_label(&model, &rule.action),
                            p.primary_fg,
                            match rule.action.kind {
                                RouteActionKind::Proxy => p.primary,
                                RouteActionKind::Direct => p.success,
                                RouteActionKind::Block => p.danger,
                            },
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!("up-{}", rule.id)))
                                        .compact()
                                        .label("↑")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.move_rule(
                                                &up_id,
                                                this.rule_group_id.as_deref(),
                                                -1,
                                            );
                                            this.sync(cx);
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("down-{}", rule.id)))
                                        .compact()
                                        .label("↓")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.move_rule(
                                                &down_id,
                                                this.rule_group_id.as_deref(),
                                                1,
                                            );
                                            this.sync(cx);
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("assign-{}", rule.id)))
                                        .compact()
                                        .label(self.tr("分组", "Group"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.rule_move_id = Some(assign_id.clone());
                                            this.sync(cx);
                                        })),
                                ),
                        )
                        .child(
                            div().flex_shrink_0().child(
                                danger_icon_button(
                                    SharedString::from(format!("delete-rule-{}", rule_id)),
                                    IconName::Trash,
                                    self.tr("删除规则", "Delete rule"),
                                    &p,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.service.delete_rule(&rule_id_delete);
                                        this.sync(cx);
                                    },
                                )),
                            ),
                        )
                })),
        );

        if let Some(rule_id) = self.rule_move_id.clone() {
            let ungroup_id = rule_id.clone();
            body = body.child(
                card(&p)
                    .child(section_title(
                        &p,
                        self.tr("移动规则", "Move rule"),
                        self.tr("选择目标分组", "Choose a destination"),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("move-ungrouped")
                                    .compact()
                                    .label(self.tr("未分组", "Ungrouped"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.assign_rule_group(&ungroup_id, None);
                                        this.rule_move_id = None;
                                        this.sync(cx);
                                    })),
                            )
                            .children(routing.rule_groups.iter().map(|group| {
                                let id = group.id.clone();
                                let rule_id = rule_id.clone();
                                Button::new(SharedString::from(format!("move-to-{}", id)))
                                    .compact()
                                    .label(group.name.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.service.assign_rule_group(&rule_id, Some(&id));
                                        this.rule_move_id = None;
                                        this.sync(cx);
                                    }))
                            }))
                            .child(
                                Button::new("cancel-rule-move")
                                    .compact()
                                    .label(self.tr("取消", "Cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.rule_move_id = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }

        // Rule sets / geo data.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("规则集 / Geo 数据", "Rule sets / Geo data"),
                            self.tr(
                                "管理路由与 DNS 规则引用的本地规则集与 Geo 数据",
                                "Manage local rule-sets and Geo data referenced by routing and DNS rules",
                            ),
                        ))
                        .child(
                            Button::new("sync-rulesets")
                                .compact()
                                .label(self.tr("更新规则集", "Update rule-sets"))
                                .loading(self.snapshot.busy.iter().any(|b| b == "rulesets"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.service.sync_rule_sets();
                                })),
                        ),
                )
                .children(rule_sets.iter().map(|set| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .bg(p.bg)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_sm()
                                        .text_color(p.fg)
                                        .truncate()
                                        .child(set.tag.clone()),
                                )
                                .child(
                                    div().flex_shrink_0().child(pill(
                                        set.format.as_str(),
                                        p.muted_fg,
                                        p.panel_alt,
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted_fg)
                                .truncate()
                                .child(set.url.clone()),
                        )
                }))
                .when(!self.snapshot.rule_set_updates.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(self.snapshot.rule_set_updates.iter().map(|update| {
                                div()
                                    .text_xs()
                                    .text_color(if update.status == "failed" { p.danger } else { p.muted_fg })
                                    .child(format!(
                                        "{} · {} · {}",
                                        update.tag,
                                        update.status,
                                        update.error.clone().unwrap_or_else(|| format!("{} B", update.bytes))
                                    ))
                            })),
                    )
                }),
        );

        // Validation / warnings.
        if let Some(validation) = &self.snapshot.validation {
            if !validation.ok || !self.snapshot.compiled_warnings.is_empty() {
                body = body.child(
                    card(&p)
                        .border_color(if validation.ok { p.warning } else { p.danger })
                        .child(section_title(
                            &p,
                            self.tr("编译与校验", "Compile & validation"),
                            validation.summary(),
                        ))
                        .children(validation.issues.iter().map(|issue| {
                            div()
                                .text_xs()
                                .text_color(p.danger)
                                .child(match issue.line {
                                    Some(line) => format!("第 {line} 行：{}", issue.message),
                                    None => issue.message.clone(),
                                })
                        }))
                        .children(self.snapshot.compiled_warnings.iter().map(|warning| {
                            div().text_xs().text_color(p.warning).child(warning.clone())
                        })),
                );
            }
        }

        let _ = (lang, running);
        body
    }
}
