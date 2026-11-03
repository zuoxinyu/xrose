use std::collections::{BTreeMap, HashMap};

use gpui_base::{Disableable, Selectable};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{CoreKind, GroupKind, Node};
use xross_core::service::{LatencyScope, ToastLevel};
use xross_core::settings::Language;
use xross_core::storage::LatencyEntry;
use xross_core::util::format_speed;

use super::theme::{
    LATENCY_EXCELLENT_MS, LATENCY_NORMAL_MS, LATENCY_SLOW_MS, LATENCY_STALE_SECS, Palette,
    latency_age_secs, latency_color, palette,
};
use super::widgets::*;
use super::{AppView, Page};

/// "Fastest" is only shown when it leads the runner-up by at least
/// `max(FASTEST_MIN_GAP_MS, FASTEST_MIN_GAP_RATIO * runner_up)`.
const FASTEST_MIN_GAP_MS: u32 = 30;
const FASTEST_MIN_GAP_RATIO: f32 = 0.15;

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
    pub(crate) fn page_nodes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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

        let hide_ips = self.hide_ips;

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
        // Automatic groups pick the node themselves; rows only display it.
        let auto_mode = group_kind != GroupKind::Selector;

        let has_nodes = !model.nodes.is_empty();
        let sort_active = has_nodes && self.sort_by_latency;
        let grouped = has_nodes && self.group_by_subscription;

        let search_query = if has_nodes {
            self.node_search.read(cx).value().trim().to_lowercase()
        } else {
            String::new()
        };
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

        // Sorting only changes the view; the stored order is left alone.
        if sort_active {
            display_nodes.sort_by_key(|n| {
                let ms = self.snapshot.latency.get(&n.id).and_then(|l| l.delay_ms);
                match ms {
                    Some(v) => (0, v),
                    None => (1, u32::MAX),
                }
            });
        }

        // Fastest node among all enabled nodes, only if it clearly wins.
        let fastest_node_id = clear_winner(
            model
                .nodes
                .iter()
                .filter(|n| n.enabled)
                .filter_map(|n| {
                    let ms = self.snapshot.latency.get(&n.id).and_then(|l| l.delay_ms)?;
                    (ms > 0).then(|| (n.id.clone(), ms))
                })
                .collect(),
        );

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

        let menu_open = self.node_menu_open.clone();
        let batch_mode = self.batch_mode;
        let batch_selected = self.batch_selected.clone();
        let latency_busy = this_busy(&self.snapshot.busy, "latency");
        let tests_busy = latency_busy || this_busy(&self.snapshot.busy, "download");
        let core_busy = is_core_busy;

        // Subscription map for grouping.
        let sub_names: HashMap<String, String> = self
            .snapshot
            .subscriptions
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();

        // Rows are rendered up front so flat and grouped views share them.
        let mut rows: HashMap<String, AnyElement> = HashMap::new();
        for node in display_nodes.iter() {
            let row = render_node_row(
                RowProps {
                    node,
                    lang,
                    selected: active_node.as_deref() == Some(node.id.as_str()),
                    fastest: fastest_node_id.as_deref() == Some(node.id.as_str()),
                    latency: self.snapshot.latency.get(&node.id),
                    download: self.snapshot.downloads.get(&node.id),
                    download_busy: this_busy(&self.snapshot.busy, "download"),
                    downloading: this_busy(&self.snapshot.busy, &format!("download:{}", node.id)),
                    hide_ips,
                    auto_mode,
                    group_id: group_id_for_select.clone(),
                    core_busy,
                    latency_busy: tests_busy,
                    testing: latency_busy
                        && node.enabled
                        && self.latency_pending.contains(&node.id),
                    menu_open: menu_open.as_deref() == Some(node.id.as_str()),
                    batch_mode,
                    batch_checked: batch_selected.contains(&node.id),
                    drag_enabled: !sort_active,
                },
                &p,
                window,
                cx,
            );
            rows.insert(node.id.clone(), row.into_any_element());
        }

        // Node list card (now the primary focal point of the page).
        body = body.child(
            card(&p)
                // Header: title and count on the left; mode switch, privacy, test all,
                // add node, and inline expandable actions on the right.
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
                                .flex_wrap()
                                .gap_2p5()
                                .child(
                                    div()
                                        .text_lg()
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
                                ))
                                .when(is_xray, |d| {
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .text_xs()
                                            .text_color(p.warn_text)
                                            .child(Icon::new(IconName::TriangleAlert).size(px(14.)))
                                            .child(self.tr(
                                                "Xray core 模式：切换节点需重启 core",
                                                "Xray core: switching node restarts core",
                                            )),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("toggle-add-drawer")
                                        .compact()
                                        .primary()
                                        .icon(if self.show_import_box || self.show_manual_form {
                                            IconName::X
                                        } else {
                                            IconName::Plus
                                        })
                                        .label(if self.show_import_box || self.show_manual_form {
                                            self.tr("收起", "Close")
                                        } else {
                                            self.tr("添加节点", "Add node")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let open =
                                                !(this.show_import_box || this.show_manual_form);
                                            this.show_import_box = open;
                                            this.show_manual_form = false;
                                            this.manual_error = None;
                                            cx.notify();
                                        })),
                                )
                                .when(self.toolbar_menu_open, |d| {
                                    d.child(
                                        Button::new("toggle-batch")
                                            .compact()
                                            .ghost()
                                            .selected(self.batch_mode)
                                            .icon(IconName::ListChecks)
                                            .label(if self.batch_mode {
                                                self.tr("退出批量管理", "Exit batch mode")
                                            } else {
                                                self.tr("批量管理", "Batch manage")
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.batch_mode = !this.batch_mode;
                                                if !this.batch_mode {
                                                    this.batch_selected.clear();
                                                }
                                                this.toolbar_menu_open = false;
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("clean-dead-nodes")
                                            .compact()
                                            .ghost()
                                            .icon(IconName::Broom)
                                            .label(format!(
                                                "{} ({timeout_count})",
                                                self.tr("清理超时节点", "Remove timed-out")
                                            ))
                                            .disabled(timeout_count == 0)
                                            .tooltip(self.tr(
                                                "移除所有测速超时的节点",
                                                "Remove all nodes that timed out during latency testing",
                                            ))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_clean_confirm = true;
                                                this.toolbar_menu_open = false;
                                                cx.notify();
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("toolbar-more")
                                        .compact()
                                        .ghost()
                                        .selected(self.toolbar_menu_open)
                                        .icon(if self.toolbar_menu_open {
                                            IconName::ChevronRight
                                        } else {
                                            IconName::ChevronLeft
                                        })
                                        .tooltip(if self.toolbar_menu_open {
                                            self.tr("收起更多操作", "Hide actions")
                                        } else {
                                            self.tr("更多操作", "More actions")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toolbar_menu_open = !this.toolbar_menu_open;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                // Toolbar row: Search, Mode, View, Sort <-> Test All, Privacy (right-aligned).
                .when(has_nodes, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(div().w(px(220.)).child(Input::new(&self.node_search)))
                                    .when(!groups.is_empty(), |d| {
                                        d.child(segmented(&p).children(groups.iter().map(|g| {
                                            let selected =
                                                active_group.as_deref() == Some(g.id.as_str());
                                            let gid = g.id.clone();
                                            let builtin = g.id == "proxy" || g.id == "auto";
                                            let label: SharedString = if builtin {
                                                match g.kind {
                                                    GroupKind::Selector => {
                                                        super::tr(lang, "手动", "Manual")
                                                    }
                                                    GroupKind::UrlTest => {
                                                        super::tr(lang, "自动", "Auto")
                                                    }
                                                    GroupKind::Fallback => {
                                                        super::tr(lang, "故障转移", "Fallback")
                                                    }
                                                    GroupKind::LoadBalance => {
                                                        super::tr(lang, "负载均衡", "Load balance")
                                                    }
                                                }
                                            } else {
                                                SharedString::from(g.name.clone())
                                            };
                                            let tip: SharedString = match g.kind {
                                                GroupKind::Selector => super::tr(
                                                    lang,
                                                    "手动：由你选择出站节点",
                                                    "Manual: you choose the outbound node",
                                                ),
                                                GroupKind::UrlTest => super::tr(
                                                    lang,
                                                    "自动：按定期延迟测试结果选用最快的节点",
                                                    "Auto: uses the fastest node from periodic latency tests",
                                                ),
                                                GroupKind::Fallback => super::tr(
                                                    lang,
                                                    "故障转移：当前节点不可用时依次切换到备用节点",
                                                    "Fallback: switches to the next node when the current one fails",
                                                ),
                                                GroupKind::LoadBalance => super::tr(
                                                    lang,
                                                    "负载均衡：在可用节点之间分配连接",
                                                    "Load balance: spreads connections across available nodes",
                                                ),
                                            };
                                            segment_item(
                                                &p,
                                                SharedString::from(format!("group-{}", g.id)),
                                                label,
                                                selected,
                                            )
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(tip.clone()).build(window, cx)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.selected_group = Some(gid.clone());
                                                this.refresh_header_location();
                                                cx.notify();
                                            }))
                                        })))
                                    })
                                    .child(
                                        segmented(&p)
                                            .child(
                                                segment_item(
                                                    &p,
                                                    "view-flat",
                                                    self.tr("平铺", "Flat"),
                                                    !grouped,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.group_by_subscription = false;
                                                    cx.notify();
                                                })),
                                            )
                                            .child(
                                                segment_item(
                                                    &p,
                                                    "view-grouped",
                                                    self.tr("订阅", "Subscription"),
                                                    grouped,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.group_by_subscription = true;
                                                    cx.notify();
                                                })),
                                            ),
                                    )
                                    .child(
                                        segmented(&p)
                                            .child(
                                                segment_item(
                                                    &p,
                                                    "sort-manual",
                                                    self.tr("顺序", "Order"),
                                                    !sort_active,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sort_by_latency = false;
                                                    cx.notify();
                                                })),
                                            )
                                            .child({
                                                let tip = self.tr(
                                                    "仅改变显示顺序，不影响已保存的顺序；拖拽排序只在手动顺序下可用",
                                                    "Changes the view only, not the saved order; drag to reorder works in manual order",
                                                );
                                                segment_item(
                                                    &p,
                                                    "sort-latency",
                                                    self.tr("延迟 ↑", "Latency ↑"),
                                                    sort_active,
                                                )
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(tip.clone()).build(window, cx)
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sort_by_latency = true;
                                                    cx.notify();
                                                }))
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("test-all")
                                            .compact()
                                            .ghost()
                                            .icon(IconName::Activity)
                                            .tooltip(
                                                self.tr("全部节点测速", "Test latency for all nodes"),
                                            )
                                            .disabled(tests_busy)
                                            .loading(latency_busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.latency_pending = this
                                                    .snapshot
                                                    .model
                                                    .nodes
                                                    .iter()
                                                    .filter(|n| n.enabled)
                                                    .map(|n| n.id.clone())
                                                    .collect();
                                                this.service.test_latency(LatencyScope::All);
                                                this.sync(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("toggle-privacy")
                                            .compact()
                                            .ghost()
                                            .icon(if hide_ips {
                                                IconName::EyeOff
                                            } else {
                                                IconName::Eye
                                            })
                                            .tooltip(if hide_ips {
                                                self.tr(
                                                    "已隐藏服务器地址与位置，点击显示",
                                                    "Addresses and location hidden — click to reveal",
                                                )
                                            } else {
                                                self.tr(
                                                    "隐藏服务器地址与位置",
                                                    "Hide server addresses and location",
                                                )
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.hide_ips = !this.hide_ips;
                                                let mut settings = this.snapshot.settings.clone();
                                                settings.privacy_mode = this.hide_ips;
                                                this.service.update_settings(settings);
                                                this.sync(cx);
                                            })),
                                    ),
                            ),
                    )
                })
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
                                            .disabled(selected_count == 0 || tests_busy)
                                            .label(self.tr("批量测速", "Test selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.latency_pending.extend(sel.iter().cloned());
                                                this.service.test_latency(LatencyScope::Nodes(sel.clone()));
                                                this.sync(cx);
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
                                        let del_fg = p.danger.opacity(0.72);
                                        let del_hover_fg = p.danger;
                                        let del_hover_bg = p.danger.opacity(0.14);
                                        let del_active_bg = p.danger.opacity(0.22);
                                        div()
                                            .id("batch-delete")
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .h(px(26.))
                                            .px_2p5()
                                            .rounded_md()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(del_fg)
                                            .when(selected_count == 0, |d| d.opacity(0.45))
                                            .when(selected_count > 0, |d| {
                                                d.cursor_pointer()
                                                    .hover(move |s| {
                                                        s.bg(del_hover_bg).text_color(del_hover_fg)
                                                    })
                                                    .active(move |s| {
                                                        s.bg(del_active_bg).text_color(del_hover_fg)
                                                    })
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.service.delete_nodes(&sel);
                                                        this.batch_selected.clear();
                                                        this.sync(cx);
                                                    }))
                                            })
                                            .child(Icon::new(IconName::Trash).size(px(14.)))
                                            .child(self.tr("批量删除", "Delete selected"))
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
                                                    .child(dot(p.data_down))
                                                    .child(self.tr("下行", "Down")),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(dot(p.data_up))
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
                .when(!grouped, |container| {
                    container.children(display_nodes.iter().filter_map(|node| rows.remove(&node.id)))
                })
                .when(grouped, |container| {
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
                                            .disabled(tests_busy)
                                            .tooltip(self.tr("测试该订阅下所有节点的延迟", "Test latency of all nodes in this subscription"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.latency_pending.extend(node_ids_for_ping.iter().cloned());
                                                this.service.test_latency(LatencyScope::Nodes(node_ids_for_ping.clone()));
                                                this.sync(cx);
                                            })),
                                    ),
                            )
                            .when(!is_collapsed, |d| {
                                d.children(group_nodes.iter().filter_map(|node| rows.remove(&node.id)))
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

/// Everything a node row needs; computed once per frame by the page.
struct RowProps<'a> {
    node: &'a Node,
    lang: Language,
    selected: bool,
    fastest: bool,
    latency: Option<&'a LatencyEntry>,
    download: Option<&'a xross_core::runtime::probe::DownloadResult>,
    download_busy: bool,
    downloading: bool,
    hide_ips: bool,
    auto_mode: bool,
    group_id: Option<String>,
    core_busy: bool,
    latency_busy: bool,
    testing: bool,
    menu_open: bool,
    batch_mode: bool,
    batch_checked: bool,
    drag_enabled: bool,
}

/// The fastest node, but only if it leads the runner-up by at least
/// `max(30 ms, 15 %)`. Smaller gaps are measurement noise and flip on the
/// next run, so no node is marked.
fn clear_winner(mut measured: Vec<(String, u32)>) -> Option<String> {
    measured.sort_by_key(|(_, ms)| *ms);
    let (best_id, best) = measured.first()?;
    let (_, second) = measured.get(1)?;
    let threshold = FASTEST_MIN_GAP_MS.max((*second as f32 * FASTEST_MIN_GAP_RATIO) as u32);
    (second.saturating_sub(*best) >= threshold).then(|| best_id.clone())
}

fn age_text(lang: Language, secs: i64) -> String {
    let zh = lang == Language::Zh;
    match secs {
        s if s < 60 => {
            if zh {
                "刚刚测速".into()
            } else {
                "Tested just now".into()
            }
        }
        s if s < 3600 => {
            let n = s / 60;
            if zh {
                format!("{n} 分钟前测速")
            } else {
                format!("Tested {n} min ago")
            }
        }
        s if s < 86_400 => {
            let n = s / 3600;
            if zh {
                format!("{n} 小时前测速")
            } else {
                format!("Tested {n} h ago")
            }
        }
        s => {
            let n = s / 86_400;
            if zh {
                format!("{n} 天前测速")
            } else {
                format!("Tested {n} d ago")
            }
        }
    }
}

/// Render a single node row: flush selection bar, radio, metadata, latency and
/// hover-revealed actions.
fn render_node_row(
    props: RowProps<'_>,
    p: &Palette,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let RowProps {
        node,
        lang,
        selected,
        fastest: is_fastest,
        latency,
        download,
        download_busy,
        downloading,
        hide_ips,
        auto_mode,
        group_id,
        core_busy: is_core_busy,
        latency_busy: is_latency_busy,
        testing,
        menu_open: is_menu_open,
        batch_mode,
        batch_checked: is_batch_checked,
        drag_enabled,
    } = props;
    let node_id = node.id.clone();
    let node_id_select = node.id.clone();
    let node_id_tcp = node.id.clone();
    let node_id_real = node.id.clone();
    let node_id_download = node.id.clone();
    let node_id_share = node.id.clone();
    let node_id_qr = node.id.clone();
    let node_id_del = node.id.clone();
    let node_id_menu = node.id.clone();
    let node_id_batch = node.id.clone();
    let group_for_click = group_id;
    let name = node.name.clone();
    let endpoint_display = format_endpoint(&node.endpoint(), hide_ips);
    let is_node_busy = is_core_busy && selected;
    let drop_target = node.id.clone();
    let drag_primary = p.primary;
    let drag_accent = p.accent;
    let hover_bg = p.panel_alt.opacity(0.5);
    let hover_group = SharedString::from(format!("node-group-{}", node.id));

    // The test button stays hidden until the row is hovered or the button has
    // keyboard focus; the focus handle is kept per row across frames.
    let tcp_focus = window
        .use_keyed_state(
            SharedString::from(format!("tcp-focus-{}", node.id)),
            cx,
            |_, cx| cx.focus_handle(),
        )
        .read(cx)
        .clone();
    let tcp_focused = tcp_focus.is_focused(window);

    let delay_label = latency
        .map(|l| match l.delay_ms {
            Some(ms) => format!("{ms} ms"),
            None => super::tr(lang, "超时", "Timeout").to_string(),
        })
        .unwrap_or_else(|| "—".into());
    let delay_color = latency_color(p, latency);
    let stale = latency.is_some_and(|l| latency_age_secs(l) > LATENCY_STALE_SECS);
    let delay_tip: SharedString = match latency {
        None => super::tr(lang, "尚未测速", "Not tested yet"),
        Some(entry) => {
            let grades = if lang == Language::Zh {
                format!(
                    "分级：<{LATENCY_EXCELLENT_MS} 优 · <{LATENCY_NORMAL_MS} 正常 · <{LATENCY_SLOW_MS} 偏慢 · 其余较差（ms）"
                )
            } else {
                format!(
                    "Grades: <{LATENCY_EXCELLENT_MS} excellent · <{LATENCY_NORMAL_MS} normal · <{LATENCY_SLOW_MS} slow · else poor (ms)"
                )
            };
            SharedString::from(format!(
                "{}\n{}\n{grades}",
                age_text(lang, latency_age_secs(entry)),
                match entry.detail.as_deref() {
                    Some("HTTP 延迟") => super::tr(lang, "HTTP 延迟", "HTTP latency").to_string(),
                    Some("TCP 延迟") => super::tr(lang, "TCP 延迟", "TCP latency").to_string(),
                    detail => detail.unwrap_or_default().to_string(),
                }
            ))
        }
    };

    let radio_border = if auto_mode && selected {
        p.muted_fg
    } else if auto_mode {
        p.border
    } else if selected {
        p.primary
    } else {
        p.muted_fg
    };
    let radio_dot = if auto_mode { p.muted_fg } else { p.primary };

    div()
        .id(SharedString::from(format!("node-{}", node.id)))
        .group(hover_group.clone())
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(p.border)
        .bg(if selected { p.accent } else { p.bg })
        .overflow_hidden()
        .when(!selected, |d| d.hover(move |s| s.bg(hover_bg)))
        .drag_over::<DraggedNode>(move |style, _, _, _| {
            style.border_color(drag_primary).bg(drag_accent)
        })
        .on_drop(cx.listener(move |this, dragged: &DraggedNode, _, cx| {
            if dragged.id != drop_target {
                this.service.move_node_to(&dragged.id, &drop_target);
                this.sync(cx);
            }
        }))
        .when(!node.enabled, |d| d.opacity(0.45))
        // Selection bar: inset vertically by the corner radius so it never
        // protrudes past the rounded corners of the row card.
        .when(selected, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(1.))
                    .top(px(7.))
                    .bottom(px(7.))
                    .w(px(3.))
                    .rounded_full()
                    .bg(p.primary),
            )
        })
        .child(
            div()
                .id(SharedString::from(format!("node-row-{}", node.id)))
                .flex()
                .items_center()
                .gap_3()
                .pl_4()
                .pr_3()
                .py_3()
                .when(batch_mode || !auto_mode, |d| d.cursor_pointer())
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
                    // Automatic groups choose for themselves.
                    if auto_mode || selected || is_core_busy {
                        return;
                    }
                    if let Some(group) = &group_for_click {
                        this.service.select_node(group, &node_id_select);
                        this.sync(cx);
                    }
                }))
                // Drag handle: dimmed until hover, absent while a sort is applied.
                .child(if drag_enabled {
                    div()
                        .id(SharedString::from(format!("drag-node-{}", node.id)))
                        .flex_shrink_0()
                        .cursor_pointer()
                        .opacity(0.35)
                        .group_hover(hover_group.clone(), |s| s.opacity(1.))
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
                        )
                        .into_any_element()
                } else {
                    div().w(px(14.)).flex_shrink_0().into_any_element()
                })
                // Batch checkbox (when batch mode is active).
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
                // Radio: disabled in automatic groups, where the chosen node is only marked.
                .child(
                    div()
                        .w(px(20.))
                        .h(px(20.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .border_color(radio_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(if selected {
                            div().size(px(10.)).rounded_full().bg(radio_dot)
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
                                .when(selected && auto_mode, |d| {
                                    d.child(pill(
                                        super::tr(lang, "当前选用", "In use"),
                                        p.primary_fg,
                                        p.primary,
                                    ))
                                })
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
                                            .bg(p.panel)
                                            .text_xs()
                                            .text_color(p.ok_text)
                                            .child(Icon::new(IconName::Zap).size(px(12.)))
                                            .child(super::tr(lang, "最快", "Fastest")),
                                    )
                                })
                                .when(is_node_busy, |d| {
                                    d.child(pill(
                                        super::tr(lang, "切换中…", "Switching…"),
                                        p.warn_text,
                                        p.panel,
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
                // Latency column: fixed width, right-aligned, mono. A spinner replaces
                // the value while this node is being measured; old values are dimmed.
                .child(
                    div()
                        .id(SharedString::from(format!("delay-{}", node.id)))
                        .w(px(84.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(delay_color)
                        .when(stale && !testing, |d| d.opacity(0.55))
                        .tooltip(move |window, cx| {
                            Tooltip::new(delay_tip.clone()).build(window, cx)
                        })
                        .child(if testing {
                            Spinner::new().small().color(p.muted_fg).into_any_element()
                        } else {
                            div().child(delay_label).into_any_element()
                        }),
                )
                .when(downloading || download.is_some(), |d| {
                    d.child(
                        div()
                            .w(px(108.))
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(p.muted_fg)
                            .child(if downloading {
                                super::tr(lang, "下载测速中", "Downloading").to_string()
                            } else if let Some(result) = download {
                                if result.error.is_some() {
                                    super::tr(lang, "下载失败", "Download failed").to_string()
                                } else {
                                    format!(
                                        "{:.2} MiB/s",
                                        result.bytes_per_second as f64 / 1048576.0
                                    )
                                }
                            } else {
                                String::new()
                            }),
                    )
                })
                // Actions: the test button is revealed by hover or keyboard focus.
                .child(
                    div()
                        .id(SharedString::from(format!("node-actions-{}", node.id)))
                        .flex()
                        .items_center()
                        .gap_1()
                        .flex_shrink_0()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .when(!tcp_focused, |d| {
                                    d.opacity(0.)
                                        .group_hover(hover_group.clone(), |s| s.opacity(1.))
                                })
                                .child(
                                    Button::new(SharedString::from(format!("tcp-{node_id_tcp}")))
                                        .compact()
                                        .ghost()
                                        .icon(IconName::Activity)
                                        .tooltip(super::tr(lang, "测速", "Test latency"))
                                        .disabled(is_latency_busy)
                                        .track_focus(&tcp_focus)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.latency_pending.clear();
                                            this.latency_pending.insert(node_id_tcp.clone());
                                            this.service.test_latency(LatencyScope::Node(
                                                node_id_tcp.clone(),
                                            ));
                                            this.sync(cx);
                                        })),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("more-{node_id_menu}")))
                                .compact()
                                .ghost()
                                .icon(IconName::Ellipsis)
                                .tooltip(super::tr(lang, "更多操作", "More actions"))
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
                // Row end: enabled toggle, separated by a hairline.
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
        // Expanded '⋯' menu tray: stretches the row container with the same background.
        .when(is_menu_open, |d| {
            let del_fg = p.danger.opacity(0.72);
            let del_hover_fg = p.danger;
            let del_hover_bg = p.danger.opacity(0.14);
            let del_active_bg = p.danger.opacity(0.22);
            let del_tip = super::tr(lang, "删除此节点", "Delete this node");
            d.child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(if selected {
                        p.primary.opacity(0.16)
                    } else {
                        p.border
                    })
                    .child(
                        Button::new(SharedString::from(format!("real-{node_id_real}")))
                            .compact()
                            .icon(IconName::Zap)
                            .label(super::tr(lang, "HTTP 测速", "HTTP Test"))
                            .disabled(is_latency_busy)
                            .tooltip(super::tr(
                                lang,
                                "通过节点发起 HTTP 请求测试实际连通延迟",
                                "Measure end-to-end HTTP latency through this node",
                            ))
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.latency_pending.clear();
                                this.latency_pending.insert(node_id_real.clone());
                                this.service.test_real_latency(&node_id_real);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("download-{node_id_download}")))
                            .compact()
                            .icon(IconName::Download)
                            .label(super::tr(lang, "下载测速", "Download test"))
                            .loading(downloading)
                            .disabled(download_busy || is_latency_busy || !node.enabled)
                            .tooltip(super::tr(
                                lang,
                                "通过此节点下载测试，最多 10 秒或 50 MiB",
                                "Download through this node for up to 10 seconds or 50 MiB",
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.test_download(&node_id_download);
                                this.sync(cx);
                            })),
                    )
                    .children(download.map(|result| {
                        let label = if result.error.is_some() {
                            super::tr(lang, "下载失败", "Download failed").to_string()
                        } else {
                            format!("{:.2} MiB/s", result.bytes_per_second as f64 / 1048576.0)
                        };
                        div().text_sm().text_color(p.muted_fg).child(label)
                    }))
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
                        div()
                            .id(SharedString::from(format!("del-{node_id_del}")))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .h(px(26.))
                            .px_2p5()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(del_fg)
                            .hover(move |s| s.bg(del_hover_bg).text_color(del_hover_fg))
                            .active(move |s| s.bg(del_active_bg).text_color(del_hover_fg))
                            .tooltip(move |window, cx| {
                                Tooltip::new(del_tip.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.delete_node(&node_id_del);
                                this.node_menu_open = None;
                                this.sync(cx);
                            }))
                            .child(Icon::new(IconName::Trash).size(px(14.)))
                            .child(super::tr(lang, "删除", "Delete")),
                    ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::clear_winner;

    fn nodes(values: &[u32]) -> Vec<(String, u32)> {
        values
            .iter()
            .enumerate()
            .map(|(i, ms)| (format!("n{i}"), *ms))
            .collect()
    }

    #[test]
    fn close_latencies_have_no_fastest_marker() {
        assert_eq!(clear_winner(nodes(&[203, 209])), None);
        // 30 ms apart but under 15 % of the runner-up.
        assert_eq!(clear_winner(nodes(&[300, 330])), None);
    }

    #[test]
    fn clear_lead_is_marked() {
        assert_eq!(clear_winner(nodes(&[100, 200])), Some("n0".into()));
        // Absolute floor of 30 ms applies to fast nodes.
        assert_eq!(clear_winner(nodes(&[80, 40])), Some("n1".into()));
    }

    #[test]
    fn a_single_measurement_has_nothing_to_beat() {
        assert_eq!(clear_winner(nodes(&[50])), None);
        assert_eq!(clear_winner(Vec::new()), None);
    }
}
