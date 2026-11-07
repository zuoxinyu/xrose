mod direct_entry;
mod mesh_exits;
mod outbound_groups;
mod switching;

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_base::{Disableable, Selectable};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants, DropdownButton};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{CoreKind, Node};
use xrose_core::service::{LatencyScope, ToastLevel};
use xrose_core::settings::Language;
use xrose_core::storage::LatencyEntry;

use super::menus::{self, Command, MenuTarget};
use super::theme::{
    LATENCY_EXCELLENT_MS, LATENCY_NORMAL_MS, LATENCY_SLOW_MS, LATENCY_STALE_SECS, Palette,
    latency_age_secs, latency_color, palette,
};
use super::widgets::*;
use super::{AppView, Page};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};

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
    fn copy_selected_links(&mut self, selected: &[String], cx: &mut Context<Self>) {
        let links = self.service.export_links(selected);
        let count = links.lines().count();
        if count > 0 {
            cx.write_to_clipboard(ClipboardItem::new_string(links));
        }
        let mut message = if self.lang() == Language::Zh {
            format!("已复制 {count} 条分享链接")
        } else {
            format!("Copied {count} share links")
        };
        if count < selected.len() {
            let skipped = selected.len() - count;
            message.push_str(&format!(
                " · {} {skipped}",
                self.tr("无法导出", "Could not export")
            ));
        }
        let level = if count == 0 {
            ToastLevel::Error
        } else if count < selected.len() {
            ToastLevel::Warning
        } else {
            ToastLevel::Success
        };
        self.service.toast(level, message);
        self.sync(cx);
    }

    pub(crate) fn page_nodes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = self.snapshot.model.clone();
        let exits = mesh_exits::choices(&self.snapshot.mesh, &model, self.hide_ips, lang);
        let has_nodes = !model.nodes.is_empty() || !exits.is_empty();
        let active_group = model.default_group_id();
        let group = active_group
            .as_ref()
            .and_then(|id| model.group(id))
            .cloned();
        let active_node = self
            .snapshot
            .selected_node(active_group.as_deref())
            .map(|n| n.id.clone());
        let is_xray = self.snapshot.settings.core == CoreKind::Xray;
        let is_core_busy = this_busy(&self.snapshot.busy, "core");

        let mut body = div().flex().flex_col().gap_4().w_full();

        // First run / empty state wizard.
        if !has_nodes {
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
                                self.tr("欢迎使用 Xrose", "Welcome to Xrose"),
                                self.tr(
                                    "下载 sing-box 并点击「启动」，即可查看流量、连接和网络诊断。如需代理，可添加订阅、粘贴节点链接或二维码，或拖入二维码图片。",
                                    "Install sing-box and click Start to view traffic, connections and network diagnostics. To use a proxy, add a subscription, paste node links or a QR code, or drop a QR image.",
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("wizard-install")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .primary()
                                    .label(self.tr("下载 Core", "Install core"))
                                    .on_click(cx.listener(|this, _, _, _| {
                                        let core = this.snapshot.settings.core;
                                        this.service.install_core_update(core);
                                    })),
                            )
                            .child(
                                Button::new("wizard-subscription")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .label(self.tr("添加订阅", "Add subscription"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.page = Page::Subscriptions;
                                        this.open_subscription_dialog(window,cx);
                                    })),
                            )
                            .child(
                                Button::new("wizard-manual")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .label(self.tr("手动添加节点", "Add manually"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_node_dialog(None, true, window, cx);
                                    })),
                            ),
                    ),
            );
        }

        let hide_ips = self.hide_ips;

        // Filter and sort nodes.
        let selectable_members: HashSet<_> = group
            .iter()
            .filter(|group| group.enabled && group.manually_selectable())
            .flat_map(|group| &group.members)
            .collect();
        let auto_mode = group.as_ref().is_some_and(|g| !g.manually_selectable());

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

        let batch_mode = self.batch_mode;
        let batch_selected = self.batch_selected.clone();
        let latency_busy = this_busy(&self.snapshot.busy, "latency");
        let tests_busy = latency_busy
            || this_busy(&self.snapshot.busy, "download")
            || this_busy(&self.snapshot.busy, "auto-probe");
        let exit_busy = this_busy(&self.snapshot.busy, "mesh-exit");
        let core_busy = is_core_busy || exit_busy;
        let exit_count = exits.len();
        let mut exit_rows = Vec::new();
        for exit in exits
            .iter()
            .filter(|exit| exit.matches(&search_query, hide_ips))
        {
            let disabled = core_busy
                || this_busy(&self.snapshot.busy, "mesh")
                || auto_mode
                || batch_mode
                || active_group
                    .as_ref()
                    .is_none_or(|id| !model.can_select_mesh_exit(id, &exit.gateway));
            exit_rows.push(exit.row(active_group.clone(), disabled, hide_ips, lang, &p, cx));
        }
        let shown = display_nodes.len() + exit_rows.len();

        // Subscription map for grouping.
        let sub_names: HashMap<String, String> = self
            .snapshot
            .subscriptions
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();

        // Rows are rendered up front so flat and grouped views share them.
        let mut rows: HashMap<String, AnyElement> = HashMap::new();
        let offline_exits: std::collections::HashSet<_> = self
            .snapshot
            .mesh
            .exits()
            .filter(|(_, peer, _)| peer.exit_selected && !peer.online)
            .map(|(profile, _, _)| format!("mesh-exit-{}", profile.id))
            .collect();
        for node in display_nodes.iter() {
            let row = render_node_row(
                RowProps {
                    node,
                    lang,
                    selected: active_node.as_deref() == Some(node.id.as_str()),
                    fastest: fastest_node_id.as_deref() == Some(node.id.as_str()),
                    latency: self.snapshot.latency.get(&node.id),
                    location: self.snapshot.node_location(node),
                    download: self.snapshot.downloads.get(&node.id),
                    downloading: this_busy(&self.snapshot.busy, &format!("download:{}", node.id)),
                    hide_ips,
                    auto_mode,
                    offline_exit: offline_exits.contains(&node.id),
                    group_id: active_group
                        .as_ref()
                        .filter(|_| {
                            selectable_members.contains(&node.id)
                                && node.usable_with(self.snapshot.settings.core)
                        })
                        .cloned(),
                    core_busy,
                    latency_busy: tests_busy,
                    testing: latency_busy
                        && node.enabled
                        && self.latency_pending.contains(&node.id),
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
                                        format!("{} {}", shown, self.tr("个", ""))
                                    } else {
                                        format!(
                                            "{}/{} {}",
                                            shown,
                                            model.nodes.len() + exit_count,
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
                                                "Xray core 模式：切换节点需重载内核",
                                                "Xray core: switching nodes reloads the core",
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
                                    more_button(
                                        "toolbar-more",
                                        self.tr("更多操作", "More actions"),
                                    )
                                    .dropdown_menu(
                                        menus::builder(cx.entity().downgrade(), MenuTarget::Nodes),
                                    ),
                                )
                                .child(
                                    Button::new("toggle-add-drawer")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label(self.tr("添加节点", "Add node"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_node_dialog(None, false, window, cx);
                                        })),
                                ),
                        ),
                )
                .when(has_nodes, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                ChoiceGroup::new(
                                    "node-view",
                                    vec![self.tr("平铺", "Flat"), self.tr("订阅", "Subscription")],
                                    usize::from(grouped),
                                )
                                .on_change(cx.listener(
                                    |this, index, _, cx| {
                                        this.group_by_subscription = *index == 1;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                ChoiceGroup::new(
                                    "node-sort",
                                    vec![self.tr("顺序", "Order"), self.tr("延迟 ↑", "Latency ↑")],
                                    usize::from(sort_active),
                                )
                                .on_change(cx.listener(
                                    |this, index, _, cx| {
                                        this.sort_by_latency = *index == 1;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(super::theme::FORM_FIELD_WIDTH))
                                    .child(Input::new(&self.node_search)),
                            )
                            .child(
                                Button::new("test-all")
                                    .ghost()
                                    .icon(IconName::Gauge)
                                    .tooltip(self.tr("全部节点测速", "Test all nodes"))
                                    .accessibility_label(self.tr("全部节点测速", "Test all nodes"))
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
                                Button::new("locate-all")
                                    .ghost()
                                    .icon(IconName::MapPin)
                                    .tooltip(self.tr(
                                        "探测全部启用节点的服务器归属地",
                                        "Locate servers for all enabled nodes",
                                    ))
                                    .accessibility_label(
                                        self.tr("全部探测归属地", "Locate all nodes"),
                                    )
                                    .disabled(
                                        this_busy(&self.snapshot.busy, "node-locations")
                                            || !model.nodes.iter().any(|node| node.enabled),
                                    )
                                    .loading(this_busy(&self.snapshot.busy, "node-locations"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.service.request_all_node_locations();
                                        this.sync(cx);
                                    })),
                            )
                            .child(
                                Button::new("toggle-traffic-chart")
                                    .ghost()
                                    .selected(self.hide_traffic_chart)
                                    .icon(IconName::ChartLine)
                                    .tooltip(if self.hide_traffic_chart {
                                        self.tr("显示流量折线图", "Show traffic chart")
                                    } else {
                                        self.tr("隐藏流量折线图", "Hide traffic chart")
                                    })
                                    .accessibility_label(if self.hide_traffic_chart {
                                        self.tr("显示流量折线图", "Show traffic chart")
                                    } else {
                                        self.tr("隐藏流量折线图", "Hide traffic chart")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.hide_traffic_chart = !this.hide_traffic_chart;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("toggle-privacy")
                                    .ghost()
                                    .selected(hide_ips)
                                    .icon(if hide_ips {
                                        IconName::EyeOff
                                    } else {
                                        IconName::Eye
                                    })
                                    .tooltip(
                                        self.tr(
                                            "隐藏服务器地址与位置",
                                            "Hide addresses and location",
                                        ),
                                    )
                                    .accessibility_label(self.tr("隐私模式", "Privacy mode"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.hide_ips = !this.hide_ips;
                                        let mut settings = this.snapshot.settings.clone();
                                        settings.privacy_mode = this.hide_ips;
                                        this.service.update_settings(settings);
                                        this.sync(cx);
                                    })),
                            ),
                    )
                })
                // Batch Operation Bar (Visible when batch_mode is active).
                .when(batch_mode, |d| {
                    let all_ids: Vec<String> = display_nodes.iter().map(|n| n.id.clone()).collect();
                    let all_selected =
                        !all_ids.is_empty() && all_ids.iter().all(|id| batch_selected.contains(id));
                    let selected_ids: Vec<String> = model
                        .nodes
                        .iter()
                        .filter(|n| batch_selected.contains(&n.id))
                        .map(|n| n.id.clone())
                        .collect();
                    let selected_count = selected_ids.len();

                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .flex_wrap()
                            .gap_2()
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
                                            .child(format!(
                                                "{} {selected_count} / {}",
                                                self.tr("已选择", "Selected"),
                                                display_nodes.len()
                                            )),
                                    )
                                    .child(
                                        Button::new("batch-select-all")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .ghost()
                                            .label(if all_selected {
                                                self.tr("取消全选", "Deselect all")
                                            } else {
                                                self.tr("全选当前", "Select all")
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if all_selected {
                                                    this.batch_selected.clear();
                                                } else {
                                                    this.batch_selected =
                                                        all_ids.iter().cloned().collect();
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
                                    .flex_wrap()
                                    .child({
                                        let selected = selected_ids.clone();
                                        Button::new("batch-export-links")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .disabled(selected_count == 0)
                                            .label(self.tr("复制分享链接", "Copy share links"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.copy_selected_links(&selected, cx);
                                            }))
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        DropdownButton::new("batch-test")
                                            .disabled(selected_count == 0 || tests_busy)
                                            .menu_accessibility_label(
                                                self.tr("更多批量测速方式", "More batch tests"),
                                            )
                                            .dropdown_menu(menus::builder(
                                                cx.entity().downgrade(),
                                                MenuTarget::BatchTests(selected_ids.clone()),
                                            ))
                                            .button(
                                                Button::new("batch-ping")
                                                    .compact()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .label(
                                                        self.tr("批量 TCP 测速", "Batch TCP test"),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.latency_pending
                                                                .extend(sel.iter().cloned());
                                                            this.service.test_latency(
                                                                LatencyScope::Nodes(sel.clone()),
                                                            );
                                                            this.sync(cx);
                                                        },
                                                    )),
                                            )
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-enable")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                                            .ghost()
                                            .icon(IconName::Trash)
                                            .label(self.tr("批量删除", "Delete selected"))
                                            .disabled(selected_count == 0)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.run_command(
                                                    Command::DeleteNodes(sel.clone()),
                                                    false,
                                                    window,
                                                    cx,
                                                )
                                            }))
                                    })
                                    .child(
                                        Button::new("batch-exit")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                .when(!self.hide_traffic_chart, |d| {
                    d.child(self.traffic_chart(&p))
                })
                .child(self.direct_entry(
                    &p,
                    auto_mode && model.has_usable_default_group(self.snapshot.settings.core),
                    core_busy,
                    cx,
                ))
                .when(exit_busy, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(Spinner::new())
                            .child(self.tr(
                                "正在重新连接组网并切换出口…",
                                "Reconnecting the mesh and switching exit…",
                            )),
                    )
                })
                .when(shown == 0 && !search_query.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr("未找到匹配的节点", "No matching nodes found"),
                    ))
                })
                // Node list: either Grouped by Subscription or Flat List.
                .when(!grouped, |container| {
                    container.children(
                        display_nodes
                            .iter()
                            .filter_map(|node| rows.remove(&node.id)),
                    )
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
                            Some(id) => sub_names.get(id).cloned().unwrap_or_else(|| {
                                self.tr("订阅节点", "Subscription nodes").to_string()
                            }),
                            None => self
                                .tr("手动导入 / 未分类", "Manual / Uncategorized")
                                .to_string(),
                        };
                        let node_ids_for_ping: Vec<String> =
                            group_nodes.iter().map(|n| n.id.clone()).collect();
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
                                                div().text_xs().text_color(p.muted_fg).child(
                                                    Icon::new(if is_collapsed {
                                                        IconName::ChevronRight
                                                    } else {
                                                        IconName::ChevronDown
                                                    })
                                                    .size(px(14.)),
                                                ),
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
                                        Button::new(SharedString::from(format!(
                                            "ping-sub-{}",
                                            group_key
                                        )))
                                        .compact()
                                        .ghost()
                                        .label(self.tr("测速本组", "Test group"))
                                        .disabled(tests_busy)
                                        .tooltip(self.tr(
                                            "测试该订阅下所有节点的延迟",
                                            "Test latency of all nodes in this subscription",
                                        ))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.latency_pending
                                                    .extend(node_ids_for_ping.iter().cloned());
                                                this.service.test_latency(LatencyScope::Nodes(
                                                    node_ids_for_ping.clone(),
                                                ));
                                                this.sync(cx);
                                            }),
                                        ),
                                    ),
                            )
                            .when(!is_collapsed, |d| {
                                d.children(
                                    group_nodes.iter().filter_map(|node| rows.remove(&node.id)),
                                )
                            })
                    }))
                })
                .children(exit_rows),
        );

        body
    }
}

fn this_busy(busy: &[String], key: &str) -> bool {
    busy.iter().any(|b| b == key)
}

/// Helper function to format and optionally mask host addresses for privacy.
pub(super) fn format_endpoint(endpoint: &str, hide: bool) -> String {
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
    location: Option<&'a xrose_core::runtime::location::NodeLocation>,
    download: Option<&'a xrose_core::runtime::probe::DownloadResult>,
    downloading: bool,
    hide_ips: bool,
    auto_mode: bool,
    offline_exit: bool,
    group_id: Option<String>,
    core_busy: bool,
    latency_busy: bool,
    testing: bool,
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
        location,
        download,
        downloading,
        hide_ips,
        auto_mode,
        offline_exit,
        group_id,
        core_busy: is_core_busy,
        latency_busy: is_latency_busy,
        testing,
        batch_mode,
        batch_checked: is_batch_checked,
        drag_enabled,
    } = props;
    let node_id = node.id.clone();
    let node_id_select = node.id.clone();
    let node_id_tcp = node.id.clone();
    let node_id_batch = node.id.clone();
    let can_select = group_id.is_some() && !is_core_busy && !offline_exit;
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

    let delay_label = super::theme::latency_label(latency, false, lang);
    let location_label = location.filter(|_| !hide_ips).map(|entry| {
        if entry.pending {
            super::tr(lang, "归属地探测中", "Locating server").to_string()
        } else {
            entry
                .label
                .clone()
                .unwrap_or_else(|| super::tr(lang, "归属地未知", "Location unknown").to_string())
        }
    });
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
    } else if !can_select {
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
        .border_color(if selected {
            p.border.opacity(0.)
        } else {
            p.border
        })
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
        .when(!node.enabled || offline_exit, |d| d.opacity(0.45))
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
                .min_h(px(super::theme::NODE_ROW_HEIGHT))
                .py_2()
                .when(batch_mode || can_select, |d| d.cursor_pointer())
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
                    if !can_select || selected {
                        return;
                    }
                    if let Some(group) = &group_id {
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
                .child(radio_indicator(selected, radio_border, radio_dot))
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
                                .child(pill(
                                    if node.id.starts_with("mesh-exit-") {
                                        super::tr(lang, "组网出口", "Mesh exit").to_string()
                                    } else {
                                        node.protocol.display_name().to_uppercase()
                                    },
                                    p.primary,
                                    p.panel,
                                ))
                                .when(offline_exit, |d| {
                                    d.child(pill(
                                        super::tr(lang, "离线", "Offline"),
                                        p.muted_fg,
                                        p.panel_alt,
                                    ))
                                })
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
                                )
                                .when_some(location_label, |d, label| {
                                    let tip = label.clone();
                                    d.child(div().text_color(p.muted_fg.opacity(0.6)).child("·"))
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "node-location-{}",
                                                    node.id
                                                )))
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_color(p.muted_fg)
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(tip.clone()).build(window, cx)
                                                })
                                                .child(label),
                                        )
                                }),
                        ),
                )
                // Download speed column: fixed width, right-aligned, mono. Placed to the left of latency.
                .child(
                    div()
                        .w(px(96.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(p.muted_fg)
                        .child(if downloading {
                            Spinner::new().small().color(p.muted_fg).into_any_element()
                        } else if let Some(result) = download {
                            if result.error.is_some() {
                                div()
                                    .text_xs()
                                    .text_color(p.danger)
                                    .child(super::tr(lang, "下载失败", "Failed"))
                                    .into_any_element()
                            } else {
                                div()
                                    .child(format!(
                                        "{:.2} MiB/s",
                                        result.bytes_per_second as f64 / 1048576.0
                                    ))
                                    .into_any_element()
                            }
                        } else {
                            div().into_any_element()
                        }),
                )
                // Latency column: fixed width, right-aligned, mono. A spinner replaces
                // the value while this node is being measured; old values are dimmed.
                .child(
                    div()
                        .id(SharedString::from(format!("delay-{}", node.id)))
                        .w(px(80.))
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
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .ghost()
                                        .icon(IconName::Gauge)
                                        .tooltip(super::tr(lang, "测速", "Test latency"))
                                        .accessibility_label(super::tr(
                                            lang,
                                            "节点测速",
                                            "Test node latency",
                                        ))
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
                            more_button(
                                SharedString::from(format!("more-{}", node.id)),
                                super::tr(lang, "更多操作", "More actions"),
                            )
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                MenuTarget::Node(node.id.clone()),
                            )),
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
                            .tooltip(super::tr(
                                lang,
                                "允许此节点参与连接与自动选择",
                                "Allow this node for connections and automatic selection",
                            ))
                            .accessibility_label(super::tr(lang, "启用节点", "Enable node"))
                            .on_change(cx.listener(
                                move |this, checked, _, cx| {
                                    this.service.set_node_enabled(&node_id, *checked);
                                    this.sync(cx);
                                },
                            )),
                        ),
                ),
        )
        .context_menu(menus::builder(
            cx.entity().downgrade(),
            MenuTarget::Node(node.id.clone()),
        ))
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
