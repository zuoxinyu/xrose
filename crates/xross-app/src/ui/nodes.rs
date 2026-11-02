use std::collections::{BTreeMap, HashMap};

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{CoreKind, GroupKind, Node};
use xross_core::service::{LatencyScope, ToastLevel};
use xross_core::settings::Language;
use xross_core::util::format_speed;

use super::theme::{palette, Palette};
use super::widgets::*;
use super::{AppView, Page};

#[derive(Clone)]
struct DraggedNode {
    id: String,
    name: String,
}

impl Render for DraggedNode {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(p.panel)
            .border_1()
            .border_color(p.primary)
            .max_w(px(280.))
            .text_sm()
            .text_color(p.fg)
            .truncate()
            .child(self.name.clone())
    }
}

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
        let active_node = self
            .snapshot
            .selected_node(active_group.as_deref())
            .map(|n| n.id.clone());
        let running = self.snapshot.process.phase.is_alive();
        let is_xray = self.snapshot.settings.core == CoreKind::Xray;
        let is_core_busy = this_busy(&self.snapshot.busy, "core");

        let mut body = div().flex().flex_col().gap_4().w_full();

        // First run / empty state wizard.
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
                            )
                            .child(
                                Button::new("wizard-manual")
                                    .compact()
                                    .label(self.tr("手动添加节点", "Add manually"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_manual_form = true;
                                        this.manual_error = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }

        // P0: Standard Segmented Control for Outbound Groups / Modes.
        if !groups.is_empty() {
            body = body.child(
                div().flex().flex_col().gap_2().w_full().child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .p_1()
                                .rounded_lg()
                                .bg(p.panel)
                                .border_1()
                                .border_color(p.border)
                                .children(groups.iter().map(|g| {
                                    let selected = active_group.as_deref() == Some(g.id.as_str());
                                    let gid = g.id.clone();
                                    div()
                                        .id(SharedString::from(format!("group-{}", g.id)))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_1p5()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .when(selected, |d| {
                                            d.bg(p.bg).shadow_sm().border_1().border_color(p.border)
                                        })
                                        .when(!selected, |d| {
                                            d.hover(|h| h.bg(p.panel_alt.opacity(0.4)))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.selected_group = Some(gid.clone());
                                            this.refresh_header_location();
                                            cx.notify();
                                        }))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(if selected {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if selected {
                                                    p.fg
                                                } else {
                                                    p.muted_fg
                                                })
                                                .child(match (g.id.as_str(), g.name.as_str()) {
                                                    ("proxy", "代理") => {
                                                        self.tr("代理", "Proxy").to_string()
                                                    }
                                                    ("auto", "自动选择") => self
                                                        .tr("自动选择", "Auto select")
                                                        .to_string(),
                                                    _ => g.name.clone(),
                                                }),
                                        )
                                        .child(
                                            div()
                                                .px_1p5()
                                                .py_0p5()
                                                .rounded_sm()
                                                .text_xs()
                                                .bg(if selected { p.accent } else { p.panel_alt })
                                                .text_color(if selected {
                                                    p.primary
                                                } else {
                                                    p.muted_fg
                                                })
                                                .child(match g.kind {
                                                    GroupKind::Selector => {
                                                        self.tr("手动选择", "Manual")
                                                    }
                                                    GroupKind::UrlTest => {
                                                        self.tr("自动测速", "URL test")
                                                    }
                                                    GroupKind::Fallback => {
                                                        self.tr("故障转移", "Fallback")
                                                    }
                                                    GroupKind::LoadBalance => {
                                                        self.tr("负载均衡", "Load balance")
                                                    }
                                                }),
                                        )
                                })),
                        )
                        .when(is_xray, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_xs()
                                    .text_color(p.warning)
                                    .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
                                    .child(self.tr(
                                        "Xray core 模式：切换节点需重启 core",
                                        "Xray core: switching node restarts core",
                                    )),
                            )
                        }),
                ),
            );
        }

        if self.show_import_box || self.show_manual_form {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("add-mode-link")
                            .compact()
                            .when(self.show_import_box, |b| b.primary())
                            .label(self.tr("链接导入", "Import links"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_import_box = true;
                                this.show_manual_form = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("add-mode-manual")
                            .compact()
                            .when(self.show_manual_form, |b| b.primary())
                            .label(self.tr("手动填写", "Enter manually"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_import_box = false;
                                this.show_manual_form = true;
                                this.manual_error = None;
                                cx.notify();
                            })),
                    ),
            );
        }

        // P1: Collapsible Import Drawer (triggered by "+ 添加节点" in list toolbar).
        if self.show_import_box {
            let import_val = self.import_input.read(cx).value().trim().to_string();
            let is_empty = import_val.is_empty();
            body = body.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(p.panel)
                    .border_1()
                    .border_color(p.primary.opacity(0.3))
                    .shadow_sm()
                    .flex()
                    .flex_col()
                    .gap_2()
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
                                    .disabled(is_empty)
                                    .label(self.tr("导入", "Import"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let content = this.import_input.read(cx).value().to_string();
                                        if content.trim().is_empty() {
                                            this.service.toast(
                                                ToastLevel::Warning,
                                                "请先粘贴节点链接",
                                            );
                                        } else {
                                            this.service.import_content(&content);
                                            this.import_input.update(cx, |input, cx| {
                                                input.set_value("", window, cx);
                                            });
                                            this.show_import_box = false;
                                        }
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                Button::new("import-clipboard")
                                    .compact()
                                    .label(self.tr("从剪贴板导入", "From Clipboard"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                                            this.service.import_content(&text);
                                            this.show_import_box = false;
                                        } else {
                                            this.service.toast(
                                                ToastLevel::Warning,
                                                "剪贴板没有文本",
                                            );
                                        }
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                Button::new("close-import")
                                    .compact()
                                    .ghost()
                                    .icon(IconName::X)
                                    .tooltip(self.tr("收起", "Close"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_import_box = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr(
                                "支持 vmess / vless / trojan / ss / hysteria2 / tuic 分享链接、Clash YAML、SIP008、sing-box JSON",
                                "Supports vmess / vless / trojan / ss / hysteria2 / tuic links, Clash YAML, SIP008, sing-box JSON",
                            )),
                    ),
            );
        }

        // P1b: full manual node form (protocol / credentials / TLS / transport).
        if self.show_manual_form {
            body = body.child(self.manual_node_card(cx));
        }

        // Filter and sort nodes.
        let group_id_for_select = active_group.clone();
        let group_kind = group
            .as_ref()
            .map(|g| g.kind)
            .unwrap_or(GroupKind::Selector);

        let search_query = self.node_search.read(cx).value().trim().to_lowercase();
        let mut display_nodes: Vec<Node> = model
            .nodes
            .iter()
            .filter(|n| {
                if search_query.is_empty() {
                    true
                } else {
                    n.name.to_lowercase().contains(&search_query)
                        || n.endpoint().to_lowercase().contains(&search_query)
                        || n.protocol
                            .display_name()
                            .to_lowercase()
                            .contains(&search_query)
                }
            })
            .cloned()
            .collect();

        if self.sort_by_latency {
            display_nodes.sort_by_key(|n| {
                let ms = self.snapshot.latency.get(&n.id).and_then(|l| l.delay_ms);
                match ms {
                    Some(v) => (0, v),
                    None => (1, u32::MAX),
                }
            });
        }

        // Identify fastest node (among enabled nodes with valid delay).
        let fastest_node_id: Option<String> = display_nodes
            .iter()
            .filter(|n| n.enabled)
            .filter_map(|n| {
                let ms = self.snapshot.latency.get(&n.id).and_then(|l| l.delay_ms)?;
                if ms > 0 {
                    Some((n.id.clone(), ms))
                } else {
                    None
                }
            })
            .min_by_key(|(_, ms)| *ms)
            .map(|(id, _)| id);

        // Identify timeout / dead nodes for batch cleanup.
        let timeout_node_ids: Vec<String> = model
            .nodes
            .iter()
            .filter(|n| {
                self.snapshot
                    .latency
                    .get(&n.id)
                    .map(|l| l.delay_ms.is_none())
                    .unwrap_or(false)
            })
            .map(|n| n.id.clone())
            .collect();
        let timeout_count = timeout_node_ids.len();

        // Traffic sparkline statistics.
        let samples = self.snapshot.runtime.history.clone();
        let max_speed = samples
            .iter()
            .map(|s| s.up_speed + s.down_speed)
            .max()
            .unwrap_or(0);
        let has_traffic = max_speed > 0;

        let hide_ips = self.hide_ips;
        let menu_open = self.node_menu_open.clone();
        let batch_mode = self.batch_mode;
        let batch_selected = self.batch_selected.clone();

        // Subscription map for grouping.
        let sub_names: HashMap<String, String> = self
            .snapshot
            .subscriptions
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();

        // Node list card (now the primary focal point of the page).
        body = body.child(
            card(&p)
                // Header with unified toolbar (search, sort, group, privacy, test all, add node, batch mode).
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .flex_wrap()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2p5()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.fg)
                                        .child(self.tr("节点列表", "Nodes")),
                                )
                                .child(pill(
                                    if search_query.is_empty() {
                                        format!("{} {}", display_nodes.len(), self.tr("个", ""))
                                    } else {
                                        format!(
                                            "{}/{} {}",
                                            display_nodes.len(),
                                            model.nodes.len(),
                                            self.tr("个", "")
                                        )
                                    },
                                    p.primary,
                                    p.accent,
                                )),
                        )
                        .child(
                            Button::new("toggle-add-drawer")
                                .compact().primary()
                                .icon(if self.show_import_box || self.show_manual_form { IconName::X } else { IconName::Plus })
                                .label(if self.show_import_box || self.show_manual_form { self.tr("收起", "Close") } else { self.tr("添加节点", "Add node") })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let open = !(this.show_import_box || this.show_manual_form);
                                    this.show_import_box = open;
                                    this.show_manual_form = false;
                                    this.manual_error = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_start()
                                .gap_1p5()
                                .child(
                                    div()
                                        .w(px(190.))
                                        .child(Input::new(&self.node_search)),
                                )
                                // Group by subscription toggle
                                .child(
                                    Button::new("toggle-grouping")
                                        .compact()
                                        .when(self.group_by_subscription, |b| b.primary())
                                        .when(!self.group_by_subscription, |b| b.ghost())
                                        .icon(IconName::Layers)
                                        .label(if self.group_by_subscription { self.tr("分组中", "Grouped") } else { self.tr("按订阅", "By subscription") })
                                        .tooltip(if self.group_by_subscription {
                                            self.tr("按订阅分组展示中 (点击切回平铺)", "Grouped by subscription (click to flatten)")
                                        } else {
                                            self.tr("按订阅分组与折叠节点 (推荐 20+ 节点时使用)", "Group by subscription (recommended for 20+ nodes)")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.group_by_subscription = !this.group_by_subscription;
                                            cx.notify();
                                        })),
                                )
                                // Latency sort toggle
                                .child(
                                    Button::new("sort-latency")
                                        .compact()
                                        .when(self.sort_by_latency, |b| b.primary())
                                        .when(!self.sort_by_latency, |b| b.ghost())
                                        .icon(IconName::ArrowDownUp)
                                        .label(self.tr("延迟", "Latency"))
                                        .tooltip(if self.sort_by_latency {
                                            self.tr("当前按延迟升序排序 (点击恢复默认)", "Sorted by latency (click to reset)")
                                        } else {
                                            self.tr("按延迟排序", "Sort by latency")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sort_by_latency = !this.sort_by_latency;
                                            cx.notify();
                                        })),
                                )
                                // Privacy mode toggle
                                .child(
                                    Button::new("toggle-privacy")
                                        .compact()
                                        .when(self.hide_ips, |b| b.primary())
                                        .when(!self.hide_ips, |b| b.ghost())
                                        .icon(if self.hide_ips { IconName::EyeOff } else { IconName::Eye })
                                        .label(self.tr("隐私", "Privacy"))
                                        .tooltip(if self.hide_ips {
                                            self.tr("服务器地址已脱敏保护 (点击还原)", "Addresses masked (click to reveal)")
                                        } else {
                                            self.tr("脱敏隐藏服务器地址 (防截图泄露)", "Mask server addresses for privacy")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.hide_ips = !this.hide_ips;
                                            cx.notify();
                                        })),
                                )
                                // Batch management toggle
                                .child(
                                    Button::new("toggle-batch")
                                        .compact()
                                        .when(self.batch_mode, |b| b.primary())
                                        .when(!self.batch_mode, |b| b.ghost())
                                        .icon(IconName::ListChecks)
                                        .label(self.tr("批量", "Batch"))
                                        .tooltip(if self.batch_mode {
                                            self.tr("退出批量管理模式", "Exit batch mode")
                                        } else {
                                            self.tr("开启多选批量管理模式", "Open batch management mode")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.batch_mode = !this.batch_mode;
                                            if !this.batch_mode {
                                                this.batch_selected.clear();
                                            }
                                            cx.notify();
                                        })),
                                )
                                // Clean dead nodes button
                                .child(
                                    Button::new("clean-dead-nodes")
                                        .compact()
                                        .ghost()
                                        .icon(IconName::Broom)
                                        .label(self.tr("清理超时", "Clean Dead"))
                                        .tooltip(self.tr(
                                            "一键清理测速超时的不可用节点",
                                            "Batch delete all timeout/dead nodes",
                                        ))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let timeout_ids: Vec<String> = this
                                                .snapshot
                                                .model
                                                .nodes
                                                .iter()
                                                .filter(|n| {
                                                    this.snapshot
                                                        .latency
                                                        .get(&n.id)
                                                        .map(|l| l.delay_ms.is_none())
                                                        .unwrap_or(false)
                                                })
                                                .map(|n| n.id.clone())
                                                .collect();
                                            if timeout_ids.is_empty() {
                                                this.service.toast(
                                                    ToastLevel::Info,
                                                    "当前没有测速超时的不可用节点",
                                                );
                                            } else {
                                                this.show_clean_confirm = true;
                                                cx.notify();
                                            }
                                        })),
                                )
                                // Test all
                                .child(
                                    Button::new("test-all")
                                        .compact()
                                        .icon(IconName::Activity)
                                        .label(self.tr("全部测速", "Test All"))
                                        .loading(this_busy(&self.snapshot.busy, "latency"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.test_latency(LatencyScope::All);
                                        })),
                                )
                )
                // Batch Operation Bar (Visible when batch_mode is active).
                .when(batch_mode, |d| {
                    let all_ids: Vec<String> = display_nodes.iter().map(|n| n.id.clone()).collect();
                    let all_selected = !all_ids.is_empty() && all_ids.iter().all(|id| batch_selected.contains(id));
                    let selected_count = batch_selected.len();
                    let selected_ids: Vec<String> = batch_selected.iter().cloned().collect();

                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(p.accent)
                            .border_1()
                            .border_color(p.primary.opacity(0.4))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child(format!("{} {selected_count} / {}", self.tr("已选择", "Selected"), display_nodes.len())),
                                    )
                                    .child(
                                        Button::new("batch-select-all")
                                            .compact()
                                            .ghost()
                                            .label(if all_selected { self.tr("取消全选", "Deselect all") } else { self.tr("全选当前", "Select all") })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if all_selected {
                                                    this.batch_selected.clear();
                                                } else {
                                                    this.batch_selected = all_ids.iter().cloned().collect();
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-ping")
                                            .compact()
                                            .disabled(selected_count == 0)
                                            .label(self.tr("批量测速", "Test selected"))
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                for id in &sel {
                                                    this.service.test_latency(LatencyScope::Node(id.clone()));
                                                }
                                            }))
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-enable")
                                            .compact()
                                            .disabled(selected_count == 0)
                                            .label(self.tr("批量启用", "Enable selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                for id in &sel {
                                                    this.service.set_node_enabled(id, true);
                                                }
                                                this.sync(cx);
                                            }))
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-disable")
                                            .compact()
                                            .disabled(selected_count == 0)
                                            .label(self.tr("批量停用", "Disable selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                for id in &sel {
                                                    this.service.set_node_enabled(id, false);
                                                }
                                                this.sync(cx);
                                            }))
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-delete")
                                            .compact()
                                            .disabled(selected_count == 0)
                                            .ghost()
                                            .icon(IconName::Trash)
                                            .label(self.tr("批量删除", "Delete selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.delete_nodes(&sel);
                                                this.batch_selected.clear();
                                                this.sync(cx);
                                            }))
                                    })
                                    .child(
                                        Button::new("batch-exit")
                                            .compact()
                                            .ghost()
                                            .label(self.tr("退出批量", "Exit batch mode"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.batch_mode = false;
                                                this.batch_selected.clear();
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                })
                // Clean Dead Nodes Confirmation Banner
                .when(self.show_clean_confirm, |d| {
                    let ids = timeout_node_ids.clone();
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_4()
                            .py_2p5()
                            .rounded_md()
                            .bg(p.accent)
                            .border_1()
                            .border_color(p.warning)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(Icon::new(IconName::TriangleAlert).size(px(16.)).text_color(p.warning))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .child(format!("{timeout_count} {}", self.tr("个节点测速超时或无法连接，确认全部删除？", "nodes timed out or are unreachable. Delete all?"))),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("confirm-clean-dead")
                                            .compact()
                                            .primary()
                                            .label(self.tr("确认批量删除", "Confirm deletion"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.delete_nodes(&ids);
                                                this.show_clean_confirm = false;
                                                this.sync(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("cancel-clean-dead")
                                            .compact()
                                            .ghost()
                                            .label(self.tr("取消", "Cancel"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_clean_confirm = false;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                })
                // Sparkline: collapsed when no active traffic, with clear legend when active.
                .when(has_traffic, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .py_1()
                            .border_y_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(dot(p.primary))
                                                    .child(self.tr("下行", "Down")),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(dot(p.info))
                                                    .child(self.tr("上行", "Up")),
                                            ),
                                    )
                                    .child(
                                        div().child(format!(
                                            "{}: {}",
                                            self.tr("峰值速率", "Peak"),
                                            format_speed(max_speed)
                                        )),
                                    ),
                            )
                            .child(self.sparkline(&p)),
                    )
                })
                .when(display_nodes.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        if search_query.is_empty() {
                            self.tr(
                                "还没有节点，点击右上角“+ 添加节点”或添加订阅",
                                "No nodes yet — click '+ Add' above or add a subscription",
                            )
                        } else {
                            self.tr("未找到匹配的节点", "No matching nodes found")
                        },
                    ))
                })
                // Node list: either Grouped by Subscription or Flat List.
                .when(!self.group_by_subscription, |container| {
                    container.children(display_nodes.iter().map(|node| {
                        render_node_row(
                            node,
                            &p,
                            lang,
                            active_node.as_deref(),
                            fastest_node_id.as_deref(),
                            self.snapshot.latency.get(&node.id),
                            hide_ips,
                            group_kind,
                            group_id_for_select.clone(),
                            is_core_busy,
                            this_busy(&self.snapshot.busy, "latency"),
                            menu_open.as_deref(),
                            batch_mode,
                            batch_selected.contains(&node.id),
                            cx,
                        )
                    }))
                })
                .when(self.group_by_subscription, |container| {
                    // Partition nodes into Subscription Groups
                    let mut sub_groups: BTreeMap<Option<String>, Vec<Node>> = BTreeMap::new();
                    for node in display_nodes.iter() {
                        sub_groups
                            .entry(node.subscription_id.clone())
                            .or_default()
                            .push(node.clone());
                    }

                    let collapsed_set = self.collapsed_subs.clone();

                    container.children(sub_groups.into_iter().map(|(sub_id, group_nodes)| {
                        let group_key = sub_id.clone().unwrap_or_else(|| "manual".to_string());
                        let is_collapsed = collapsed_set.contains(&group_key);
                        let sub_title = match &sub_id {
                            Some(id) => sub_names.get(id).cloned().unwrap_or_else(|| self.tr("订阅节点", "Subscription nodes").to_string()),
                            None => self.tr("手动导入 / 未分类", "Manual / Uncategorized").to_string(),
                        };
                        let node_ids_for_ping: Vec<String> = group_nodes.iter().map(|n| n.id.clone()).collect();
                        let key_for_click = group_key.clone();

                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                // Subscription Group Header
                                div()
                                    .id(SharedString::from(format!("sub-header-{}", group_key)))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(p.panel)
                                    .border_1()
                                    .border_color(p.border)
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.collapsed_subs.contains(&key_for_click) {
                                            this.collapsed_subs.remove(&key_for_click);
                                        } else {
                                            this.collapsed_subs.insert(key_for_click.clone());
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(p.muted_fg)
                                                    .child(Icon::new(if is_collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).size(px(14.))),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(p.fg)
                                                    .child(sub_title),
                                            )
                                            .child(pill(
                                                format!("{} 个", group_nodes.len()),
                                                p.muted_fg,
                                                p.panel_alt,
                                            )),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!("ping-sub-{}", group_key)))
                                            .compact()
                                            .ghost()
                                            .label(self.tr("测速本组", "Test group"))
                                            .tooltip(self.tr("测试该订阅下所有节点的延迟", "Test latency of all nodes in this subscription"))
                                            .on_click(cx.listener(move |this, _, _, _| {
                                                for id in &node_ids_for_ping {
                                                    this.service.test_latency(LatencyScope::Node(id.clone()));
                                                }
                                            })),
                                    ),
                            )
                            .when(!is_collapsed, |d| {
                                d.children(group_nodes.iter().map(|node| {
                                    render_node_row(
                                        node,
                                        &p,
                                        lang,
                                        active_node.as_deref(),
                                        fastest_node_id.as_deref(),
                                        self.snapshot.latency.get(&node.id),
                                        hide_ips,
                                        group_kind,
                                        group_id_for_select.clone(),
                                        is_core_busy,
                                        this_busy(&self.snapshot.busy, "latency"),
                                        menu_open.as_deref(),
                                        batch_mode,
                                        batch_selected.contains(&node.id),
                                        cx,
                                    )
                                }))
                            })
                    }))
                }),
        );

        // QR overlay as an inline card.
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
                                        .icon(IconName::X)
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

        let _ = running;
        body
    }
}

fn this_busy(busy: &[String], key: &str) -> bool {
    busy.iter().any(|b| b == key)
}

/// Helper function to format and optionally mask host addresses for privacy.
fn format_endpoint(endpoint: &str, hide: bool) -> String {
    if hide {
        if let Some((host, port)) = endpoint.rsplit_once(':') {
            if host.starts_with('[') && host.ends_with(']') {
                format!("[••••:••••]:{port}")
            } else if host.contains('.') {
                let parts: Vec<&str> = host.split('.').collect();
                if parts.len() == 4 {
                    format!("{}.*.*.*:{}", parts[0], port)
                } else {
                    format!("••••••:{port}")
                }
            } else {
                format!("••••••:{port}")
            }
        } else {
            "••••••••".to_string()
        }
    } else if endpoint.len() > 32 {
        let start = &endpoint[..14];
        let end = &endpoint[endpoint.len() - 12..];
        format!("{start}…{end}")
    } else {
        endpoint.to_string()
    }
}

/// Render a single node row with interactive controls, radio selection, and optional batch checkbox.
fn render_node_row(
    node: &Node,
    p: &Palette,
    lang: Language,
    active_node: Option<&str>,
    fastest_node_id: Option<&str>,
    latency: Option<&xross_core::storage::LatencyEntry>,
    hide_ips: bool,
    group_kind: GroupKind,
    group_id_for_select: Option<String>,
    is_core_busy: bool,
    is_latency_busy: bool,
    menu_open: Option<&str>,
    batch_mode: bool,
    is_batch_checked: bool,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let selected = active_node == Some(node.id.as_str());
    let node_id = node.id.clone();
    let node_id_select = node.id.clone();
    let node_id_tcp = node.id.clone();
    let node_id_real = node.id.clone();
    let node_id_share = node.id.clone();
    let node_id_qr = node.id.clone();
    let node_id_del = node.id.clone();
    let node_id_menu = node.id.clone();
    let node_id_batch = node.id.clone();
    let group_for_click = group_id_for_select.clone();
    let name = node.name.clone();
    let endpoint_display = format_endpoint(&node.endpoint(), hide_ips);
    let is_fastest = fastest_node_id == Some(&node.id);
    let is_menu_open = menu_open == Some(&node.id);
    let is_node_busy = is_core_busy && selected;
    let drop_target = node.id.clone();
    let drag_primary = p.primary;
    let drag_accent = p.accent;

    let delay_label = latency
        .as_ref()
        .map(|l| match l.delay_ms {
            Some(ms) => format!("{ms} ms"),
            None => super::tr(lang, "超时", "Timeout").to_string(),
        })
        .unwrap_or_else(|| "—".into());

    let delay_color = match latency.as_ref().and_then(|l| l.delay_ms) {
        Some(ms) if ms < 150 => p.success,
        Some(ms) if ms < 350 => p.warning,
        Some(_) => p.danger,
        None => p.muted_fg,
    };

    div()
        .id(SharedString::from(format!("node-{}", node.id)))
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(if selected { p.primary } else { p.border })
        .bg(if selected { p.accent } else { p.bg })
        .overflow_hidden()
        .drag_over::<DraggedNode>(move |style, _, _, _| {
            style.border_color(drag_primary).bg(drag_accent)
        })
        .on_drop(cx.listener(move |this, dragged: &DraggedNode, _, cx| {
            if dragged.id != drop_target {
                this.service.move_node_to(&dragged.id, &drop_target);
                this.sort_by_latency = false;
                this.sync(cx);
            }
        }))
        .when(!node.enabled, |d| d.opacity(0.45))
        .child(
            div()
                .id(SharedString::from(format!("node-row-{}", node.id)))
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if batch_mode {
                        if this.batch_selected.contains(&node_id_batch) {
                            this.batch_selected.remove(&node_id_batch);
                        } else {
                            this.batch_selected.insert(node_id_batch.clone());
                        }
                        cx.notify();
                        return;
                    }
                    if selected || is_core_busy {
                        return;
                    }
                    if let Some(group) = &group_for_click {
                        this.service.select_node(group, &node_id_select);
                        this.sync(cx);
                    }
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("drag-node-{}", node.id)))
                        .flex_shrink_0()
                        .cursor_pointer()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .on_drag(
                            DraggedNode {
                                id: node.id.clone(),
                                name: node.name.clone(),
                            },
                            |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                        )
                        .child(
                            Icon::new(IconName::GripVertical)
                                .size(px(14.))
                                .text_color(p.muted_fg),
                        ),
                )
                // Batch Checkbox (when batch mode is active)
                .when(batch_mode, |d| {
                    d.child(
                        div()
                            .w(px(20.))
                            .h(px(20.))
                            .rounded_sm()
                            .border_1()
                            .border_color(if is_batch_checked {
                                p.primary
                            } else {
                                p.muted_fg
                            })
                            .bg(if is_batch_checked { p.primary } else { p.bg })
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(p.primary_fg)
                            .when(is_batch_checked, |d| {
                                d.child(Icon::new(IconName::Check).size(px(14.)))
                            }),
                    )
                })
                // Left accent bar
                .child(div().w(px(3.)).h(px(22.)).rounded_full().bg(if selected {
                    p.primary
                } else {
                    hsla(0., 0., 0., 0.)
                }))
                // Row start: Radio button indicating selected active node.
                .child(
                    div()
                        .w(px(20.))
                        .h(px(20.))
                        .rounded_full()
                        .border_1()
                        .border_color(if selected { p.primary } else { p.muted_fg })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(if selected {
                            div().size(px(10.)).rounded_full().bg(p.primary)
                        } else {
                            div()
                        }),
                )
                // Node metadata
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
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.fg)
                                        .min_w(px(0.))
                                        .truncate()
                                        .child(name),
                                )
                                .when(is_fastest, |d| {
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .flex_shrink_0()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(p.accent)
                                            .text_xs()
                                            .text_color(p.success)
                                            .child(Icon::new(IconName::Zap).size(px(12.)))
                                            .child(super::tr(lang, "最快", "Fastest")),
                                    )
                                })
                                .when(is_node_busy, |d| {
                                    d.child(pill(
                                        super::tr(lang, "切换中…", "Switching…"),
                                        p.warning,
                                        p.accent,
                                    ))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .min_w(px(0.))
                                .child(pill(
                                    node.protocol.display_name().to_uppercase(),
                                    p.primary,
                                    p.panel,
                                ))
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_color(p.muted_fg)
                                        .child(endpoint_display),
                                )
                                .child(div().text_color(p.muted_fg.opacity(0.6)).child("·"))
                                .child(
                                    div()
                                        .text_color(p.muted_fg)
                                        .child(node.transport.kind_name().to_uppercase()),
                                ),
                        ),
                )
                .when(group_kind != GroupKind::Selector, |d| {
                    d.child(pill(
                        if group_kind == GroupKind::UrlTest {
                            super::tr(lang, "自动测速中", "Testing automatically")
                        } else {
                            super::tr(lang, "备用", "Standby")
                        },
                        p.muted_fg,
                        p.panel_alt,
                    ))
                })
                // Latency column: fixed width (80px), right-aligned, mono font.
                .child(
                    div()
                        .w(px(80.))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(delay_color)
                        .child(delay_label),
                )
                // Action buttons: Ping test and More menu.
                .child(
                    div()
                        .id(SharedString::from(format!("node-actions-{}", node.id)))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .flex_shrink_0()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            Button::new(SharedString::from(format!("tcp-{node_id_tcp}")))
                                .compact()
                                .label(super::tr(lang, "测速", "Test"))
                                .tooltip(super::tr(lang, "测试 TCP 延迟", "Test TCP latency"))
                                .loading(is_latency_busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.service
                                        .test_latency(LatencyScope::Node(node_id_tcp.clone()));
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("more-{node_id_menu}")))
                                .compact()
                                .ghost()
                                .icon(IconName::Ellipsis)
                                .tooltip(super::tr(
                                    lang,
                                    "更多操作 (真连/分享/二维码/删除)",
                                    "More actions (HTTP test, share, QR, delete)",
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.node_menu_open =
                                        if this.node_menu_open.as_deref() == Some(&node_id_menu) {
                                            None
                                        } else {
                                            Some(node_id_menu.clone())
                                        };
                                    cx.notify();
                                })),
                        ),
                )
                // Row end: Enabled toggle switch, isolated with a left border divider.
                .child(
                    div()
                        .id(SharedString::from(format!("node-toggle-{}", node.id)))
                        .flex_shrink_0()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .pl_2()
                        .border_l_1()
                        .border_color(p.border)
                        .child(
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
                ),
        )
        // Expanded '⋯' menu tray
        .when(is_menu_open, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .bg(p.panel)
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        Button::new(SharedString::from(format!("real-{node_id_real}")))
                            .compact()
                            .icon(IconName::Zap)
                            .label(super::tr(lang, "测真连", "Test HTTP"))
                            .tooltip(super::tr(
                                lang,
                                "测试真实 HTTP 访问延迟 (通过节点访问 generate_204)",
                                "Test HTTP latency through this node (generate_204)",
                            ))
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.service.test_real_latency(&node_id_real);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("share-{node_id_share}")))
                            .compact()
                            .icon(IconName::Copy)
                            .label(super::tr(lang, "复制链接", "Copy link"))
                            .tooltip(super::tr(lang, "复制节点分享链接", "Copy node share link"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let link = this.service.export_links(&[node_id_share.clone()]);
                                if link.is_empty() {
                                    this.service
                                        .toast(ToastLevel::Warning, "该协议暂不支持导出链接");
                                } else {
                                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                                    this.service
                                        .toast(ToastLevel::Success, "分享链接已复制到剪贴板");
                                }
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("qr-{node_id_qr}")))
                            .compact()
                            .icon(IconName::QrCode)
                            .label(super::tr(lang, "二维码", "QR code"))
                            .tooltip(super::tr(lang, "生成并查看二维码", "Show QR code"))
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
                            .icon(IconName::Trash)
                            .label(super::tr(lang, "删除", "Delete"))
                            .tooltip(super::tr(lang, "删除此节点", "Delete this node"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.delete_node(&node_id_del);
                                this.node_menu_open = None;
                                this.sync(cx);
                            })),
                    ),
            )
        })
}
