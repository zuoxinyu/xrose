//! Outbound-group management and node membership editors.

use super::*;
use crate::ui::dialogs::settings_dialog;
use crate::ui::theme::{CONTROL_HEIGHT, FORM_FIELD_WIDTH, MEMBER_LIST_HEIGHT};
use gpui_kit::component::WindowExt;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::InputState;
use gpui_kit::component::radio::Radio;
use std::collections::BTreeSet;
use xross_core::model::{GroupKind, OutboundGroup, SwitchMode};
use xross_core::service::ToastLevel;

struct GroupEditor {
    original: Option<OutboundGroup>,
    draft: OutboundGroup,
    name: Entity<InputState>,
    search: Entity<InputState>,
    advanced: Entity<bool>,
    error: Option<String>,
}

struct MembershipEditor {
    original: BTreeSet<String>,
    selected: BTreeSet<String>,
    error: Option<String>,
}

impl AppView {
    pub(in crate::ui) fn open_outbound_groups(&self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let groups = this.snapshot.model.groups.clone();
                let default = this.snapshot.model.default_group_id();
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().flex().justify_end().child(
                            Button::new("create-outbound-group")
                                .compact()
                                .primary()
                                .icon(IconName::Plus)
                                .label(this.tr("新建出站组", "New group"))
                                .disabled(this_busy(&this.snapshot.busy, "core"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_outbound_group_editor(None, window, cx);
                                })),
                        ),
                    )
                    .children(groups.into_iter().map(|group| {
                        let id = group.id.clone();
                        let label = this.outbound_group_label(&group);
                        let mode = this.outbound_policy_label(&group);
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_2()
                                            .child(div().text_sm().child(label))
                                            .when(default.as_ref() == Some(&group.id), |d| {
                                                d.child(pill(
                                                    this.tr("默认", "Default"),
                                                    p.primary,
                                                    p.accent,
                                                ))
                                            })
                                            .when(!group.enabled, |d| {
                                                d.child(pill(
                                                    this.tr("已禁用", "Disabled"),
                                                    p.muted_fg,
                                                    p.panel_alt,
                                                ))
                                            }),
                                    )
                                    .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                        "{} {} · {}",
                                        group.members.len(),
                                        this.tr("个节点", "nodes"),
                                        mode
                                    ))),
                            )
                            .child(
                                Button::new((ElementId::from("edit-outbound-group"), id.clone()))
                                    .compact()
                                    .label(this.tr("编辑", "Edit"))
                                    .disabled(this_busy(&this.snapshot.busy, "core"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.run_command(
                                            Command::EditOutboundGroup(id.clone()),
                                            false,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                more_button(
                                    SharedString::from(format!("outbound-group-menu-{}", group.id)),
                                    this.tr("出站组操作", "Group actions"),
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::OutboundGroup(group.id),
                                )),
                            )
                    }));
                settings_dialog(
                    dialog,
                    this.tr("出站组", "Outbound groups"),
                    body,
                    &p,
                    window,
                )
            })
        });
    }

    fn outbound_policy_label(&self, group: &OutboundGroup) -> SharedString {
        match (group.switch_mode(), group.kind) {
            (SwitchMode::Off, _) => self.tr("手动选择", "Manual selection"),
            (SwitchMode::Failover, _) => self.tr("仅故障切换", "Failover only"),
            (SwitchMode::Latency, _) | (SwitchMode::GroupDefault, GroupKind::UrlTest) => {
                self.tr("延迟优先", "Lowest latency")
            }
            (SwitchMode::Throughput, _) => self.tr("吞吐优先", "Highest throughput"),
            _ => self.tr("负载均衡", "Load balancing"),
        }
    }

    pub(in crate::ui) fn open_outbound_group_editor(
        &self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let original = id
            .as_ref()
            .and_then(|id| self.snapshot.model.group(id))
            .cloned();
        if id.is_some() && original.is_none() {
            self.service.toast(
                ToastLevel::Error,
                self.tr(
                    "出站组已移除，请刷新后重试",
                    "Group removed. Refresh and try again.",
                )
                .to_string(),
            );
            return;
        }
        let draft = original.clone().unwrap_or_else(|| {
            let mut group = OutboundGroup::new("", GroupKind::Selector);
            group.switch_policy.mode = SwitchMode::Off;
            group
        });
        let name = cx.new(|cx| InputState::new(window, cx).default_value(draft.name.clone()));
        let placeholder = self.tr("搜索成员节点…", "Search member nodes…");
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let advanced = cx.new(|_| false);
        let editor = cx.new(|_| GroupEditor {
            original,
            draft,
            name,
            search,
            advanced,
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
                let body = this.outbound_group_fields(&editor, cx);
                let title = if editor.read(cx).original.is_some() {
                    this.tr("编辑出站组", "Edit outbound group")
                } else {
                    this.tr("新建出站组", "New outbound group")
                };
                let save_editor = editor.clone();
                let profile = profile.clone();
                settings_dialog(dialog, title, body, &p, window)
                    .overlay_closable(false)
                    .footer(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-outbound-group")
                                    .label(this.tr("取消", "Cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("save-outbound-group")
                                    .primary()
                                    .label(this.tr("保存", "Save"))
                                    .disabled(this_busy(&this.snapshot.busy, "core"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let state = save_editor.read(cx);
                                        let mut draft = state.draft.clone();
                                        draft.name = state.name.read(cx).value().to_string();
                                        let result = if this.snapshot.active_profile == profile {
                                            this.service
                                                .save_outbound_group(state.original.as_ref(), draft)
                                        } else {
                                            Err(xross_core::error::Error::config(
                                                "当前配置已切换，请重新打开后编辑",
                                            ))
                                        };
                                        match result {
                                            Ok(()) => {
                                                this.sync(cx);
                                                window.close_dialog(cx);
                                            }
                                            Err(error) => {
                                                save_editor.update(cx, |state, _| {
                                                    state.error = Some(error.to_string())
                                                });
                                                cx.notify();
                                            }
                                        }
                                    })),
                            ),
                    )
            })
        });
    }

    fn outbound_group_fields(&self, editor: &Entity<GroupEditor>, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let state = editor.read(cx);
        let draft = state.draft.clone();
        let advanced = state.advanced.clone();
        let error = state.error.clone();
        let query = state.search.read(cx).value().trim().to_lowercase();
        let busy = this_busy(&self.snapshot.busy, "core");
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_sm().child(self.tr("名称", "Name")))
                    .child(div().w(px(FORM_FIELD_WIDTH)).child(Input::new(&state.name))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_sm().child(format!(
                        "{} · {}",
                        self.tr("成员节点", "Member nodes"),
                        draft.members.len()
                    )))
                    .child(
                        div()
                            .w(px(FORM_FIELD_WIDTH))
                            .child(Input::new(&state.search)),
                    ),
            );
        let members: BTreeSet<_> = draft.members.iter().cloned().collect();
        let nodes: Vec<_> = self
            .snapshot
            .model
            .nodes
            .iter()
            .filter(|node| query.is_empty() || node.name.to_lowercase().contains(&query))
            .map(|node| {
                let name = if self.hide_ips && !node.server.is_empty() {
                    node.name.replace(&node.server, "•••")
                } else {
                    node.name.clone()
                };
                let status = if !node.enabled {
                    self.tr("已禁用", "Disabled")
                } else if !node.usable_with(self.snapshot.settings.core) {
                    self.tr("当前内核不支持", "Unsupported by current core")
                } else {
                    "".into()
                };
                (
                    node.id.clone(),
                    name,
                    status,
                    node.usable_with(self.snapshot.settings.core),
                )
            })
            .collect();
        let list_editor = editor.clone();
        let view = cx.entity().downgrade();
        let preferred = draft.selected.clone();
        let preferred_label = self.tr("首选", "Preferred");
        let list_height = (nodes.len() as f32 * CONTROL_HEIGHT).min(MEMBER_LIST_HEIGHT);
        body = body.child(if nodes.is_empty() {
            empty_hint(&p, self.tr("没有匹配的节点", "No matching nodes")).into_any_element()
        } else {
            uniform_list("outbound-group-members", nodes.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let (id, name, status, usable) = &nodes[index];
                        let selected = members.contains(id);
                        let id = id.clone();
                        let editor = list_editor.clone();
                        let view = view.clone();
                        let preferred_id = id.clone();
                        let preferred_editor = editor.clone();
                        let preferred_view = view.clone();
                        div()
                            .w_full()
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Checkbox::new(("group-member", index))
                                    .label(name.clone())
                                    .flex_1()
                                    .min_w(px(0.))
                                    .checked(selected)
                                    .disabled(busy)
                                    .on_change(move |checked, _, cx| {
                                        editor.update(cx, |state, _| {
                                            if *checked {
                                                state.draft.members.push(id.clone());
                                            } else {
                                                state.draft.members.retain(|member| member != &id);
                                            }
                                            state.draft.repair();
                                            state.error = None;
                                        });
                                        let _ = view.update(cx, |_, cx| cx.notify());
                                    }),
                            )
                            .child(div().text_xs().text_color(p.muted_fg).child(status.clone()))
                            .child(
                                Radio::new(("group-preferred", index))
                                    .label(preferred_label.clone())
                                    .accessibility_label(format!("{} {}", preferred_label, name))
                                    .checked(preferred.as_ref() == Some(&preferred_id))
                                    .disabled(busy || !selected || !usable)
                                    .on_change(move |_, _, cx| {
                                        preferred_editor.update(cx, |state, _| {
                                            state.draft.selected = Some(preferred_id.clone())
                                        });
                                        let _ = preferred_view.update(cx, |_, cx| cx.notify());
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .h(px(list_height))
            .w_full()
            .into_any_element()
        });
        let policy_editor = editor.clone();
        body.child(self.node_policy_fields(
            &draft,
            advanced,
            move |_, policy, interval, cx| {
                policy_editor.update(cx, |state, _| {
                    state.draft.switch_policy = policy;
                    state.draft.interval_secs = interval;
                    state.error = None;
                });
                cx.notify();
            },
            cx,
        ))
        .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
            "保存后应用配置并重启内核生效。",
            "Save, then apply the configuration and restart the core.",
        )))
        .when_some(error, |d, error| {
            d.child(div().text_sm().text_color(p.danger_text).child(error))
        })
    }

    pub(in crate::ui) fn open_node_outbound_groups(
        &self,
        node_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.snapshot.model.node(&node_id) else {
            return;
        };
        let name = if self.hide_ips && !node.server.is_empty() {
            node.name.replace(&node.server, "•••")
        } else {
            node.name.clone()
        };
        let original: BTreeSet<_> = self
            .snapshot
            .model
            .groups
            .iter()
            .filter(|group| group.members.contains(&node_id))
            .map(|group| group.id.clone())
            .collect();
        let editor = cx.new(|_| MembershipEditor {
            selected: original.clone(),
            original,
            error: None,
        });
        let profile = self.snapshot.active_profile.clone();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else { return dialog };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let busy = this_busy(&this.snapshot.busy, "core");
                let state = editor.read(cx);
                let selected = state.selected.clone();
                let error = state.error.clone();
                let body = div().flex().flex_col().gap_3()
                    .child(div().text_sm().child(name.clone()))
                    .children(this.snapshot.model.groups.iter().map(|group| {
                        let id = group.id.clone();
                        let editor = editor.clone();
                        div().flex().items_center().gap_2().min_h(px(CONTROL_HEIGHT))
                            .child(Checkbox::new((ElementId::from("node-membership"), id.clone()))
                                .label(this.outbound_group_label(group)).checked(selected.contains(&id)).disabled(busy)
                                .on_change(cx.listener(move |_, checked: &bool, _, cx| {
                                    editor.update(cx, |state, _| {
                                        if *checked { state.selected.insert(id.clone()); }
                                        else { state.selected.remove(&id); }
                                        state.error = None;
                                    });
                                    cx.notify();
                                })))
                            .child(div().text_xs().text_color(p.muted_fg).child(this.outbound_policy_label(group)))
                    }))
                    .child(div().text_xs().text_color(p.muted_fg).child(this.tr(
                        "可同时加入多个出站组。保存后应用配置并重启内核生效。",
                        "A node can belong to multiple groups. Save, then apply and restart the core.",
                    )))
                    .when_some(error, |d, error| d.child(div().text_sm().text_color(p.danger_text).child(error)));
                let editor = editor.clone();
                let node_id = node_id.clone();
                let profile = profile.clone();
                settings_dialog(dialog, this.tr("所属出站组", "Outbound group membership"), body, &p, window)
                    .overlay_closable(false)
                    .footer(div().flex().justify_end().gap_2()
                        .child(Button::new("cancel-node-memberships").label(this.tr("取消", "Cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("save-node-memberships").primary().label(this.tr("保存", "Save")).disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let state = editor.read(cx);
                                let result = if this.snapshot.active_profile == profile {
                                    this.service.set_node_outbound_groups(&node_id, &state.original, &state.selected)
                                } else {
                                    Err(xross_core::error::Error::config("当前配置已切换，请重新打开后编辑"))
                                };
                                match result {
                                    Ok(()) => { this.sync(cx); window.close_dialog(cx); }
                                    Err(error) => {
                                        editor.update(cx, |state, _| state.error = Some(error.to_string()));
                                        cx.notify();
                                    }
                                }
                            }))))
            })
        });
    }
}
