use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{
    DomainStrategy, Matchers, RouteAction, RouteActionKind, RoutePreset, RouteRule,
};

use super::menus::{self, MenuTarget};
use super::route_drag::{self, RouteDrag};
use super::theme::palette;
use super::widgets::*;
use super::{AppView, MatcherKind};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tooltip::Tooltip;

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
        let mut body = div().flex().flex_col().gap_4().w_full();

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
                .child(
                    ChoiceGroup::new(
                        "route-preset",
                        RoutePreset::all()
                            .iter()
                            .map(|preset| self.route_preset_label(*preset))
                            .collect(),
                        RoutePreset::all()
                            .iter()
                            .position(|preset| *preset == routing.preset)
                            .unwrap_or(0),
                    )
                    .cards()
                    .on_change(cx.listener(|this, index, _, cx| {
                        this.service.set_route_preset(RoutePreset::all()[*index]);
                        this.sync(cx);
                    })),
                )
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
                                    ChoiceGroup::new(
                                        "domain-strategy",
                                        vec![
                                            self.tr("保持域名", "Keep domain"),
                                            self.tr("优先 IPv4", "Prefer IPv4"),
                                            self.tr("优先 IPv6", "Prefer IPv6"),
                                            self.tr("未匹配时解析", "Resolve if unmatched"),
                                        ],
                                        [
                                            DomainStrategy::AsIs,
                                            DomainStrategy::PreferIpv4,
                                            DomainStrategy::PreferIpv6,
                                            DomainStrategy::IpIfNonMatch,
                                        ]
                                        .iter()
                                        .position(|v| *v == routing.domain_strategy)
                                        .unwrap_or(0),
                                    )
                                    .on_change(cx.listener(
                                        |this, index, _, cx| {
                                            let route = &this.snapshot.model.routing;
                                            this.service.set_routing_flags(
                                                route.bypass_lan,
                                                route.bypass_china_ips,
                                                [
                                                    DomainStrategy::AsIs,
                                                    DomainStrategy::PreferIpv4,
                                                    DomainStrategy::PreferIpv6,
                                                    DomainStrategy::IpIfNonMatch,
                                                ][*index],
                                            );
                                            this.sync(cx);
                                        },
                                    )),
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
                        .children(routing.rule_groups.iter().map(|group| {
                            let id = group.id.clone();
                            let name = group.name.clone();
                            let menu_target = MenuTarget::Group(id.clone());
                            let row = div()
                                .id(SharedString::from(format!("group-tab-{id}")))
                                .flex()
                                .items_center()
                                .rounded_md()
                                .border_1()
                                .border_color(if self.rule_group_id.as_deref() == Some(&id) {
                                    p.primary
                                } else {
                                    p.border
                                })
                                .bg(if self.rule_group_id.as_deref() == Some(&id) {
                                    p.accent
                                } else {
                                    p.bg
                                })
                                .child(route_drag::handle(
                                    RouteDrag {
                                        id: id.clone(),
                                        name: name.clone(),
                                        group: None,
                                        groups: true,
                                        epoch: self.route_drag_epoch,
                                    },
                                    cx.entity().downgrade(),
                                ))
                                .child(
                                    Button::new(SharedString::from(format!("select-{id}")))
                                        .compact()
                                        .ghost()
                                        .label(format!("{} · {}", name, group.rules.len()))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.route_drag_epoch += 1;
                                            this.rule_group_id = Some(id.clone());
                                            this.rule_group_name.update(cx, |input, cx| {
                                                input.set_value(name.clone(), window, cx)
                                            });
                                            this.sync(cx);
                                        })),
                                )
                                .child(
                                    more_button(
                                        SharedString::from(format!("group-menu-{}", group.id)),
                                        self.tr("分组操作", "Group actions"),
                                    )
                                    .dropdown_menu(
                                        menus::builder(
                                            cx.entity().downgrade(),
                                            menu_target.clone(),
                                        ),
                                    ),
                                );
                            route_drag::target(row, group.id.clone(), None, true, self, cx)
                                .context_menu(menus::builder(cx.entity().downgrade(), menu_target))
                        }))
                        .child(
                            Button::new("ungrouped-rules")
                                .compact()
                                .label(self.tr("未分组 / 预设", "Ungrouped / preset"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.route_drag_epoch += 1;
                                    this.rule_group_id = None;
                                    this.sync(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(super::theme::FORM_FIELD_WIDTH))
                                .child(Input::new(&self.rule_group_name)),
                        )
                        .child(
                            Button::new("create-rule-group")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                            d.child(
                                Button::new("rename-rule-group")
                                    .compact()
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("校验", "Validate"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "validate"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.validate_config();
                                        })),
                                )
                                .child(
                                    Button::new("toggle-rule-form")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                    let id = rule.id.clone();
                    let toggle_id = id.clone();
                    let menu_target = MenuTarget::Rule(id.clone(), self.rule_group_id.clone());
                    let summary = rule.matchers.summary().replace(',', ", ");
                    let (fg, bg) = match rule.action.kind {
                        RouteActionKind::Proxy => (p.primary, p.accent),
                        RouteActionKind::Direct => (p.muted_fg, p.panel_alt),
                        RouteActionKind::Block => (
                            p.danger_text,
                            p.danger.opacity(super::theme::SEMANTIC_ALPHA),
                        ),
                    };
                    let row = div()
                        .id(SharedString::from(format!("rule-{id}")))
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .w_full()
                        .min_w(px(0.))
                        .rounded_md()
                        .bg(p.bg)
                        .border_1()
                        .border_color(p.border)
                        .child(route_drag::handle(
                            RouteDrag {
                                id: id.clone(),
                                name: rule.name.clone(),
                                group: self.rule_group_id.clone(),
                                groups: false,
                                epoch: self.route_drag_epoch,
                            },
                            cx.entity().downgrade(),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .flex_1()
                                .min_w(px(0.))
                                .when(!rule.enabled, |d| d.opacity(super::theme::DISABLED_OPACITY))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .truncate()
                                        .child(rule.name.clone()),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("rule-summary-{id}")))
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .truncate()
                                        .child(summary.clone())
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(summary.clone()).build(window, cx)
                                        }),
                                ),
                        )
                        .child(pill(
                            match rule.action.kind {
                                RouteActionKind::Direct => self.tr("直连", "Direct").to_string(),
                                RouteActionKind::Block => self.tr("阻止", "Block").to_string(),
                                RouteActionKind::Proxy => {
                                    let label = self.tr("代理", "Proxy");
                                    rule.action
                                        .group
                                        .as_ref()
                                        .and_then(|id| model.group(id))
                                        .or_else(|| {
                                            model.default_group_id().and_then(|id| model.group(&id))
                                        })
                                        .filter(|group| {
                                            group.name != "代理"
                                                && !group.name.eq_ignore_ascii_case("proxy")
                                        })
                                        .map(|group| format!("{label} → {}", group.name))
                                        .unwrap_or_else(|| label.to_string())
                                }
                            },
                            fg,
                            bg,
                        ))
                        .child(
                            more_button(
                                SharedString::from(format!("rule-menu-{id}")),
                                self.tr("规则操作", "Rule actions"),
                            )
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                menu_target.clone(),
                            )),
                        )
                        .child(
                            toggle(
                                SharedString::from(format!("rule-enabled-{id}")),
                                "",
                                rule.enabled,
                            )
                            .accessibility_label(self.tr("启用规则", "Enable rule"))
                            .tooltip(self.tr("启用此规则参与匹配", "Include this rule in matching"))
                            .on_change(cx.listener(
                                move |this, checked, _, cx| {
                                    this.service.set_rule_enabled(&toggle_id, *checked);
                                    this.sync(cx);
                                },
                            )),
                        );
                    route_drag::target(row, id, self.rule_group_id.clone(), false, self, cx)
                        .context_menu(menus::builder(cx.entity().downgrade(), menu_target))
                })),
        );

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
                                .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
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

        let _ = lang;
        body
    }
}
