//! Proxy chain editor mounted inside the Add/Edit Node dialog.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{AppModel, ChainHop, MAX_CHAIN_HOPS, Node};
use xrose_core::settings::Language;

use super::dialogs::dialog_footer;
use super::menus::{self, MenuTarget};
use super::nodes::display_node_name;
use super::theme::{CONTROL_HEIGHT, palette};
use super::widgets::{FieldWidth, dialog_field, pill};
use super::{AppView, tr};

pub(crate) struct ChainForm {
    pub name: Entity<InputState>,
    pub hops: Vec<Option<ChainHop>>,
}

impl ChainForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("留空自动使用 跳点1 → 跳点2".to_string())
        });
        Self {
            name,
            hops: vec![None, None],
        }
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.hops = vec![None, None];
    }

    pub fn load(&mut self, node: &Node, window: &mut Window, cx: &mut Context<AppView>) {
        self.name
            .update(cx, |input, cx| input.set_value(&node.name, window, cx));
        let mut hops: Vec<Option<ChainHop>> = node.chain_hops.iter().cloned().map(Some).collect();
        while hops.len() < 2 {
            hops.push(None);
        }
        self.hops = hops;
    }

    pub fn prefill_entry(
        &mut self,
        node_id: String,
        window: &mut Window,
        cx: &mut Context<AppView>,
    ) {
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.hops = vec![Some(ChainHop::Node(node_id)), None];
    }

    pub fn localize_inputs(&self, lang: Language, window: &mut Window, cx: &mut Context<AppView>) {
        let placeholder = tr(
            lang,
            "留空自动使用 跳点1 → 跳点2",
            "Leave blank to auto-name from hops",
        );
        self.name.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
        });
    }
}

pub(super) fn hop_role_label(index: usize, total: usize, lang: Language) -> SharedString {
    if index == 0 {
        tr(lang, "入口跳点", "Entry hop")
    } else if index + 1 == total {
        tr(lang, "落地跳点", "Exit hop")
    } else {
        tr(lang, "中转跳点", "Relay hop")
    }
}

pub(super) fn format_hop_label(
    hop: &ChainHop,
    model: &AppModel,
    hide_ips: bool,
    suffixes: &[&str],
    lang: Language,
) -> (String, bool) {
    match hop {
        ChainHop::Node(id) => match model.node(id) {
            Some(node) => (display_node_name(node, hide_ips, suffixes), !node.enabled),
            None => (tr(lang, "缺失节点", "Missing node").to_string(), true),
        },
        ChainHop::Group(gid) => match model.group(gid) {
            Some(group) => (
                format!(
                    "{} ({})",
                    crate::i18n::outbound_group_label(lang, group),
                    tr(lang, "出站组", "Group")
                ),
                !group.enabled,
            ),
            None => (tr(lang, "缺失出站组", "Missing group").to_string(), true),
        },
    }
}

pub(super) fn auto_chain_name(model: &AppModel, hops: &[ChainHop], lang: Language) -> String {
    let parts: Vec<String> = hops
        .iter()
        .map(|hop| match hop {
            ChainHop::Node(id) => model
                .node(id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| tr(lang, "节点", "Node").to_string()),
            ChainHop::Group(gid) => model
                .group(gid)
                .map(|g| crate::i18n::outbound_group_label(lang, g).to_string())
                .unwrap_or_else(|| tr(lang, "出站组", "Group").to_string()),
        })
        .collect();
    parts.join(" → ")
}

/// Returns whether any trailing hop (index >= 1) requires UDP/QUIC forwarding
/// from an earlier hop (for example TUIC, Hysteria2, or WireGuard).
pub(super) fn has_trailing_udp_hop(model: &AppModel, hops: &[Option<ChainHop>]) -> bool {
    hops.iter().skip(1).any(|slot| match slot {
        Some(ChainHop::Node(id)) => model.node(id).is_some_and(Node::uses_udp_transport),
        _ => false,
    })
}

/// Returns whether swapping `index` and `index + 1` keeps any `ChainHop::Group`
/// strictly at slot 0.
pub(super) fn can_swap_hops(hops: &[Option<ChainHop>], index: usize) -> bool {
    if index + 1 >= hops.len() {
        return false;
    }
    if index == 0 && matches!(hops[0], Some(ChainHop::Group(_))) {
        return false;
    }
    true
}

impl AppView {
    pub(super) fn chain_editor_fields(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let lang = self.lang();
        let model = &self.snapshot.model;
        let suffixes = self.snapshot.mesh.magic_dns_suffixes();
        let hops = &self.chain_form.hops;
        let total = hops.len();
        let single_hop_count = model.nodes.iter().filter(|n| !n.is_chain()).count();

        let mut hop_list = div().flex().flex_col().gap_1p5().w_full();
        for (index, slot) in hops.iter().enumerate() {
            let role = hop_role_label(index, total, lang);
            let can_up = index > 0 && can_swap_hops(hops, index - 1);
            let can_down = can_swap_hops(hops, index);
            let can_remove = total > 2;
            let (picker_label, is_placeholder, is_warn) = match slot {
                Some(hop) => {
                    let (label, warn) =
                        format_hop_label(hop, model, self.hide_ips, &suffixes, lang);
                    (label, false, warn)
                }
                None => (
                    if index == 0 {
                        tr(lang, "选择节点或出站组…", "Choose node or group…").to_string()
                    } else if index + 1 == total {
                        tr(lang, "选择落地节点…", "Choose exit node…").to_string()
                    } else {
                        tr(lang, "选择中转节点…", "Choose relay node…").to_string()
                    },
                    true,
                    false,
                ),
            };

            hop_list = hop_list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .bg(p.bg)
                    .border_1()
                    .border_color(if is_warn {
                        p.warn_text.opacity(0.6)
                    } else {
                        p.border
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .w(px(96.))
                            .flex_shrink_0()
                            .child(pill(format!("{}", index + 1), p.primary, p.accent))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.muted_fg)
                                    .truncate()
                                    .child(role),
                            ),
                    )
                    .child(
                        div().flex_1().min_w(px(0.)).child(
                            Button::new(SharedString::from(format!("chain-hop-picker-{index}")))
                                .compact()
                                .h(px(CONTROL_HEIGHT))
                                .w_full()
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .text_sm()
                                                .text_color(if is_placeholder {
                                                    p.muted_fg
                                                } else if is_warn {
                                                    p.warn_text
                                                } else {
                                                    p.fg
                                                })
                                                .truncate()
                                                .child(picker_label),
                                        )
                                        .child(
                                            Icon::new(IconName::ChevronDown)
                                                .size(px(14.))
                                                .flex_shrink_0()
                                                .text_color(p.muted_fg),
                                        ),
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::ChainHop(index),
                                )),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_0p5()
                            .flex_shrink_0()
                            .child(
                                Button::new(SharedString::from(format!("chain-hop-up-{index}")))
                                    .compact()
                                    .ghost()
                                    .icon(IconName::ChevronUp)
                                    .disabled(!can_up)
                                    .accessibility_label(self.tr("上移跳点", "Move hop up"))
                                    .tooltip(self.tr("上移跳点", "Move hop up"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if index > 0
                                            && can_swap_hops(&this.chain_form.hops, index - 1)
                                        {
                                            this.chain_form.hops.swap(index - 1, index);
                                            this.manual_error = None;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("chain-hop-down-{index}")))
                                    .compact()
                                    .ghost()
                                    .icon(IconName::ChevronDown)
                                    .disabled(!can_down)
                                    .accessibility_label(self.tr("下移跳点", "Move hop down"))
                                    .tooltip(self.tr("下移跳点", "Move hop down"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if can_swap_hops(&this.chain_form.hops, index) {
                                            this.chain_form.hops.swap(index, index + 1);
                                            this.manual_error = None;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .when(can_remove, |d| {
                                d.child(
                                    Button::new(SharedString::from(format!(
                                        "chain-hop-remove-{index}"
                                    )))
                                    .compact()
                                    .ghost()
                                    .icon(IconName::X)
                                    .accessibility_label(self.tr("移除跳点", "Remove hop"))
                                    .tooltip(self.tr("移除跳点", "Remove hop"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.chain_form.hops.len() > 2
                                            && index < this.chain_form.hops.len()
                                        {
                                            this.chain_form.hops.remove(index);
                                            this.manual_error = None;
                                            cx.notify();
                                        }
                                    })),
                                )
                            }),
                    ),
            );

            if index + 1 < total {
                hop_list = hop_list.child(
                    div().pl(px(16.)).flex().items_center().child(
                        Icon::new(IconName::ArrowDown)
                            .size(px(12.))
                            .text_color(p.muted_fg.opacity(0.7)),
                    ),
                );
            }
        }

        let filled_preview: Option<String> = {
            let chosen: Vec<String> = hops
                .iter()
                .filter_map(|slot| slot.as_ref())
                .map(|hop| format_hop_label(hop, model, self.hide_ips, &suffixes, lang).0)
                .collect();
            (chosen.len() >= 2).then(|| chosen.join(" ──▶ "))
        };
        let trailing_udp = has_trailing_udp_hop(model, hops);

        div()
            .flex()
            .flex_col()
            .gap_3()
            .min_w(px(0.))
            .child(dialog_field(
                &p,
                self.tr("代理链名称", "Chain name"),
                &self.chain_form.name,
                FieldWidth::Full,
            ))
            .when(single_hop_count < 2, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .p_2p5()
                        .rounded_md()
                        .bg(p.bg)
                        .border_1()
                        .border_color(p.warn_text.opacity(0.5))
                        .text_xs()
                        .text_color(p.warn_text)
                        .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
                        .child(self.tr(
                            "需要至少 2 个普通节点才能组建代理链，请先添加或导入节点。",
                            "At least 2 single-hop nodes are required to build a proxy chain.",
                        )),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.fg)
                                    .child(self.tr("跳点顺序", "Hop sequence")),
                            )
                            .child(
                                Button::new("chain-add-hop")
                                    .compact()
                                    .ghost()
                                    .h(px(26.))
                                    .icon(IconName::Plus)
                                    .label(self.tr("添加跳点", "Add hop"))
                                    .disabled(total >= MAX_CHAIN_HOPS || single_hop_count < 2)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.chain_form.hops.len() < MAX_CHAIN_HOPS {
                                            this.chain_form.hops.push(None);
                                            this.manual_error = None;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    )
                    .child(hop_list),
            )
            .when_some(filled_preview, |d, summary| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(p.accent)
                        .text_xs()
                        .text_color(p.primary)
                        .truncate()
                        .child(format!(
                            "{}：{summary}",
                            self.tr("转发路径", "Forwarding path")
                        )),
                )
            })
            .when(trailing_udp, |d| {
                d.child(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .p_2p5()
                        .rounded_md()
                        .bg(p.bg)
                        .border_1()
                        .border_color(p.warn_text.opacity(0.5))
                        .text_xs()
                        .text_color(p.warn_text)
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(14.))
                                .flex_shrink_0(),
                        )
                        .child(self.tr(
                            "使用 UDP 的跳点需要前一跳能向其服务器端口转发 UDP。",
                            "A hop that uses UDP requires the preceding hop to forward UDP to its server port.",
                        )),
                )
            })
            .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                "流量按顺序由第 1 跳转发至最后一跳，目标网站看到最后一跳的出口 IP。仅第 1 跳支持引用出站组。",
                "Traffic forwards in order from hop 1 to the last hop; destinations see the last hop's exit IP. Only hop 1 can reference an outbound group.",
            )))
    }

    pub(super) fn chain_editor_footer(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let editing = self.editing_node.is_some();
        let core_busy = self.snapshot.busy.iter().any(|k| k == "core");
        dialog_footer(
            &p,
            self.manual_error.clone(),
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-chain-editor")
                        .label(self.tr("取消", "Cancel"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.close_dialog(cx);
                            this.clear_node_editor(window, cx);
                        })),
                )
                .child(
                    Button::new("save-chain-editor")
                        .primary()
                        .disabled(core_busy)
                        .label(if editing {
                            self.tr("保存代理链", "Save chain")
                        } else {
                            self.tr("创建代理链", "Create chain")
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_chain_editor(window, cx);
                        })),
                ),
        )
    }

    fn submit_chain_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut hops = Vec::with_capacity(self.chain_form.hops.len());
        for slot in &self.chain_form.hops {
            let Some(hop) = slot else {
                self.manual_error = Some(
                    self.tr(
                        "请为每一个跳点选择节点或出站组",
                        "Please select a node or outbound group for every hop",
                    )
                    .to_string(),
                );
                cx.notify();
                return;
            };
            hops.push(hop.clone());
        }
        let raw_name = self.chain_form.name.read(cx).value().trim().to_string();
        let name = if raw_name.is_empty() {
            auto_chain_name(&self.snapshot.model, &hops, self.lang())
        } else {
            raw_name
        };
        let editing_id = self.editing_node.clone();
        match self
            .service
            .save_proxy_chain(editing_id.as_deref(), name, hops)
        {
            Ok(_) => {
                window.close_dialog(cx);
                self.clear_node_editor(window, cx);
                self.sync(cx);
            }
            Err(err) => {
                self.manual_error = Some(err.to_string());
                cx.notify();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{auto_chain_name, can_swap_hops};
    use xrose_core::model::{AppModel, ChainHop, GroupKind, Node, OutboundGroup, Protocol};
    use xrose_core::settings::Language;

    #[test]
    fn swap_guard_keeps_group_hop_pinned_to_entry() {
        let hops = vec![
            Some(ChainHop::Group("g1".into())),
            Some(ChainHop::Node("n1".into())),
            Some(ChainHop::Node("n2".into())),
        ];
        assert!(!can_swap_hops(&hops, 0));
        assert!(can_swap_hops(&hops, 1));
        assert!(!can_swap_hops(&hops, 2));

        let node_hops = vec![
            Some(ChainHop::Node("n1".into())),
            Some(ChainHop::Node("n2".into())),
        ];
        assert!(can_swap_hops(&node_hops, 0));
    }

    #[test]
    fn auto_chain_name_joins_group_and_node_labels() {
        let mut model = AppModel::new("fixture");
        let n = Node::new("US-Home", Protocol::Socks, "203.0.113.10", 1080);
        model.add_nodes([n.clone()]);
        let mut g = OutboundGroup::new("HK Relay", GroupKind::Selector);
        g.members = vec![n.id.clone()];
        model.groups.push(g.clone());
        let name = auto_chain_name(
            &model,
            &[ChainHop::Group(g.id), ChainHop::Node(n.id)],
            Language::Zh,
        );
        assert_eq!(name, "HK Relay → US-Home");
    }

    #[test]
    fn trailing_udp_hop_flags_only_hops_after_entry() {
        let mut model = AppModel::new("fixture");
        let vless = Node::new("Vision", Protocol::Vless, "203.0.113.10", 443);
        let tuic = Node::new("HK-TUIC", Protocol::Tuic, "203.0.113.20", 443);
        model.add_nodes([vless.clone(), tuic.clone()]);

        assert!(!super::has_trailing_udp_hop(
            &model,
            &[
                Some(ChainHop::Node(tuic.id.clone())),
                Some(ChainHop::Node(vless.id.clone())),
            ],
        ));
        assert!(super::has_trailing_udp_hop(
            &model,
            &[
                Some(ChainHop::Node(vless.id)),
                Some(ChainHop::Node(tuic.id)),
            ],
        ));
    }
}
