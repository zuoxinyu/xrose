use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{GroupKind, Node};
use xross_core::service::LatencyScope;
use xross_core::util::format_speed;

use super::theme::palette;
use super::widgets::*;
use super::{AppView, Page};

impl AppView {
    pub(crate) fn page_nodes(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = self.snapshot.model.clone();
        let groups = model.groups.clone();
        let active_group = self
            .selected_group
            .clone()
            .filter(|id| model.group(id).is_some())
            .or_else(|| model.default_group_id());
        let group = active_group
            .as_ref()
            .and_then(|id| model.group(id))
            .cloned();
        let active_node = group.as_ref().and_then(|g| g.active_node().cloned());
        let running = self.snapshot.process.phase.is_alive();

        let mut body = div().flex().flex_col().gap_4().w_full();

        // First run / empty state.
        if model.nodes.is_empty() {
            body = body.child(
                card(&p)
                    .border_color(p.primary)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(img(self.logo_hero.clone()).w(px(64.)).h(px(64.)))
                            .child(section_title(
                                &p,
                                self.tr("欢迎使用 Xross", "Welcome to Xross"),
                                self.tr(
                                    "三步开始：添加订阅或导入节点 → 下载 Core → 启动",
                                    "Three steps: add a subscription or import a node → install a core → start",
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(pill("1", p.primary_fg, p.primary))
                                    .child(self.tr(
                                        "添加订阅（推荐）或从剪贴板导入单个节点",
                                        "Add a subscription (recommended) or import a node from the clipboard",
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(pill("2", p.primary_fg, p.primary))
                                    .child(self.tr(
                                        "在设置 → Core 下载 xray / sing-box（有校验文件时自动验证 SHA-256）",
                                        "Install xray / sing-box in Settings → Core (SHA-256 verified when published)",
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_sm()
                                    .child(pill("3", p.primary_fg, p.primary))
                                    .child(self.tr(
                                        "点击右上角“启动”，需要时再应用配置",
                                        "Click Start in the top-right, then apply config when needed",
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("wizard-subscription")
                                    .compact()
                                    .primary()
                                    .label(self.tr("添加订阅", "Add subscription"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.page = Page::Subscriptions;
                                        this.show_subscription_form = true;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("wizard-install")
                                    .compact()
                                    .label(self.tr("下载 Core", "Install core"))
                                    .on_click(cx.listener(|this, _, _, _| {
                                        let core = this.snapshot.settings.core;
                                        this.service.install_core_update(core);
                                    })),
                            ),
                    ),
            );
        }

        // Group switcher.
        if !groups.is_empty() {
            body = body.child(
                card(&p)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .children(groups.iter().map(|g| {
                                let selected = active_group.as_deref() == Some(g.id.as_str());
                                let gid = g.id.clone();
                                div()
                                    .id(SharedString::from(format!("group-{}", g.id)))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .border_1()
                                    .border_color(if selected { p.primary } else { p.border })
                                    .bg(if selected { p.accent } else { p.bg })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_group = Some(gid.clone());
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(if selected { p.fg } else { p.muted_fg })
                                            .child(g.name.clone()),
                                    )
                                    .child(pill(
                                        g.kind.display_name(),
                                        p.muted_fg,
                                        p.panel_alt,
                                    ))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(self.tr(
                                        "点击节点立即切换：sing-box 走 Clash API 热切换，Xray 会重启 core",
                                        "Click a node to switch: sing-box uses the Clash API, Xray restarts the core",
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("test-all")
                                            .compact()
                                            .label(self.tr("全部测速", "Test all"))
                                            .loading(this_busy(&self.snapshot.busy, "latency"))
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service.test_latency(LatencyScope::All);
                                            })),
                                    )
                                    .child(
                                        Button::new("add-node")
                                            .compact()
                                            .label(self.tr("添加节点", "Add node"))
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service
                                                    .toast(xross_core::service::ToastLevel::Info, "在下方粘贴节点链接即可导入");
                                            })),
                                    ),
                            ),
                    ),
            );
        }

        // Import bar.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex_1().child(Input::new(&self.import_input)))
                        .child(
                            Button::new("import-link")
                                .compact()
                                .primary()
                                .label(self.tr("导入", "Import"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let content = this.import_input.read(cx).value().to_string();
                                    if content.trim().is_empty() {
                                        this.service.toast(
                                            xross_core::service::ToastLevel::Warning,
                                            "请先粘贴节点链接",
                                        );
                                    } else {
                                        this.service.import_content(&content);
                                        this.import_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx);
                                        });
                                    }
                                    this.sync(cx);
                                })),
                        )
                        .child(
                            Button::new("import-clipboard")
                                .compact()
                                .label(self.tr("从剪贴板导入", "Import clipboard"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                                        this.service.import_content(&text);
                                    } else {
                                        this.service.toast(
                                            xross_core::service::ToastLevel::Warning,
                                            "剪贴板没有文本",
                                        );
                                    }
                                    this.sync(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr(
                            "支持 vmess / vless / trojan / ss / hysteria2 / tuic 分享链接、Clash YAML、SIP008、sing-box JSON",
                            "Supports vmess / vless / trojan / ss / hysteria2 / tuic links, Clash YAML, SIP008 and sing-box JSON",
                        )),
                ),
        );

        // Node list.
        let group_id_for_select = active_group.clone();
        let group_kind = group
            .as_ref()
            .map(|g| g.kind)
            .unwrap_or(GroupKind::Selector);
        let nodes: Vec<Node> = model.nodes.clone();
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("节点", "Nodes"),
                            format!("{} {}", nodes.len(), self.tr("个节点", "nodes")),
                        ))
                        .child(div().text_xs().text_color(p.muted_fg).child(format!(
                            "↓ {}  ↑ {}",
                            format_speed(self.snapshot.runtime.down_speed),
                            format_speed(self.snapshot.runtime.up_speed)
                        ))),
                )
                .child(self.sparkline(&p))
                .when(nodes.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr(
                            "还没有节点，先添加订阅或导入链接",
                            "No nodes yet — add a subscription or import a link",
                        ),
                    ))
                })
                .children(nodes.iter().filter(|n| n.enabled).map(|node| {
                    let selected = active_node.as_deref() == Some(node.id.as_str());
                    let latency = self.snapshot.latency.get(&node.id).cloned();
                    let node_id = node.id.clone();
                    let node_id_select = node.id.clone();
                    let node_id_tcp = node.id.clone();
                    let node_id_real = node.id.clone();
                    let node_id_share = node.id.clone();
                    let node_id_qr = node.id.clone();
                    let node_id_del = node.id.clone();
                    let group_for_click = group_id_for_select.clone();
                    let name = node.name.clone();
                    let meta = format!(
                        "{} · {} · {}",
                        node.protocol.display_name(),
                        node.endpoint(),
                        node.transport.kind_name()
                    );
                    let delay_label = latency
                        .as_ref()
                        .map(|l| match l.delay_ms {
                            Some(ms) => format!("{ms} ms"),
                            None => self.tr("超时", "timeout").to_string(),
                        })
                        .unwrap_or_else(|| "—".into());
                    let delay_color = match latency.as_ref().and_then(|l| l.delay_ms) {
                        Some(ms) if ms < 200 => p.success,
                        Some(ms) if ms < 600 => p.warning,
                        Some(_) => p.danger,
                        None => p.muted_fg,
                    };
                    div()
                        .id(SharedString::from(format!("node-{}", node.id)))
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(if selected { p.primary } else { p.border })
                        .bg(if selected { p.accent } else { p.bg })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(group) = &group_for_click {
                                this.service.select_node(group, &node_id_select);
                                this.sync(cx);
                            }
                        }))
                        .child(
                            div()
                                .w(px(24.))
                                .text_center()
                                .text_color(if selected { p.primary } else { p.muted_fg })
                                .child(if selected { "●" } else { "○" }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .flex_1()
                                .child(div().text_sm().text_color(p.fg).child(name))
                                .child(div().text_xs().text_color(p.muted_fg).child(meta)),
                        )
                        .when(group_kind != GroupKind::Selector, |d| {
                            d.child(pill(
                                if group_kind == GroupKind::UrlTest {
                                    self.tr("自动", "auto")
                                } else {
                                    self.tr("备用", "backup")
                                },
                                p.muted_fg,
                                p.panel_alt,
                            ))
                        })
                        .child(
                            div()
                                .w(px(72.))
                                .text_sm()
                                .text_color(delay_color)
                                .child(delay_label),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!("tcp-{node_id_tcp}")))
                                        .compact()
                                        .ghost()
                                        .label("TCP")
                                        .on_click(cx.listener(move |this, _, _, _| {
                                            this.service.test_latency(LatencyScope::Node(
                                                node_id_tcp.clone(),
                                            ));
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("real-{node_id_real}")))
                                        .compact()
                                        .ghost()
                                        .label(self.tr("真连", "Real"))
                                        .on_click(cx.listener(move |this, _, _, _| {
                                            this.service.test_real_latency(&node_id_real);
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "share-{node_id_share}"
                                    )))
                                    .compact()
                                    .ghost()
                                    .label("↗")
                                    .tooltip(self.tr("复制分享链接", "Copy share link"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let link =
                                            this.service.export_links(&[node_id_share.clone()]);
                                        if link.is_empty() {
                                            this.service.toast(
                                                xross_core::service::ToastLevel::Warning,
                                                "该协议暂不支持导出链接",
                                            );
                                        } else {
                                            cx.write_to_clipboard(ClipboardItem::new_string(link));
                                            this.service.toast(
                                                xross_core::service::ToastLevel::Success,
                                                "分享链接已复制",
                                            );
                                        }
                                    })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("qr-{node_id_qr}")))
                                        .compact()
                                        .ghost()
                                        .label("▦")
                                        .tooltip(self.tr("二维码", "QR code"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.qr_node = Some(node_id_qr.clone());
                                            this.show_qr = true;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("del-{node_id_del}")))
                                        .compact()
                                        .ghost()
                                        .label("✕")
                                        .tooltip(self.tr("删除节点", "Delete node"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.delete_node(&node_id_del);
                                            this.sync(cx);
                                        })),
                                ),
                        )
                        .child(
                            div().w(px(52.)).child(
                                toggle(
                                    SharedString::from(format!("enabled-{node_id}")),
                                    "",
                                    node.enabled,
                                )
                                .on_change(cx.listener(
                                    move |this, checked, _, cx| {
                                        this.service.set_node_enabled(&node_id, *checked);
                                        this.sync(cx);
                                    },
                                )),
                            ),
                        )
                })),
        );

        // QR overlay as an inline card (keeps the layout simple).
        if self.show_qr {
            if let Some(node_id) = self.qr_node.clone() {
                let qr = self
                    .service
                    .node_qr(&node_id)
                    .unwrap_or_else(|err| format!("无法生成二维码：{err}"));
                let name = model
                    .node(&node_id)
                    .map(|n| n.name.clone())
                    .unwrap_or_default();
                body = body.child(
                    card(&p)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(section_title(
                                    &p,
                                    self.tr("节点二维码", "Node QR code"),
                                    name,
                                ))
                                .child(
                                    Button::new("close-qr")
                                        .compact()
                                        .ghost()
                                        .label("✕")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_qr = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div().flex().justify_center().child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .text_color(p.fg)
                                    .child(qr),
                            ),
                        ),
                );
            }
        }

        // Disabled nodes (collapsed list)
        let disabled: Vec<&Node> = model.nodes.iter().filter(|n| !n.enabled).collect();
        if !disabled.is_empty() {
            body = body.child(
                card(&p)
                    .child(section_title(
                        &p,
                        self.tr("已禁用", "Disabled"),
                        format!("{} {}", disabled.len(), self.tr("个节点", "nodes")),
                    ))
                    .children(disabled.into_iter().map(|node| {
                        let node_id = node.id.clone();
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .p_2()
                            .rounded_md()
                            .bg(p.bg)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(p.muted_fg)
                                            .child(node.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(node.endpoint()),
                                    ),
                            )
                            .child(
                                toggle(
                                    SharedString::from(format!("disabled-{}", node.id)),
                                    "",
                                    false,
                                )
                                .on_change(cx.listener(
                                    move |this, checked, _, cx| {
                                        if *checked {
                                            this.service.set_node_enabled(&node_id, true);
                                            this.sync(cx);
                                        }
                                    },
                                )),
                            )
                    })),
            );
        }

        let _ = (lang, running);
        body
    }
}

fn this_busy(busy: &[String], key: &str) -> bool {
    busy.iter().any(|b| b == key)
}
