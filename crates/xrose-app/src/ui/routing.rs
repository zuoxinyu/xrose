use gpui_base::Selectable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt};
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
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self
            .snapshot
            .model
            .routing
            .rule_groups
            .iter()
            .find(|group| group.id == group_id)
        else {
            return;
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(self.tr("分组名称", "Group name"))
                .default_value(group.name.clone())
        });
        let error = cx.new(|_| None::<SharedString>);
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
                    this.tr("重命名分组", "Rename group"),
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
                                .label(this.tr("保存", "Save"))
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
                            let Ok(trimmed) = non_empty_group_name(&value, this.lang()) else {
                                let message = this.tr("请填写分组名称", "Enter a group name");
                                failure.update(cx, |error, cx| {
                                    *error = Some(message);
                                    cx.notify();
                                });
                                cx.notify();
                                return false;
                            };
                            match this.service.save_rule_group(Some(&editing_id), trimmed) {
                                Ok(_) => {
                                    this.route_drag_epoch += 1;
                                    this.sync(cx);
                                    true
                                }
                                Err(err) => {
                                    failure.update(cx, |error, cx| {
                                        *error = Some(SharedString::from(err.to_string()));
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

    pub(super) fn open_inline_rule_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rule_group_inline_open = true;
        self.rule_group_inline_error = None;
        self.rule_group_name_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.rule_group_name_input
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_inline_rule_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rule_group_inline_open = false;
        self.rule_group_inline_error = None;
        self.rule_group_name_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    pub(super) fn submit_inline_rule_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.rule_group_name_input.read(cx).value().to_string();
        let trimmed = match non_empty_group_name(&value, self.lang()) {
            Ok(trimmed) => trimmed,
            Err(message) => {
                self.rule_group_inline_error = Some(message);
                self.rule_group_name_input
                    .focus_handle(cx)
                    .focus(window, cx);
                cx.notify();
                return;
            }
        };
        match self.service.save_rule_group(None, trimmed) {
            Ok(id) => {
                self.rule_group_inline_open = false;
                self.rule_group_inline_error = None;
                self.rule_group_name_input.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                });
                self.route_drag_epoch += 1;
                self.rule_show_all = false;
                self.rule_group_id = Some(id);
                self.sync(cx);
            }
            Err(err) => {
                self.rule_group_inline_error = Some(SharedString::from(err.to_string()));
                self.rule_group_name_input
                    .focus_handle(cx)
                    .focus(window, cx);
                cx.notify();
            }
        }
    }

    pub(crate) fn page_routing(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = self.snapshot.model.clone();
        let rule_sets = self.snapshot.rule_sets();
        let rule_set_updates: std::collections::BTreeMap<_, _> = self
            .snapshot
            .rule_set_updates
            .iter()
            .map(|update| (update.tag.as_str(), update))
            .collect();

        let mut body = div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(self.route_strategy_card(&model, &p, lang, cx))
            .child(self.route_rules_card(&model, &p, lang, cx))
            .child(self.route_rule_sets_card(&rule_sets, &rule_set_updates, &p, cx));

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

        body
    }

    pub(crate) fn allows_rule_drop(&self, group: Option<&str>, cx: &gpui_kit::App) -> bool {
        if !self.rule_search.read(cx).value().trim().is_empty() {
            return false;
        }
        let groups = &self.snapshot.model.routing.rule_groups;
        if groups.is_empty() {
            return group.is_none();
        }
        if self.rule_show_all {
            return true;
        }
        let selected_id = groups
            .iter()
            .find(|g| Some(&g.id) == self.rule_group_id.as_ref())
            .map(|g| g.id.as_str());
        selected_id == group
    }

    fn route_strategy_card(
        &self,
        model: &xrose_core::model::AppModel,
        p: &super::theme::Palette,
        lang: xrose_core::settings::Language,
        cx: &mut Context<Self>,
    ) -> Div {
        let routing = &model.routing;
        card(p)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(section_title(
                        p,
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
                                .h(px(super::theme::CONTROL_HEIGHT))
                                .tooltip(self.tr(
                                    "用于未指定出站组的代理规则",
                                    "Used by proxy rules without an explicit outbound group",
                                ))
                                .child(prefixed_dropdown_content(
                                    self.tr("默认代理出站", "Default proxy outbound"),
                                    name,
                                    p,
                                ))
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::DefaultOutbound,
                                )),
                        )
                    }),
            )
            .when(model.work_mode == xrose_core::model::WorkMode::Mesh, |card| {
                card.child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                    "组网模式下，本地代理分流已暂停。规则会保留，切回代理模式后生效。",
                    "Local proxy routing is paused in Mesh mode. Saved rules apply when you return to Proxy mode.",
                )))
            })
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
                .full_width()
                .tooltips(
                    RoutePreset::all()
                        .iter()
                        .map(|preset| route_preset_description(*preset, lang))
                        .collect(),
                )
                .on_change(cx.listener(|this, index, _, cx| {
                    this.service.set_route_preset(RoutePreset::all()[*index]);
                    this.sync(cx);
                })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_stretch()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_auto()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .min_h(px(40.))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.muted_fg)
                                    .whitespace_nowrap()
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
                                .small()
                                .no_wrap()
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
                    .child(route_bypass_flags_card(
                        routing.bypass_lan,
                        routing.bypass_china_ips,
                        lang,
                        p,
                        cx.listener(|this, checked, _, cx| {
                            let strategy = this.snapshot.model.routing.domain_strategy;
                            let china = this.snapshot.model.routing.bypass_china_ips;
                            this.service.set_routing_flags(*checked, china, strategy);
                            this.sync(cx);
                        }),
                        cx.listener(|this, checked, _, cx| {
                            let strategy = this.snapshot.model.routing.domain_strategy;
                            let lan = this.snapshot.model.routing.bypass_lan;
                            this.service.set_routing_flags(lan, *checked, strategy);
                            this.sync(cx);
                        }),
                    )),
            )
    }

    fn route_rules_card(
        &self,
        model: &xrose_core::model::AppModel,
        p: &super::theme::Palette,
        lang: xrose_core::settings::Language,
        cx: &mut Context<Self>,
    ) -> Div {
        let routing = &model.routing;
        let showing_all = self.rule_show_all;
        let selected_group = if showing_all {
            None
        } else {
            routing
                .rule_groups
                .iter()
                .find(|g| Some(&g.id) == self.rule_group_id.as_ref())
        };
        let enabled_rules = routing
            .rule_groups
            .iter()
            .filter(|g| g.enabled)
            .flat_map(|g| &g.rules)
            .filter(|r| r.enabled)
            .count()
            + routing.rules.iter().filter(|r| r.enabled).count();
        let search_query = self.rule_search.read(cx).value().to_string();
        let is_searching = !search_query.trim().is_empty();
        let rules_menu_busy = self
            .snapshot
            .busy
            .iter()
            .any(|b| b == "route-file" || b == "validate");
        let target_group_id = selected_group.map(|g| g.id.clone());

        struct RuleRowEntry<'a> {
            rule: &'a RouteRule,
            group: Option<(&'a str, &'a str, bool)>,
        }
        let mut entries: Vec<RuleRowEntry<'_>> = if showing_all {
            routing
                .rule_groups
                .iter()
                .flat_map(|group| {
                    group.rules.iter().map(move |rule| RuleRowEntry {
                        rule,
                        group: Some((group.id.as_str(), group.name.as_str(), group.enabled)),
                    })
                })
                .chain(
                    routing
                        .rules
                        .iter()
                        .map(|rule| RuleRowEntry { rule, group: None }),
                )
                .collect()
        } else if let Some(group) = selected_group {
            group
                .rules
                .iter()
                .map(|rule| RuleRowEntry {
                    rule,
                    group: Some((group.id.as_str(), group.name.as_str(), group.enabled)),
                })
                .collect()
        } else {
            routing
                .rules
                .iter()
                .map(|rule| RuleRowEntry { rule, group: None })
                .collect()
        };
        if is_searching {
            entries.retain(|entry| {
                let outbound_label = self.rule_outbound_display(model, &entry.rule.action);
                rule_matches_search(
                    entry.rule,
                    &search_query,
                    entry.group.map(|(_, name, _)| name),
                    outbound_label.as_deref(),
                )
            });
        }

        let mut section = card(p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .flex_wrap()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.fg)
                                            .child(self.tr("路由规则", "Rules")),
                                    )
                                    .child(pill(
                                        format!(
                                            "{enabled_rules} {}",
                                            self.tr("条生效", "active")
                                        ),
                                        p.primary,
                                        p.accent,
                                    )),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.muted_fg)
                                    .child(self.tr(
                                        "按分组顺序匹配，再匹配未分组与预设规则，命中即停",
                                        "Groups are checked in order, then ungrouped and preset rules. The first match wins.",
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .flex_wrap()
                            .ml_auto()
                            .gap_2()
                            .child(
                                more_button(
                                    "route-rules-actions",
                                    self.tr("更多操作", "More actions"),
                                    cx,
                                )
                                .loading(rules_menu_busy)
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::RoutingRules,
                                )),
                            )
                            .child(
                                Button::new("toggle-rule-form")
                                    .compact()
                                    .h(px(super::theme::CONTROL_HEIGHT))
                                    .when(!self.show_rule_form, |b| b.primary())
                                    .icon(if self.show_rule_form {
                                        IconName::X
                                    } else {
                                        IconName::Plus
                                    })
                                    .label(if self.show_rule_form {
                                        self.tr("收起面板", "Collapse")
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
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(self.route_group_tabs(
                        routing,
                        showing_all,
                        selected_group.map(|g| g.id.as_str()),
                        p,
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(196.))
                            .max_w_full()
                            .child(
                                Input::new(&self.rule_search)
                                    .small()
                                    .h(px(32.))
                                    .bg(p.bg)
                                    .border_color(p.border)
                                    .text_xs()
                                    .cleanable(true)
                                    .prefix(
                                        Icon::new(IconName::Search)
                                            .size(px(13.))
                                            .text_color(p.muted_fg),
                                    ),
                            ),
                    ),
            )
            .when_some(
                self.rule_group_inline_open
                    .then_some(self.rule_group_inline_error.clone())
                    .flatten(),
                |d, message| d.child(div().text_xs().text_color(p.danger_text).child(message)),
            )
            .when_some(self.snapshot.route_import.clone(), |d, (id, report)| {
                d.child(self.route_import_review(id, &report, cx))
            })
            .when(self.show_rule_form, |d| {
                d.child(self.route_rule_composer(model, target_group_id.clone(), p, lang, cx))
            });

        if entries.is_empty() {
            let empty_msg = if is_searching {
                self.tr("没有匹配的规则", "No rules match your filter")
            } else if selected_group.is_some() {
                self.tr("此分组暂无规则", "No rules in this group")
            } else {
                self.tr("当前模板没有额外规则", "No extra rules in this preset")
            };
            section = section.child(empty_hint(p, empty_msg));
        } else {
            for entry in entries {
                section = section.child(self.route_rule_row(
                    model,
                    entry.rule,
                    entry.group,
                    showing_all,
                    !is_searching,
                    p,
                    cx,
                ));
            }
        }

        section
    }

    fn inline_rule_group_editor(
        &self,
        p: &super::theme::Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let input_border = if self.rule_group_inline_error.is_some() {
            p.danger
        } else {
            p.primary
        };
        div()
            .id("inline-rule-group")
            .flex()
            .items_center()
            .gap_0p5()
            .h(px(26.))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    this.cancel_inline_rule_group(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div().w(px(140.)).h(px(26.)).child(
                    Input::new(&self.rule_group_name_input)
                        .small()
                        .h(px(26.))
                        .bg(p.panel)
                        .rounded(px(3.))
                        .border_color(input_border)
                        .text_xs()
                        .focus_bordered(false),
                ),
            )
            .child(
                icon_button(
                    "save-inline-rule-group",
                    IconName::Check,
                    self.tr("保存分组", "Save group"),
                    cx,
                )
                .w(px(24.))
                .h(px(24.))
                .rounded(ButtonRounded::Size(px(3.)))
                .custom(icon_button_style(p, true, cx))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.submit_inline_rule_group(window, cx);
                })),
            )
            .child(
                icon_button(
                    "cancel-inline-rule-group",
                    IconName::X,
                    self.tr("取消", "Cancel"),
                    cx,
                )
                .w(px(24.))
                .h(px(24.))
                .rounded(ButtonRounded::Size(px(3.)))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.cancel_inline_rule_group(window, cx);
                })),
            )
            .into_any_element()
    }

    fn route_group_tabs(
        &self,
        routing: &xrose_core::model::RoutingConfig,
        showing_all: bool,
        selected_group_id: Option<&str>,
        p: &super::theme::Palette,
        cx: &mut Context<Self>,
    ) -> SlidingTabTrack {
        let total_rules: usize = routing
            .rule_groups
            .iter()
            .map(|g| g.rules.len())
            .sum::<usize>()
            + routing.rules.len();
        let transparent = cx.theme().transparent;
        let tab_style = |active: bool, cx: &gpui_kit::App| {
            gpui_kit::component::button::ButtonCustomVariant::new(cx)
                .color(transparent)
                .foreground(if active { p.primary } else { p.muted_fg })
                .hover(if active {
                    transparent
                } else {
                    p.neutral_hover()
                })
                .active(if active {
                    transparent
                } else {
                    p.neutral_active()
                })
                .shadow(false)
        };
        let ungrouped_active = !showing_all && selected_group_id.is_none();

        let slots = route_group_tab_slots(routing.rule_groups.len(), self.rule_group_inline_open);
        let mut items: Vec<gpui_kit::AnyElement> = Vec::with_capacity(slots.len());
        let mut selected_item = None;

        for slot in slots {
            match slot {
                RouteGroupTabSlot::All => {
                    if showing_all {
                        selected_item = Some(items.len());
                    }
                    let all_variant = tab_style(showing_all, cx);
                    items.push(
                        Button::new("all-rules")
                            .role(Role::Tab)
                            .compact()
                            .h(px(26.))
                            .rounded(ButtonRounded::Size(px(3.)))
                            .custom(all_variant)
                            .selected(showing_all)
                            .when(showing_all, |b| b.font_weight(FontWeight::MEDIUM))
                            .border_1()
                            .border_color(transparent)
                            .label(format!("{} · {total_rules}", self.tr("全部", "All")))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route_drag_epoch += 1;
                                this.rule_show_all = true;
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
                RouteGroupTabSlot::Divider => {
                    items.push(
                        div()
                            .w(px(1.))
                            .h(px(14.))
                            .mx_1()
                            .flex_shrink_0()
                            .bg(p.border)
                            .into_any_element(),
                    );
                }
                RouteGroupTabSlot::Custom(index) => {
                    let group = &routing.rule_groups[index];
                    let id = group.id.clone();
                    let name = group.name.clone();
                    let active = !showing_all && selected_group_id == Some(id.as_str());
                    if active {
                        selected_item = Some(items.len());
                    }
                    let menu_target = MenuTarget::Group(id.clone());
                    let tab_label = if group.enabled {
                        format!("{name} · {}", group.rules.len())
                    } else {
                        format!(
                            "{name} ({}) · {}",
                            self.tr("停用", "Off"),
                            group.rules.len()
                        )
                    };
                    let hover = p.neutral_hover();
                    let select_variant = gpui_kit::component::button::ButtonCustomVariant::new(cx)
                        .color(transparent)
                        .foreground(if active { p.primary } else { p.muted_fg })
                        .hover(transparent)
                        .active(transparent)
                        .shadow(false);
                    let tab = div()
                        .id(SharedString::from(format!("group-tab-{id}")))
                        .role(Role::Tab)
                        .flex()
                        .items_center()
                        .h(px(26.))
                        .pr_0p5()
                        .rounded(px(3.))
                        .border_1()
                        .border_color(transparent)
                        .when(!active, |d| d.hover(move |s| s.bg(hover)))
                        .when(!group.enabled && !active, |d| {
                            d.opacity(super::theme::DISABLED_OPACITY)
                        })
                        .child(route_drag::handle(
                            RouteDrag {
                                id: id.clone(),
                                name,
                                group: None,
                                groups: true,
                                epoch: self.route_drag_epoch,
                            },
                            cx.entity().downgrade(),
                        ))
                        .child(
                            Button::new(SharedString::from(format!("select-{id}")))
                                .compact()
                                .h(px(24.))
                                .custom(select_variant)
                                .when(active, |b| b.font_weight(FontWeight::MEDIUM))
                                .label(tab_label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.route_drag_epoch += 1;
                                    this.rule_show_all = false;
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
                            .h(px(22.))
                            .w(px(22.))
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                menu_target.clone(),
                            )),
                        );
                    items.push(
                        route_drag::target(tab, group.id.clone(), None, true, self, cx)
                            .context_menu(menus::builder(cx.entity().downgrade(), menu_target))
                            .into_any_element(),
                    );
                }
                RouteGroupTabSlot::InlineInput => {
                    items.push(self.inline_rule_group_editor(p, cx));
                }
                RouteGroupTabSlot::Ungrouped => {
                    if ungrouped_active {
                        selected_item = Some(items.len());
                    }
                    let ungrouped_variant = tab_style(ungrouped_active, cx);
                    items.push(
                        Button::new("ungrouped-rules")
                            .role(Role::Tab)
                            .compact()
                            .h(px(26.))
                            .rounded(ButtonRounded::Size(px(3.)))
                            .custom(ungrouped_variant)
                            .selected(ungrouped_active)
                            .when(ungrouped_active, |b| b.font_weight(FontWeight::MEDIUM))
                            .border_1()
                            .border_color(transparent)
                            .label(format!(
                                "{} · {}",
                                self.tr("未分组 / 预设", "Ungrouped / preset"),
                                routing.rules.len()
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.route_drag_epoch += 1;
                                this.rule_show_all = false;
                                this.rule_group_id = None;
                                this.sync(cx);
                            }))
                            .into_any_element(),
                    );
                }
                RouteGroupTabSlot::AddButton => {
                    items.push(
                        icon_button(
                            "create-rule-group",
                            IconName::Plus,
                            self.tr(
                                "添加分组：新分组排在已有自定义分组之后、「未分组 / 预设」之前，按分组顺序优先匹配",
                                "Add group: appended after custom groups and before ungrouped / preset rules; matched in group order",
                            ),
                            cx,
                        )
                        .w(px(26.))
                        .h(px(26.))
                        .rounded(ButtonRounded::Size(px(3.)))
                        .accessibility_label(self.tr("添加分组", "Add group"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_inline_rule_group(window, cx);
                        }))
                        .into_any_element(),
                    );
                }
            }
        }

        SlidingTabTrack::new("route-group-tabs", selected_item).children(items)
    }

    fn route_rule_composer(
        &self,
        model: &xrose_core::model::AppModel,
        target_group_id: Option<String>,
        p: &super::theme::Palette,
        lang: xrose_core::settings::Language,
        cx: &mut Context<Self>,
    ) -> Div {
        let kinds = MatcherKind::all();
        let action_kinds = [
            RouteActionKind::Proxy,
            RouteActionKind::Direct,
            RouteActionKind::Block,
        ];
        let mono = cx.theme().mono_font_family.clone();
        let outbound_label = match &self.rule_outbound_group {
            Some(id) => model
                .group(id)
                .map(|group| self.outbound_group_label(group))
                .unwrap_or_else(|| self.tr("请重新选择", "Select again")),
            None => self.tr("默认出站组", "Default outbound group"),
        };
        let field_label = |text: SharedString| {
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.muted_fg)
                .child(text)
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded_md()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(field_label(self.tr("规则名称", "Name")))
                    .child(
                        div()
                            .w(px(280.))
                            .max_w_full()
                            .child(Input::new(&self.rule_name)),
                    ),
            )
            .child(
                div().flex().w_full().child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .max_w_full()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(field_label(self.tr("匹配维度", "Matcher")))
                                .child(
                                    ChoiceGroup::new(
                                        "rule-matcher",
                                        kinds.iter().map(|kind| kind.label(lang)).collect(),
                                        kinds.iter().position(|kind| *kind == self.matcher_kind),
                                    )
                                    .small()
                                    .on_change(cx.listener(
                                        move |this, index, window, cx| {
                                            this.matcher_kind = kinds[*index];
                                            let placeholder =
                                                this.matcher_kind.placeholder(this.lang());
                                            this.rule_value.update(cx, |input, cx| {
                                                input.set_placeholder(placeholder, window, cx)
                                            });
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .w_full()
                                .child(field_label(self.tr("匹配内容", "Pattern")))
                                .child(
                                    div().flex_1().min_w(px(0.)).child(
                                        Input::new(&self.rule_value).prefix(
                                            div()
                                                .pr_2()
                                                .border_r_1()
                                                .border_color(p.border)
                                                .text_xs()
                                                .font_family(mono)
                                                .text_color(p.muted_fg)
                                                .child(self.matcher_kind.prefix()),
                                        ),
                                    ),
                                ),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(field_label(self.tr("执行动作", "Action")))
                            .child(
                                ChoiceGroup::new(
                                    "rule-action",
                                    vec![
                                        self.tr("代理", "Proxy"),
                                        self.tr("直连", "Direct"),
                                        self.tr("阻止", "Block"),
                                    ],
                                    action_kinds
                                        .iter()
                                        .position(|kind| *kind == self.rule_action),
                                )
                                .small()
                                .on_change(cx.listener(
                                    move |this, index, _, cx| {
                                        this.rule_action = action_kinds[*index];
                                        cx.notify();
                                    },
                                )),
                            )
                            .when(self.rule_action == RouteActionKind::Proxy, |d| {
                                d.child(
                                    Button::new("rule-outbound-group")
                                        .compact()
                                        .h(px(super::theme::CONTROL_HEIGHT))
                                        .tooltip(outbound_label.clone())
                                        .child(prefixed_dropdown_content(
                                            self.tr("出站目标", "Outbound"),
                                            outbound_label.clone(),
                                            p,
                                        ))
                                        .dropdown_menu(menus::builder(
                                            cx.entity().downgrade(),
                                            MenuTarget::RuleOutbound,
                                        )),
                                )
                            })
                            .when(self.rule_action != RouteActionKind::Proxy, |d| {
                                d.child(div().text_xs().text_color(p.muted_fg).child(
                                    match self.rule_action {
                                        RouteActionKind::Direct => self.tr(
                                            "不经代理，直接由本地网络发出",
                                            "Send directly without proxy",
                                        ),
                                        RouteActionKind::Block => self.tr(
                                            "直接拒绝或丢弃匹配连接",
                                            "Reject or drop matching connections",
                                        ),
                                        RouteActionKind::Proxy => unreachable!(),
                                    },
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                Button::new("cancel-rule-form")
                                    .compact()
                                    .ghost()
                                    .h(px(super::theme::CONTROL_HEIGHT))
                                    .label(self.tr("取消", "Cancel"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_rule_form = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("save-rule")
                                    .compact()
                                    .h(px(super::theme::CONTROL_HEIGHT))
                                    .primary()
                                    .label(self.tr("保存规则", "Save rule"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let name = this.rule_name.read(cx).value().to_string();
                                        let raw = this.rule_value.read(cx).value().to_string();
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
                                            MatcherKind::Domain => matchers.domains = values,
                                            MatcherKind::Suffix => {
                                                matchers.domain_suffixes = values
                                            }
                                            MatcherKind::Geo => matchers.geosites = values,
                                            MatcherKind::RuleSet => {
                                                let known = this.snapshot.rule_sets();
                                                if values
                                                    .iter()
                                                    .any(|v| !known.iter().any(|s| &s.tag == v))
                                                {
                                                    this.service.toast(
                                                        xrose_core::service::ToastLevel::Warning,
                                                        this.tr(
                                                            "请填写已列出的规则集标签",
                                                            "Enter a listed rule set tag",
                                                        )
                                                        .to_string(),
                                                    );
                                                    return;
                                                }
                                                matchers.rule_sets = values;
                                            }
                                            MatcherKind::Ip => matchers.ip_cidrs = values,
                                            MatcherKind::Process => matchers.process_names = values,
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
                                                group: if this.rule_action == RouteActionKind::Proxy
                                                {
                                                    this.rule_outbound_group.clone()
                                                } else {
                                                    None
                                                },
                                            },
                                        );
                                        if let Err(error) = this
                                            .service
                                            .upsert_rule_in_group(rule, target_group_id.as_deref())
                                        {
                                            this.service.toast(
                                                xrose_core::service::ToastLevel::Error,
                                                error.to_string(),
                                            );
                                            this.sync(cx);
                                            return;
                                        }
                                        this.rule_name.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        this.rule_value.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        this.show_rule_form = false;
                                        this.sync(cx);
                                    })),
                            ),
                    ),
            )
    }

    fn rule_outbound_display(
        &self,
        model: &xrose_core::model::AppModel,
        action: &RouteAction,
    ) -> Option<String> {
        if action.kind != RouteActionKind::Proxy {
            return None;
        }
        action
            .group
            .as_ref()
            .and_then(|id| model.group(id))
            .or_else(|| model.default_group_id().and_then(|id| model.group(&id)))
            .filter(|group| group.name != "代理" && !group.name.eq_ignore_ascii_case("proxy"))
            .map(|group| group.name.clone())
    }

    #[allow(clippy::too_many_arguments)]
    fn route_rule_row(
        &self,
        model: &xrose_core::model::AppModel,
        rule: &RouteRule,
        group: Option<(&str, &str, bool)>,
        showing_all: bool,
        allow_reorder: bool,
        p: &super::theme::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = rule.id.clone();
        let toggle_id = id.clone();
        let group_id = group.map(|(gid, _, _)| gid.to_string());
        let group_disabled = group.is_some_and(|(_, _, enabled)| !enabled);
        let menu_target = MenuTarget::Rule(id.clone(), group_id.clone());
        let summary = rule.matchers.summary().replace(',', ", ");
        let mono = cx.theme().mono_font_family.clone();
        let hover_bg = p.neutral_hover();
        let (fg, bg) = match rule.action.kind {
            RouteActionKind::Proxy => (p.primary, p.accent),
            RouteActionKind::Direct => (p.muted_fg, p.panel_alt),
            RouteActionKind::Block => (
                p.danger_text,
                p.danger.opacity(super::theme::SEMANTIC_ALPHA),
            ),
        };
        let action_label = match rule.action.kind {
            RouteActionKind::Direct => self.tr("直连", "Direct").to_string(),
            RouteActionKind::Block => self.tr("阻止", "Block").to_string(),
            RouteActionKind::Proxy => {
                let label = self.tr("代理", "Proxy");
                self.rule_outbound_display(model, &rule.action)
                    .map(|name| format!("{label} → {name}"))
                    .unwrap_or_else(|| label.to_string())
            }
        };

        let row = div()
            .id(SharedString::from(format!("rule-{id}")))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .w_full()
            .min_w(px(0.))
            .rounded_md()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .hover(move |s| s.bg(hover_bg))
            .when(allow_reorder, |d| {
                d.child(route_drag::handle(
                    RouteDrag {
                        id: id.clone(),
                        name: rule.name.clone(),
                        group: group_id.clone(),
                        groups: false,
                        epoch: self.route_drag_epoch,
                    },
                    cx.entity().downgrade(),
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .flex_1()
                    .min_w(px(0.))
                    .when(!rule.enabled || group_disabled, |d| {
                        d.opacity(super::theme::DISABLED_OPACITY)
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.fg)
                                    .truncate()
                                    .child(rule.name.clone()),
                            )
                            .when_some(
                                showing_all.then_some(group).flatten(),
                                |d, (_, group_name, _)| {
                                    d.child(pill(group_name.to_string(), p.muted_fg, p.panel_alt))
                                },
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("rule-summary-{id}")))
                            .text_xs()
                            .font_family(mono)
                            .text_color(p.muted_fg)
                            .truncate()
                            .child(summary.clone())
                            .tooltip(move |window, cx| {
                                Tooltip::new(summary.clone()).build(window, cx)
                            }),
                    ),
            )
            .child(pill(action_label, fg, bg))
            .child(
                more_button(
                    SharedString::from(format!("rule-menu-{id}")),
                    self.tr("规则操作", "Rule actions"),
                    cx,
                )
                .dropdown_menu(menus::builder(cx.entity().downgrade(), menu_target.clone())),
            )
            .child(
                toggle(
                    SharedString::from(format!("rule-enabled-{id}")),
                    "",
                    rule.enabled,
                )
                .accessibility_label(self.tr("启用规则", "Enable rule"))
                .tooltip(self.tr("启用此规则参与匹配", "Include this rule in matching"))
                .on_change(cx.listener(move |this, checked, _, cx| {
                    this.service.set_rule_enabled(&toggle_id, *checked);
                    this.sync(cx);
                })),
            );

        if allow_reorder {
            route_drag::target(row, id, group_id, false, self, cx)
                .context_menu(menus::builder(cx.entity().downgrade(), menu_target))
        } else {
            row.context_menu(menus::builder(cx.entity().downgrade(), menu_target))
        }
    }

    fn route_rule_sets_card(
        &self,
        rule_sets: &[xrose_core::model::RuleSet],
        rule_set_updates: &std::collections::BTreeMap<&str, &xrose_core::updates::RuleSetUpdate>,
        p: &super::theme::Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        card(p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(section_title(
                        p,
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
                                    .h(px(super::theme::CONTROL_HEIGHT))
                                    .label(self.tr("更新全部规则集", "Update all rule sets"))
                                    .loading(self.snapshot.busy.iter().any(|b| b == "rulesets"))
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.sync_rule_sets()
                                    })),
                            ),
                    ),
            )
            .children(rule_sets.iter().map(|set| {
                self.rule_set_row(set, rule_set_updates.get(set.tag.as_str()).copied(), p)
            }))
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

fn route_preset_description(
    preset: xrose_core::model::RoutePreset,
    lang: xrose_core::settings::Language,
) -> SharedString {
    use xrose_core::model::RoutePreset;
    let (zh, en) = match preset {
        RoutePreset::BypassMainland => (
            "拦截广告、直连中国大陆域名与 IP，境外网站默认代理",
            "Block ads, direct China Mainland domains & IPs, proxy foreign traffic",
        ),
        RoutePreset::GfwList => (
            "内置 GFWList 代理、广告拦截与大陆直连，搭配自由扩展规则",
            "Built-in GFWList proxy, ad-block & China direct starter rules",
        ),
        RoutePreset::Global => (
            "不启用预设直连与域名分流规则，按默认出站全局转发",
            "Clear starter bypass rules and forward traffic via default outbound",
        ),
        RoutePreset::ProxyApps => (
            "内置进程示例规则，适合按指定应用或自定义分组定向代理",
            "Starter process rules tailored for per-app and custom group routing",
        ),
        RoutePreset::Custom => (
            "保留当前分组与自定义规则顺序，由规则列表完全决定分流",
            "Keep custom group and rule order as the active routing policy",
        ),
    };
    crate::i18n::tr(lang, zh, en)
}

fn rule_matches_search(
    rule: &xrose_core::model::RouteRule,
    query: &str,
    group_name: Option<&str>,
    outbound_label: Option<&str>,
) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let summary = rule.matchers.summary().to_lowercase();
    let action_terms = match rule.action.kind {
        xrose_core::model::RouteActionKind::Proxy => "proxy 代理",
        xrose_core::model::RouteActionKind::Direct => "direct 直连",
        xrose_core::model::RouteActionKind::Block => "block 阻止",
    };
    rule.name.to_lowercase().contains(&q)
        || summary.contains(&q)
        || group_name.is_some_and(|name| name.to_lowercase().contains(&q))
        || outbound_label.is_some_and(|name| name.to_lowercase().contains(&q))
        || action_terms.contains(&q)
}

fn non_empty_group_name<'a>(
    value: &'a str,
    lang: xrose_core::settings::Language,
) -> Result<&'a str, SharedString> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(crate::i18n::tr(
            lang,
            "请填写分组名称",
            "Enter a group name",
        ))
    } else {
        Ok(trimmed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteGroupTabSlot {
    All,
    Divider,
    Custom(usize),
    InlineInput,
    Ungrouped,
    AddButton,
}

fn route_group_tab_slots(group_count: usize, inline_open: bool) -> Vec<RouteGroupTabSlot> {
    let mut slots = Vec::with_capacity(group_count + 5);
    slots.push(RouteGroupTabSlot::All);
    slots.push(RouteGroupTabSlot::Divider);
    for index in 0..group_count {
        slots.push(RouteGroupTabSlot::Custom(index));
    }
    if inline_open {
        slots.push(RouteGroupTabSlot::InlineInput);
    }
    if group_count > 0 || inline_open {
        slots.push(RouteGroupTabSlot::Divider);
    }
    slots.push(RouteGroupTabSlot::Ungrouped);
    if !inline_open {
        slots.push(RouteGroupTabSlot::Divider);
        slots.push(RouteGroupTabSlot::AddButton);
    }
    slots
}

fn route_bypass_flags_card(
    bypass_lan: bool,
    bypass_china_ips: bool,
    lang: xrose_core::settings::Language,
    p: &super::theme::Palette,
    on_lan_change: impl Fn(&bool, &mut Window, &mut gpui_kit::App) + 'static,
    on_china_change: impl Fn(&bool, &mut Window, &mut gpui_kit::App) + 'static,
) -> Div {
    div()
        .flex_auto()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .min_h(px(40.))
        .px_2p5()
        .py_1()
        .rounded_md()
        .bg(p.bg)
        .border_1()
        .border_color(p.border)
        .child(route_flag_switch_item(
            "route-bypass-lan-box",
            "bypass-lan",
            crate::i18n::tr(lang, "局域网直连", "Bypass LAN"),
            crate::i18n::tr(lang, "私有网段绕过代理", "Bypass private subnets"),
            bypass_lan,
            p,
            on_lan_change,
        ))
        .child(div().w(px(1.)).h(px(16.)).flex_shrink_0().bg(p.border))
        .child(route_flag_switch_item(
            "route-bypass-china-box",
            "bypass-china",
            crate::i18n::tr(lang, "中国大陆 IP 直连", "Bypass China IPs"),
            crate::i18n::tr(lang, "geoip:cn 自动直连", "Direct for geoip:cn"),
            bypass_china_ips,
            p,
            on_china_change,
        ))
}

fn route_flag_switch_item(
    id: &'static str,
    switch_id: &'static str,
    label: SharedString,
    hint: SharedString,
    checked: bool,
    p: &super::theme::Palette,
    on_change: impl Fn(&bool, &mut Window, &mut gpui_kit::App) + 'static,
) -> gpui_kit::Stateful<Div> {
    div()
        .id(id)
        .flex_auto()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg)
                .whitespace_nowrap()
                .child(label.clone()),
        )
        .child(
            toggle(switch_id, "", checked)
                .accessibility_label(label)
                .on_change(on_change),
        )
}

#[cfg(test)]
mod tests {
    use super::{
        MatcherKind, RouteGroupTabSlot, non_empty_group_name, route_group_tab_slots,
        route_preset_description, rule_matches_search,
    };
    use xrose_core::model::{Matchers, RouteAction, RoutePreset, RouteRule};
    use xrose_core::settings::Language;

    #[test]
    fn group_name_validation_trims_and_rejects_blank() {
        assert!(non_empty_group_name("   ", Language::Zh).is_err());
        assert!(non_empty_group_name("", Language::En).is_err());
        assert_eq!(
            non_empty_group_name("  Streaming  ", Language::Zh),
            Ok("Streaming")
        );
    }

    #[test]
    fn group_tab_slots_place_inline_input_before_ungrouped_and_add_button_at_end() {
        assert_eq!(
            route_group_tab_slots(0, false),
            vec![
                RouteGroupTabSlot::All,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Ungrouped,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::AddButton,
            ]
        );
        assert_eq!(
            route_group_tab_slots(2, false),
            vec![
                RouteGroupTabSlot::All,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Custom(0),
                RouteGroupTabSlot::Custom(1),
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Ungrouped,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::AddButton,
            ]
        );
        assert_eq!(
            route_group_tab_slots(0, true),
            vec![
                RouteGroupTabSlot::All,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::InlineInput,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Ungrouped,
            ]
        );
        assert_eq!(
            route_group_tab_slots(2, true),
            vec![
                RouteGroupTabSlot::All,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Custom(0),
                RouteGroupTabSlot::Custom(1),
                RouteGroupTabSlot::InlineInput,
                RouteGroupTabSlot::Divider,
                RouteGroupTabSlot::Ungrouped,
            ]
        );
    }

    #[test]
    fn matcher_kind_prefixes_and_placeholders_cover_all_variants() {
        for kind in MatcherKind::all() {
            assert!(kind.prefix().ends_with(':'));
            for lang in [Language::Zh, Language::En] {
                assert!(!kind.label(lang).is_empty());
                assert!(!kind.placeholder(lang).is_empty());
            }
        }
    }

    #[test]
    fn preset_descriptions_are_non_empty_for_all_presets() {
        for preset in RoutePreset::all() {
            for lang in [Language::Zh, Language::En] {
                assert!(!route_preset_description(preset, lang).is_empty());
            }
        }
    }

    #[test]
    fn rule_search_matches_name_matcher_summary_group_and_action() {
        let mut matchers = Matchers::default();
        matchers.domain_suffixes = vec!["openai.com".into()];
        let rule = RouteRule::new("AI 访问", matchers, RouteAction::proxy_via("g-us"));

        assert!(rule_matches_search(
            &rule,
            "",
            Some("自定义"),
            Some("美国代理")
        ));
        assert!(rule_matches_search(
            &rule,
            "openai",
            Some("自定义"),
            Some("美国代理")
        ));
        assert!(rule_matches_search(&rule, "ai", None, None));
        assert!(rule_matches_search(
            &rule,
            "美国代理",
            None,
            Some("美国代理")
        ));
        assert!(rule_matches_search(&rule, "自定义", Some("自定义"), None));
        assert!(!rule_matches_search(
            &rule,
            "google",
            Some("自定义"),
            Some("美国代理")
        ));
    }
}
