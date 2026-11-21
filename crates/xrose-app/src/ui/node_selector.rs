//! The status capsule's keyboard-accessible node menu.
use super::{AppView, menus::Command, theme};
use gpui_base::actions::{Confirm, SelectDown, SelectUp};
use gpui_base::{Scrollbar, ScrollbarMode};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, ThemeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::WorkMode;

struct Choice {
    label: SharedString,
    checked: bool,
    command: Option<Command>,
}

pub(super) struct NodeSelector {
    owner: WeakEntity<AppView>,
    choices: Vec<Choice>,
    focus: FocusHandle,
    highlighted: Option<usize>,
    scroll: ScrollHandle,
    scrollbar_mode: Option<ScrollbarMode>,
}

impl AppView {
    pub(super) fn node_selector_popover(&self, trigger: Button, cx: &mut Context<Self>) -> Popover {
        let menu = self.node_selector.clone();
        Popover::new("header-node-menu")
            .appearance(false)
            .open(menu.is_some())
            .trigger(trigger)
            .on_open_change(cx.listener(|this, open, window, cx| {
                if *open {
                    this.dismiss_action_menu(cx);
                    this.refresh_snapshot();
                    if !this.snapshot.busy.iter().any(|key| key == "core") {
                        let choices = this.node_choices();
                        let owner = cx.weak_entity();
                        this.node_selector =
                            Some(cx.new(|cx| NodeSelector::new(owner, choices, window, cx)));
                    }
                } else {
                    this.node_selector = None;
                }
                cx.notify();
            }))
            .content(move |_, _, _| div().children(menu.clone()))
    }

    fn node_choices(&self) -> Vec<Choice> {
        let snapshot = &self.snapshot;
        let direct = snapshot.model.work_mode == WorkMode::Direct;
        let proxy = snapshot.model.work_mode == WorkMode::Proxy;
        let mut choices = vec![Choice {
            label: self.tr("本机直连", "Direct"),
            checked: direct,
            command: Some(Command::SelectWorkMode(WorkMode::Direct)),
        }];
        let group = snapshot
            .model
            .default_group_id()
            .and_then(|id| snapshot.model.group(&id));
        if let Some(group) = group {
            let automatic = !group.manually_selectable();
            let gateway_selected = group
                .selected
                .as_ref()
                .and_then(|id| snapshot.model.node(id))
                .is_some_and(|node| node.is_mesh_gateway());
            if (automatic || gateway_selected)
                && snapshot
                    .model
                    .has_usable_default_group(snapshot.settings.core)
            {
                choices.push(Choice {
                    label: format!(
                        "{} ({})",
                        if automatic {
                            self.tr("自动选择", "Automatic")
                        } else {
                            self.tr("代理", "Proxy")
                        },
                        self.outbound_group_label(group)
                    )
                    .into(),
                    checked: proxy,
                    command: Some(Command::SelectWorkMode(WorkMode::Proxy)),
                });
            }
            let suffixes = snapshot.mesh.magic_dns_suffixes();
            let selected = snapshot.selected_node(Some(&group.id));
            let testing = snapshot.busy.iter().any(|key| key == "latency");
            for node in group
                .members
                .iter()
                .filter_map(|id| snapshot.model.node(id))
                .filter(|node| !node.is_mesh_gateway())
            {
                let name = super::nodes::display_node_name(node, self.hide_ips, &suffixes);
                let delay = theme::latency_label(
                    snapshot
                        .latency
                        .get(&node.id)
                        .filter(|entry| entry.is_http()),
                    testing
                        && snapshot
                            .busy
                            .iter()
                            .any(|k| k == &format!("latency:{}", node.id)),
                    self.lang(),
                );
                choices.push(Choice {
                    label: format!("{name}  {delay}").into(),
                    checked: selected.is_some_and(|n| n.id == node.id),
                    command: (!automatic
                        && group.enabled
                        && snapshot
                            .model
                            .node_usable_with(node, snapshot.settings.core))
                    .then(|| Command::SelectNode(group.id.clone(), node.id.clone())),
                });
            }
        }
        let suffixes = snapshot.mesh.magic_dns_suffixes();
        let target = snapshot.model.mesh_mode.target.as_ref();
        let mut target_listed = false;
        for (profile, peer, _) in snapshot.mesh.exits() {
            let current = target
                .is_some_and(|target| target.profile_id == profile.id && target.peer_id == peer.id);
            target_listed |= current;
            let name = if peer.name.is_empty() {
                profile.name.clone()
            } else if self.hide_ips {
                xrose_core::privacy::mask_magic_dns_name(&peer.name, &suffixes)
            } else {
                peer.name.clone()
            };
            choices.push(Choice {
                label: format!("{} · {name}", self.tr("组网", "Mesh")).into(),
                checked: snapshot.model.work_mode == WorkMode::Mesh && current,
                command: (peer.online || current)
                    .then(|| Command::SelectMeshExit(profile.id.clone(), peer.id.clone())),
            });
        }
        if !target_listed && let Some(target) = target {
            let name = if self.hide_ips {
                xrose_core::privacy::mask_server_in_label(
                    &target.name,
                    &target.address,
                    "•••",
                    &suffixes,
                )
            } else {
                target.name.clone()
            };
            choices.push(Choice {
                label: format!("{} · {name}", self.tr("组网", "Mesh")).into(),
                checked: snapshot.model.work_mode == WorkMode::Mesh,
                command: Some(Command::SelectWorkMode(WorkMode::Mesh)),
            });
        }
        if choices.len() == 1 {
            choices.push(Choice {
                label: if group.is_some() {
                    self.tr("此出站组暂无节点", "No nodes in this group")
                } else {
                    self.tr("请先添加节点", "Add a node first")
                },
                checked: false,
                command: None,
            });
        }
        choices
    }
}

impl NodeSelector {
    fn new(
        owner: WeakEntity<AppView>,
        choices: Vec<Choice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            owner,
            choices,
            focus,
            highlighted: None,
            scroll: ScrollHandle::new(),
            scrollbar_mode: None,
        }
    }

    fn confirm(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = self.choices[index].command.clone() {
            let _ = self.owner.update(cx, |app, cx| {
                app.dismiss_action_menu(cx);
                app.run_command(command, false, window, cx);
            });
        }
    }

    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.choices.len();
        let mut index = self.highlighted.unwrap_or(if down { count - 1 } else { 0 });
        for _ in 0..count {
            index = (index + if down { 1 } else { count - 1 }) % count;
            if self.choices[index].command.is_some() {
                self.highlighted = Some(index);
                self.scroll.scroll_to_item(index);
                cx.notify();
                break;
            }
        }
    }
}

impl Render for NodeSelector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Measure the first laid-out list before starting its one-time idle timeout.
        let mode = *self.scrollbar_mode.get_or_insert_with(|| {
            cx.on_next_frame(window, |this, _, cx| {
                if this.scroll.max_offset().y <= Pixels::ZERO {
                    this.scrollbar_mode = Some(ScrollbarMode::Scrolling);
                    return;
                }
                let idle = gpui_base::Theme::global(cx).scrollbar.motion().idle();
                cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(idle).await;
                    let _ = view.update(cx, |this, cx| {
                        this.scrollbar_mode = Some(ScrollbarMode::Scrolling);
                        cx.notify();
                    });
                })
                .detach();
            });
            ScrollbarMode::Always
        });
        let colors = theme::palette(cx);
        let max_height = (window.viewport_size().height * 0.5).min(px(450.));
        let rows = self.choices.iter().enumerate().map(|(index, choice)| {
            let enabled = choice.command.is_some();
            let row = div()
                .id(("node-choice", index))
                .role(Role::MenuItem)
                .aria_label(choice.label.clone())
                .aria_selected(self.highlighted == Some(index))
                .aria_toggled(if choice.checked {
                    accesskit::Toggled::True
                } else {
                    accesskit::Toggled::False
                })
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .min_h(px(26.))
                .rounded(cx.theme().radius)
                .when(!enabled, |d| d.text_color(colors.muted_fg))
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .when(self.highlighted == Some(index), |d| {
                            d.bg(colors.accent).text_color(cx.theme().accent_foreground)
                        })
                        .hover(|d| d.bg(colors.accent).text_color(cx.theme().accent_foreground))
                        .on_hover(cx.listener(move |this, hovered, window, cx| {
                            if *hovered {
                                this.highlighted = Some(index);
                            } else if this.highlighted == Some(index)
                                && !window.last_input_was_keyboard()
                            {
                                this.highlighted = None;
                            }
                            cx.notify();
                        }))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.confirm(index, window, cx)),
                        )
                })
                .child(
                    if choice.checked {
                        Icon::new(IconName::Check)
                    } else {
                        Icon::empty()
                    }
                    .xsmall(),
                )
                .child(div().min_w_0().truncate().child(choice.label.clone()));
            // Keep the divider inside the Direct row's container so keyboard indices match scroll indices.
            div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .child(row)
                .when(index == 0, |d| {
                    d.child(div().my_0p5().border_b_1().border_color(colors.border))
                })
        });
        div()
            .id("node-choice-menu")
            .relative()
            .popover_style(cx)
            .text_color(cx.theme().popover_foreground)
            .track_focus(&self.focus)
            .key_context("PopupMenu")
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.move_highlight(true, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.move_highlight(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                if let Some(index) = this.highlighted {
                    this.confirm(index, window, cx);
                }
            }))
            .role(Role::Menu)
            .text_sm()
            .child(
                div()
                    .id("node-choices")
                    .flex()
                    .flex_col()
                    .gap_y_0p5()
                    .p_1()
                    .min_w(rems(8.))
                    .max_w(px(500.))
                    .max_h(max_height)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
            )
            .child(Scrollbar::vertical(&self.scroll).mode(mode))
    }
}
