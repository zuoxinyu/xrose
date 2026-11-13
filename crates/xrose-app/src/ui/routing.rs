use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{
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
    pub(super) fn open_rule_set_source_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let lang = self.lang();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://rules.example/{tag}.srs")
                .default_value(self.snapshot.model.routing.custom_rule_set_template.clone())
        });
        let error = cx.new(|_| None::<String>);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let p = palette(cx);
            let tr = |zh, en| super::tr(lang, zh, en);
            let form = div().flex().flex_col().gap_2().pb_1()
                .child(tr("下载目录或 URL 模板", "Download directory or URL template"))
                .child(Input::new(&input))
                .child(div().text_xs().text_color(p.muted_fg).child(tr(
                    "目录下按 {tag}.srs 下载。模板可使用 {tag}（geoip-cn）、{kind}（geoip）和 {name}（cn）。单独指定地址的规则集保留原来源。",
                    "A directory serves {tag}.srs. Templates accept {tag} (geoip-cn), {kind} (geoip), and {name} (cn). Individually configured URLs are kept.",
                )))
                .when_some(error.read(cx).clone(), |d, e| d.child(form_feedback(e, false, &p)));
            let (input, error, view) = (input.clone(), error.clone(), view.clone());
            let focus = input.read(cx).focus_handle(cx);
            super::dialogs::settings_dialog(dialog, tr("自定义规则源", "Custom rule set source"), form, &p, window)
                .footer(div().flex().justify_end().gap_2()
                    .child(Button::new("rule-set-source-cancel").compact().label(tr("取消","Cancel")).on_click(|_,w,cx| w.close_dialog(cx)))
                    .child(Button::new("rule-set-source-save").compact().primary().label(tr("保存并使用","Save and use")).on_click(move |_, w, cx| {
                        focus.dispatch_action(&gpui_kit::component::dialog::Confirm { secondary: false }, w, cx);
                    })))
                .on_ok(move |_, _, cx| view.update(cx, |this, cx| {
                    let value = input.read(cx).value();
                    let result = xrose_core::model::route::rule_set_source_template(&value)
                        .map_err(|_| this.tr("请输入有效的 HTTP(S) 目录地址或模板。变量仅可用于路径，地址不能包含认证信息、查询参数或片段。",
                            "Enter a valid HTTP(S) directory or template. Use placeholders only in the path and omit credentials, query strings, and fragments.").to_string())
                        .and_then(|template| this.service.set_rule_set_source(xrose_core::model::RuleSetSource::Custom, Some(&template)).map_err(|e| e.to_string()));
                    match result {
                        Ok(()) => { this.sync(cx); true }
                        Err(e) => { error.update(cx, |error, cx| { *error = Some(e); cx.notify(); }); cx.notify(); false }
                    }
                }).unwrap_or(false))
        });
    }

    pub(super) fn open_rule_group_dialog(
        &self,
        group_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = group_id.as_ref().and_then(|id| {
            self.snapshot
                .model
                .routing
                .rule_groups
                .iter()
                .find(|group| &group.id == id)
        });
        if group_id.is_some() && group.is_none() {
            return;
        }
        let editing = group_id.is_some();
        let name = cx.new(|cx| {
            let mut input =
                InputState::new(window, cx).placeholder(self.tr("分组名称", "Group name"));
            if let Some(group) = group {
                input.set_value(group.name.clone(), window, cx);
            }
            input
        });
        let error = cx.new(|_| None::<String>);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let input = name.clone();
                let input_focus = name.read(cx).focus_handle(cx);
                let failure = error.clone();
                let editing_id = group_id.clone();
                let save_view = cx.entity().downgrade();
                super::dialogs::settings_dialog(
                    dialog,
                    if editing {
                        this.tr("重命名分组", "Rename group")
                    } else {
                        this.tr("添加分组", "Add group")
                    },
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pb_1()
                        .child(Input::new(&name))
                        .when_some(error.read(cx).clone(), |d, message| {
                            d.child(div().text_sm().text_color(p.danger_text).child(message))
                        }),
                    &p,
                    window,
                )
                .w(px(super::theme::DIALOG_WIDTH))
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-rule-group")
                                .compact()
                                .label(this.tr("取消", "Cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save-rule-group")
                                .compact()
                                .primary()
                                .label(if editing {
                                    this.tr("保存", "Save")
                                } else {
                                    this.tr("添加分组", "Add group")
                                })
                                .on_click(move |_, window, cx| {
                                    input_focus.dispatch_action(
                                        &gpui_kit::component::dialog::Confirm { secondary: false },
                                        window,
                                        cx,
                                    );
                                }),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    save_view
                        .update(cx, |this, cx| {
                            let value = input.read(cx).value().to_string();
                            if value.trim().is_empty() {
                                let message =
                                    this.tr("请填写分组名称", "Enter a group name").to_string();
                                failure.update(cx, |error, cx| {
                                    *error = Some(message);
                                    cx.notify();
                                });
                                cx.notify();
                                return false;
                            }
                            match this.service.save_rule_group(editing_id.as_deref(), &value) {
                                Ok(id) => {
                                    this.route_drag_epoch += 1;
                                    if editing_id.is_none() {
                                        this.rule_group_id = Some(id);
                                    }
                                    this.sync(cx);
                                    true
                                }
                                Err(err) => {
                                    failure.update(cx, |error, cx| {
                                        *error = Some(err.to_string());
                                        cx.notify();
                                    });
                                    cx.notify();
                                    false
                                }
                            }
                        })
                        .unwrap_or(false)
                })
            })
        });
    }

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
        let rule_set_updates: std::collections::BTreeMap<_, _> = self
            .snapshot
            .rule_set_updates
            .iter()
            .map(|update| (update.tag.as_str(), update))
            .collect();
        let mut body = div().flex().flex_col().gap_4().w_full();

        // Presets.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(section_title(
                            &p,
                            self.tr("路由模板", "Routing presets"),
                            self.tr(
                                "选择预设分流模式，或配置自定义分流规则",
                                "Select a routing preset or configure custom rules",
                            ),
                        ))
                        .when(model.groups.len() > 1, |d| {
                            let name = model
                                .default_group_id()
                                .and_then(|id| model.group(&id))
                                .map(|g| self.outbound_group_label(g))
                                .unwrap_or_else(|| self.tr("选择出站组", "Choose group"));
                            d.child(
                                Button::new("route-default-outbound")
                                    .compact()
                                    .label(format!(
                                        "{} · {}",
                                        self.tr("默认代理出站", "Default proxy group"),
                                        name
                                    ))
                                    .icon(IconName::ChevronDown)
                                    .tooltip(self.tr(
                                        "用于未指定出站组的代理规则",
                                        "Used by proxy rules without an explicit outbound group",
                                    ))
                                    .dropdown_menu(menus::builder(
                                        cx.entity().downgrade(),
                                        MenuTarget::DefaultOutbound,
                                    )),
                            )
                        }),
                )
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
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap_3()
                        .child(
                            toggle(
                                "bypass-lan",
                                self.tr("局域网直连", "Bypass LAN"),
                                routing.bypass_lan,
                            )
                            .on_change(cx.listener(
                                |this, checked, _, cx| {
                                    let strategy = this.snapshot.model.routing.domain_strategy;
                                    let china = this.snapshot.model.routing.bypass_china_ips;
                                    this.service.set_routing_flags(*checked, china, strategy);
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
                                    let strategy = this.snapshot.model.routing.domain_strategy;
                                    let lan = this.snapshot.model.routing.bypass_lan;
                                    this.service.set_routing_flags(lan, *checked, strategy);
                                    this.sync(cx);
                                },
                            )),
                        ),
                ),
        );

        // Rules.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(section_title(
                            &p,
                            self.tr("路由规则", "Rules"),
                            self.tr(
                                "按分组顺序匹配，再匹配未分组与预设规则，命中即停",
                                "Groups are checked in order, then ungrouped and preset rules. The first match wins.",
                            ),
                        )))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .flex_wrap()
                                .ml_auto()
                                .gap_2()
                                .child(
                                    Button::new("route-files").compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("导入 / 导出", "Import / Export"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "route-file"))
                                        .dropdown_menu(menus::builder(cx.entity().downgrade(), MenuTarget::RoutingFiles)),
                                )
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
                                    Button::new("create-rule-group")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .icon(IconName::Plus)
                                        .label(self.tr("添加分组", "Add group"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_rule_group_dialog(None, window, cx);
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.route_drag_epoch += 1;
                                            this.rule_group_id = Some(id.clone());
                                            this.sync(cx);
                                        })),
                                )
                                .child(
                                    more_button(
                                        SharedString::from(format!("group-menu-{}", group.id)),
                                        self.tr("分组操作", "Group actions"),
                                        cx,
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
                .when_some(self.snapshot.route_import.clone(), |d, (id, report)| {
                    d.child(self.route_import_review(id, &report, cx))
                })
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
                                        MatcherKind::RuleSet,
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
                                                        xrose_core::service::ToastLevel::Warning,
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
                                                    MatcherKind::RuleSet => {
                                                        let known = this.snapshot.rule_sets();
                                                        if values.iter().any(|v| !known.iter().any(|s| &s.tag == v)) {
                                                            this.service.toast(xrose_core::service::ToastLevel::Warning,
                                                                this.tr("请填写已列出的规则集标签", "Enter a listed rule set tag").to_string());
                                                            return;
                                                        }
                                                        matchers.rule_sets = values;
                                                    }
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
                        if selected_group.is_some() {
                            self.tr("此分组暂无规则", "No rules in this group")
                        } else {
                            self.tr("当前模板没有额外规则", "No extra rules in this preset")
                        },
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
                                cx,
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
                                "可在任意路由模式下更新内置模板、路由规则和 DNS 引用的规则集",
                                "Update rule sets for built-in presets, routing rules and DNS in any routing mode",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .child(
                                    more_button(
                                        "rule-set-sources",
                                        self.tr("切换规则源", "Change rule set source"),
                                        cx,
                                    )
                                    .dropdown_menu(menus::builder(
                                        cx.entity().downgrade(),
                                        MenuTarget::RuleSetSources,
                                    )),
                                )
                                .child(
                                    Button::new("sync-rulesets")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("更新全部规则集", "Update all rule sets"))
                                        .loading(self.snapshot.busy.iter().any(|b| b == "rulesets"))
                                        .on_click(cx.listener(|this, _, _, _| this.service.sync_rule_sets())),
                                ),
                        ),
                )
                .children(rule_sets.iter().map(|set| {
                    self.rule_set_row(set, rule_set_updates.get(set.tag.as_str()).copied(), &p)
                })),
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

    fn rule_set_row(
        &self,
        set: &xrose_core::model::RuleSet,
        update: Option<&xrose_core::updates::RuleSetUpdate>,
        p: &super::theme::Palette,
    ) -> Div {
        let timestamp = update
            .and_then(|u| u.updated_at)
            .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            });
        let last_updated = match timestamp {
            Some(time) => format!("{} {time}", self.tr("最后更新", "Last updated")),
            None => self.tr("尚未更新", "Never updated").to_string(),
        };
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
                            .min_w_0()
                            .text_sm()
                            .text_color(p.fg)
                            .truncate()
                            .child(set.tag.clone()),
                    )
                    .child(pill(set.format.as_str(), p.muted_fg, p.panel_alt)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .truncate()
                    .child(set.url.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .text_xs()
                    .child(div().text_color(p.muted_fg).child(last_updated))
                    .when_some(update, |row, update| {
                        let (label, color) = match update.status.as_str() {
                            "updated" => (
                                format!(
                                    "{} · {}",
                                    self.tr("已更新", "Updated"),
                                    xrose_core::util::format_bytes(update.bytes)
                                ),
                                p.ok_text,
                            ),
                            "not-modified" => {
                                (self.tr("已是最新", "Up to date").to_string(), p.muted_fg)
                            }
                            "failed" => (
                                format!(
                                    "{} · {}",
                                    self.tr("更新失败", "Update failed"),
                                    update.error.as_deref().unwrap_or_default()
                                ),
                                p.danger_text,
                            ),
                            _ => return row,
                        };
                        row.child(div().text_color(color).child(label))
                    }),
            )
    }
}
