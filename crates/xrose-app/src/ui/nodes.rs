mod direct_entry;
mod health_detail;
mod mesh_exits;
mod outbound_groups;
mod policy_popover;
mod switching;

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{AppModel, CoreKind, Node, OutboundGroup, WorkMode};
use xrose_core::service::{HttpTestTarget, ToastLevel, UiSnapshot};
use xrose_core::settings::Language;
use xrose_core::storage::LatencyEntry;

use super::menus::{self, Command, MenuTarget};
use super::theme::{
    LATENCY_STALE_SECS, Palette, SEMANTIC_ALPHA, latency_age_secs, latency_color, palette,
};
use super::widgets::*;
use super::{AppView, NodeSort, NodeView, Page};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NodeSection<'a> {
    Manual,
    Chains,
    Subscription(&'a str),
    MeshExits,
}

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
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = &self.snapshot.model;
        let exits = mesh_exits::choices(&self.snapshot.mesh, model, self.hide_ips, lang);
        let discovered_gateways: HashSet<_> =
            exits.iter().map(|exit| exit.gateway.clone()).collect();
        let has_nodes = !model.nodes.is_empty() || !exits.is_empty();
        let active_group = model.default_group_id();
        let group = active_group.as_ref().and_then(|id| model.group(id));
        let active_node = self
            .snapshot
            .selected_node(active_group.as_deref())
            .map(|n| n.id.clone());
        let is_xray = self.snapshot.settings.core == CoreKind::Xray;
        let is_core_busy = this_busy(&self.snapshot.busy, "core");

        let mut body = div().flex().flex_col().gap_4().w_full();

        // First run / empty state wizard.
        if !has_nodes {
            let core_available = self
                .snapshot
                .core_binary_available(self.snapshot.settings.core);
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
                                    "配置好内核后，点击「启动」即可查看流量、连接和网络诊断。如需代理，可添加订阅、粘贴节点链接或二维码，或拖入二维码图片。",
                                    "Set up a core and click Start to view traffic, connections and network diagnostics. To use a proxy, add a subscription, paste node links or a QR code, or drop a QR image.",
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .when(!core_available, |row| row.child(
                                Button::new("wizard-install")
                                    .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .primary()
                                    .label(self.tr("下载内核", "Install core"))
                                    .on_click(cx.listener(|this, _, _, _| {
                                        let core = this.snapshot.settings.core;
                                        this.service.install_core_update(core);
                                    })),
                            ))
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
            .flat_map(|group| group.members.iter().cloned())
            .collect();
        let auto_mode = group.as_ref().is_some_and(|g| !g.manually_selectable());

        let node_sort = if has_nodes {
            self.node_sort
        } else {
            NodeSort::Order
        };
        let sort_active = node_sort != NodeSort::Order;
        let show_toolbar = has_nodes || model.groups.len() > 1;
        let node_view = if show_toolbar {
            self.node_view
        } else {
            NodeView::Flat
        };

        let search_query = if show_toolbar {
            self.node_search.read(cx).value().trim().to_lowercase()
        } else {
            String::new()
        };
        let mut display_nodes: Vec<usize> = model
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| !node.is_mesh_gateway() || !discovered_gateways.contains(&node.id))
            .filter(|(_, n)| node_matches_query(n, model, &search_query))
            .map(|(index, _)| index)
            .collect();

        // Sorting only changes the view; the stored order is left alone.
        sort_node_indices(&mut display_nodes, node_sort, model, &self.snapshot);

        // Fastest node among all enabled nodes, only if it clearly wins.
        let fastest_node_id = clear_winner(
            model
                .nodes
                .iter()
                .filter(|n| n.enabled)
                .filter_map(|n| {
                    let ms = self
                        .snapshot
                        .latency
                        .get(&n.id)
                        .filter(|entry| entry.is_http())
                        .and_then(|l| l.delay_ms)?;
                    (ms > 0).then(|| (n.id.clone(), ms))
                })
                .collect(),
        );

        let batch_mode = self.batch_mode;
        let batch_selected = &self.batch_selected;
        let latency_busy = this_busy(&self.snapshot.busy, "latency");
        let download_busy = this_busy(&self.snapshot.busy, "download");
        let reachability_busy = this_busy(&self.snapshot.busy, "reachability");
        let tests_busy = latency_busy
            || download_busy
            || reachability_busy
            || this_busy(&self.snapshot.busy, "auto-probe");
        let exit_busy = this_busy(&self.snapshot.busy, "mesh-exit");
        let core_busy = is_core_busy || exit_busy;
        let node_count = model
            .nodes
            .iter()
            .filter(|node| !node.is_mesh_gateway() || !discovered_gateways.contains(&node.id))
            .count()
            + exits.len();
        let row_gap = rems(if node_view == NodeView::Flat {
            0.75
        } else {
            0.5
        })
        .to_pixels(window.rem_size());
        let exits: Vec<_> = exits
            .into_iter()
            .filter(|exit| exit.matches(&search_query, hide_ips))
            .collect();
        let shown_exits = exits.len();
        let shown = display_nodes.len() + shown_exits;
        let row_height = px(super::theme::NODE_ROW_HEIGHT + 2.);
        let expanded_key = self.expanded_node.clone();
        let mut exit_rows = (shown_exits != 0).then(|| {
            let busy = core_busy || this_busy(&self.snapshot.busy, "mesh") || batch_mode;
            let expanded_idx = expanded_key
                .as_deref()
                .and_then(|key| exits.iter().position(|exit| exit.row_key() == key));
            let expanded_elem = expanded_idx.map(|idx| {
                exits[idx]
                    .row(busy, hide_ips, lang, &p, self, window, cx)
                    .into_any_element()
            });
            let exits = std::rc::Rc::new(exits);
            let render_exits = {
                let exits = exits.clone();
                let view = cx.entity();
                move |range: std::ops::Range<usize>, window: &mut Window, cx: &mut App| {
                    view.update(cx, |this, cx| {
                        exits[range]
                            .iter()
                            .map(|exit| {
                                exit.row(busy, hide_ips, lang, &p, this, window, cx)
                                    .into_any_element()
                            })
                            .collect()
                    })
                }
            };
            split_node_rows(
                shown_exits,
                row_height,
                row_gap,
                expanded_idx,
                expanded_elem,
                render_exits,
            )
        });

        let offline_exits: std::collections::HashSet<_> = self
            .snapshot
            .mesh
            .exits()
            .filter(|(_, peer, _)| peer.exit_selected && !peer.online)
            .map(|(profile, _, _)| format!("mesh-exit-{}", profile.id))
            .collect();
        let offline_exits_for_groups = offline_exits.clone();
        let fastest_for_groups = fastest_node_id.clone();
        let row_group = active_group.clone();
        let render_one_flat =
            |this: &AppView, index: usize, window: &mut Window, cx: &mut Context<AppView>| {
                let node = &this.snapshot.model.nodes[index];
                let usable = this
                    .snapshot
                    .model
                    .node_usable_with(node, this.snapshot.settings.core);
                let selected = active_node.as_deref() == Some(node.id.as_str())
                    && (!node.is_mesh_gateway() || this.snapshot.model.work_mode == WorkMode::Mesh);
                let group_id = row_group
                    .as_ref()
                    .filter(|_| selectable_members.contains(&node.id) && usable)
                    .cloned();
                this.render_configured_node_row(
                    ConfiguredRowContext {
                        index,
                        row_key: node.id.clone(),
                        selected,
                        fastest: fastest_node_id.as_deref() == Some(node.id.as_str()),
                        auto_mode,
                        group_context: false,
                        offline_exit: offline_exits.contains(&node.id),
                        group_id,
                        menu_group_id: None,
                        core_busy,
                        tests_busy,
                        drag_enabled: !sort_active
                            && node_view == NodeView::Flat
                            && !node.is_mesh_gateway(),
                    },
                    &p,
                    window,
                    cx,
                )
            };
        let view = cx.entity();
        let active_node_rc = active_node.clone();
        let fastest_rc = fastest_node_id.clone();
        let offline_rc = offline_exits.clone();
        let row_group_rc = row_group.clone();
        let selectable_rc = std::rc::Rc::new(selectable_members.clone());
        let render_rows = std::rc::Rc::new(
            move |indices: &[usize], window: &mut Window, cx: &mut App| {
                let selectable = selectable_rc.clone();
                view.update(cx, |this, cx| {
                    indices
                        .iter()
                        .map(|&index| {
                            let node = &this.snapshot.model.nodes[index];
                            let usable = this
                                .snapshot
                                .model
                                .node_usable_with(node, this.snapshot.settings.core);
                            let selected = active_node_rc.as_deref() == Some(node.id.as_str())
                                && (!node.is_mesh_gateway()
                                    || this.snapshot.model.work_mode == WorkMode::Mesh);
                            let group_id = row_group_rc
                                .as_ref()
                                .filter(|_| selectable.contains(&node.id) && usable)
                                .cloned();
                            this.render_configured_node_row(
                                ConfiguredRowContext {
                                    index,
                                    row_key: node.id.clone(),
                                    selected,
                                    fastest: fastest_rc.as_deref() == Some(node.id.as_str()),
                                    auto_mode,
                                    group_context: false,
                                    offline_exit: offline_rc.contains(&node.id),
                                    group_id,
                                    menu_group_id: None,
                                    core_busy,
                                    tests_busy,
                                    drag_enabled: !sort_active
                                        && node_view == NodeView::Flat
                                        && !node.is_mesh_gateway(),
                                },
                                &p,
                                window,
                                cx,
                            )
                        })
                        .collect::<Vec<_>>()
                })
            },
        );

        let configured_rows = |indices: Vec<usize>, window: &mut Window, cx: &mut Context<Self>| {
            let expanded_idx = expanded_key.as_deref().and_then(|key| {
                indices
                    .iter()
                    .position(|&index| self.snapshot.model.nodes[index].id == key)
            });
            let expanded_elem =
                expanded_idx.map(|pos| render_one_flat(self, indices[pos], window, cx));
            let render_rows = render_rows.clone();
            let indices = std::rc::Rc::new(indices);
            let count = indices.len();
            split_node_rows(
                count,
                row_height,
                row_gap,
                expanded_idx,
                expanded_elem,
                move |range, window, cx| render_rows(&indices[range], window, cx),
            )
        };
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
                                        format!("{shown}{}", self.tr(" 个", ""))
                                    } else {
                                        format!("{shown}/{node_count}{}", self.tr(" 个", ""))
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
                                                "Xray 模式：切换节点需重载内核",
                                                "Xray: switching nodes reloads the core",
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
                                .when(show_toolbar, |d| {
                                    d.children(self.nodes_policy_popover(core_busy, cx))
                                })
                                .child(
                                    more_button(
                                        "toolbar-more",
                                        self.tr("更多操作", "More actions"),
                                        cx,
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
                .when(show_toolbar, |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child({
                                let view_choices = [
                                    NodeView::Flat,
                                    NodeView::Source,
                                    NodeView::OutboundGroups,
                                ];
                                ChoiceGroup::new(
                                    "node-view",
                                    vec![
                                        self.tr("平铺", "Flat"),
                                        self.tr("来源", "Source"),
                                        self.tr("出站组", "Groups"),
                                    ],
                                    view_choices
                                        .iter()
                                        .position(|&v| v == node_view)
                                        .unwrap_or(0),
                                )
                                .on_change(cx.listener(
                                    move |this, index, _, cx| {
                                        this.node_view = view_choices
                                            .get(*index)
                                            .copied()
                                            .unwrap_or_default();
                                        cx.notify();
                                    },
                                ))
                            })
                            .child({
                                let sort_choices = [
                                    super::NodeSort::Order,
                                    super::NodeSort::Health,
                                    super::NodeSort::Latency,
                                ];
                                ChoiceGroup::new(
                                    "node-sort",
                                    vec![
                                        self.tr("顺序", "Order"),
                                        self.tr("健康 ↓", "Health ↓"),
                                        self.tr("延迟 ↑", "Latency ↑"),
                                    ],
                                    sort_choices
                                        .iter()
                                        .position(|&s| s == node_sort)
                                        .unwrap_or(0),
                                )
                                .on_change(cx.listener(
                                    move |this, index, _, cx| {
                                        this.node_sort = sort_choices
                                            .get(*index)
                                            .copied()
                                            .unwrap_or_default();
                                        cx.notify();
                                    },
                                ))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(super::theme::FORM_FIELD_WIDTH))
                                    .child(Input::new(&self.node_search)),
                            )
                            .child(
                                toolbar_icon_button(
                                    "health-check-all",
                                    IconName::HeartPulse,
                                    self.tr(
                                        "对本机直连、已启用代理和已连接组网出口进行批量健康检查",
                                        "Batch health check Direct, enabled proxies and connected mesh exits",
                                    ),
                                    cx,
                                )
                                .disabled(tests_busy)
                                .loading(reachability_busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.service.test_all_health(false);
                                    this.sync(cx);
                                })),
                            )
                            .child(
                                toolbar_icon_button(
                                    "test-all",
                                    IconName::Gauge,
                                    self.tr("对本机直连、已启用代理和已连接组网出口进行 HTTP 测速", "HTTP test Direct, enabled proxies and connected mesh exits"),
                                    cx,
                                )
                                .disabled(tests_busy)
                                .loading(latency_busy)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.latency_pending = this
                                            .snapshot
                                            .model
                                            .nodes
                                            .iter()
                                            .filter(|n| n.enabled)
                                            .map(|n| n.id.clone())
                                            .collect();
                                        this.service.test_all_http_latency();
                                        this.sync(cx);
                                    },
                                )),
                            )
                            .child(
                                toolbar_icon_button(
                                    "locate-all",
                                    IconName::MapPin,
                                    self.tr("全部探测归属地", "Locate all nodes"),
                                    cx,
                                )
                                .tooltip(self.tr(
                                    "探测全部启用节点的服务器归属地",
                                    "Locate servers for all enabled nodes",
                                ))
                                .disabled(
                                    this_busy(&self.snapshot.busy, "node-locations")
                                        || !model.nodes.iter().any(|node| node.enabled),
                                )
                                .loading(this_busy(&self.snapshot.busy, "node-locations"))
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.service.request_all_node_locations();
                                        this.sync(cx);
                                    },
                                )),
                            )
                            .child({
                                let chart_label = if self.hide_traffic_chart {
                                    self.tr("显示流量折线图", "Show traffic chart")
                                } else {
                                    self.tr("隐藏流量折线图", "Hide traffic chart")
                                };
                                icon_button_toggled(
                                    toolbar_icon_button(
                                        "toggle-traffic-chart",
                                        IconName::ChartLine,
                                        chart_label,
                                        cx,
                                    ),
                                    !self.hide_traffic_chart,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.hide_traffic_chart = !this.hide_traffic_chart;
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                })
                // Batch Operation Bar (Visible when batch_mode is active).
                .when(batch_mode, |d| {
                    let all_ids: Vec<String> = display_nodes
                        .iter()
                        .map(|&i| model.nodes[i].id.clone())
                        .collect();
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
                                        let button = Button::new("batch-test")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .icon(IconName::Gauge)
                                            .loading(latency_busy || download_busy || reachability_busy)
                                            .label(self.tr("批量测速", "Batch test"));
                                        if selected_count == 0 || tests_busy {
                                            button.disabled(true).into_any_element()
                                        } else {
                                            button
                                                .dropdown_menu(menus::builder(
                                                    cx.entity().downgrade(),
                                                    MenuTarget::BatchTests(selected_ids.clone()),
                                                ))
                                                .into_any_element()
                                        }
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-enable")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .disabled(selected_count == 0)
                                            .label(self.tr("批量启用", "Enable selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_nodes_enabled(
                                                    sel.iter().map(String::as_str),
                                                    true,
                                                );
                                                this.sync(cx);
                                            }))
                                    })
                                    .child({
                                        let running =
                                            self.snapshot.runtime.running_proxy_node.as_ref();
                                        let sel: Vec<_> = selected_ids
                                            .iter()
                                            .filter(|id| Some(*id) != running)
                                            .cloned()
                                            .collect();
                                        Button::new("batch-disable")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .disabled(sel.is_empty())
                                            .label(self.tr("批量停用", "Disable selected"))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_nodes_enabled(
                                                    sel.iter().map(String::as_str),
                                                    false,
                                                );
                                                this.sync(cx);
                                            }))
                                    })
                                    .child({
                                        let sel = selected_ids.clone();
                                        Button::new("batch-delete")
                                            .compact()
                                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                    core_busy,
                    window,
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
                .when(
                    node_view != NodeView::OutboundGroups
                        && shown == 0
                        && !search_query.is_empty(),
                    |d| {
                        d.child(empty_hint(
                            &p,
                            self.tr("未找到匹配的节点", "No matching nodes found"),
                        ))
                    },
                )
                // Node list: Flat, Grouped by Source, or Grouped by Outbound Group.
                .when(node_view == NodeView::Flat, |container| {
                    let (mesh_nodes, proxy_nodes) =
                        split_flat_nodes(std::mem::take(&mut display_nodes), model);
                    container
                        .children(exit_rows.take())
                        .when(!mesh_nodes.is_empty(), |d| {
                            d.child(configured_rows(mesh_nodes, window, cx))
                        })
                        .when(!proxy_nodes.is_empty(), |d| {
                            d.child(configured_rows(proxy_nodes, window, cx))
                        })
                })
                .when(node_view == NodeView::Source, |container| {
                    let sub_names: HashMap<_, _> = self
                        .snapshot
                        .subscriptions
                        .iter()
                        .map(|s| (s.id.as_str(), s.name.as_str()))
                        .collect();
                    let mut sub_groups: BTreeMap<NodeSection, Vec<usize>> = BTreeMap::new();
                    for &index in &display_nodes {
                        let node = &model.nodes[index];
                        let section = if node.is_mesh_gateway() {
                            NodeSection::MeshExits
                        } else if node.is_chain() {
                            NodeSection::Chains
                        } else if let Some(id) = &node.subscription_id {
                            NodeSection::Subscription(id)
                        } else {
                            NodeSection::Manual
                        };
                        sub_groups.entry(section).or_default().push(index);
                    }
                    if shown_exits != 0 {
                        sub_groups.entry(NodeSection::MeshExits).or_default();
                    }

                    let collapsed_set = &self.collapsed_subs;

                    container.children(sub_groups.into_iter().map(|(section, group_nodes)| {
                        let mesh = section == NodeSection::MeshExits;
                        let group_key = match section {
                            NodeSection::MeshExits => "mesh-exits".into(),
                            NodeSection::Chains => "chains".into(),
                            NodeSection::Manual => "manual".into(),
                            NodeSection::Subscription(id) => format!("subscription-{id}"),
                        };
                        let is_collapsed = collapsed_set.contains(&group_key);
                        let sub_title = match section {
                            NodeSection::MeshExits => {
                                self.tr("组网出口节点", "Mesh exit nodes").to_string()
                            }
                            NodeSection::Chains => self.tr("代理链", "Proxy chains").to_string(),
                            NodeSection::Subscription(id) => sub_names
                                .get(id)
                                .map(|&name| name.to_owned())
                                .unwrap_or_else(|| {
                                    self.tr("独立节点", "Standalone nodes").to_string()
                                }),
                            NodeSection::Manual => self
                                .tr("手动导入 / 未分类", "Manual / Uncategorized")
                                .to_string(),
                        };
                        let node_ids_for_ping: Vec<String> = group_nodes
                            .iter()
                            .map(|&i| model.nodes[i].id.clone())
                            .collect();
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
                                    .hover({
                                        let hover_bg = p.row_bg();
                                        move |s| s.bg(hover_bg)
                                    })
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
                                                format!(
                                                    "{}{}",
                                                    group_nodes.len()
                                                        + if mesh { shown_exits } else { 0 },
                                                    self.tr(" 个", "")
                                                ),
                                                p.muted_fg,
                                                p.panel_alt,
                                            )),
                                    )
                                    .when(!group_nodes.is_empty(), |header| {
                                        let group_testing = node_ids_for_ping.iter().any(|id| {
                                            this_busy(&self.snapshot.busy, &format!("latency:{id}"))
                                                || this_busy(&self.snapshot.busy, &format!("download:{id}"))
                                        });
                                        header.child(
                                            Button::new(SharedString::from(format!(
                                                "ping-sub-{}",
                                                group_key
                                            )))
                                            .compact()
                                            .ghost()
                                            .icon(IconName::Gauge)
                                            .loading(group_testing)
                                            .label(self.tr("测速本组", "Test group"))
                                            .disabled(tests_busy)
                                            .tooltip(self.tr(
                                                "测试本组已配置节点的延迟",
                                                "Test latency of configured nodes in this group",
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.latency_pending
                                                    .extend(node_ids_for_ping.iter().cloned());
                                                this.service
                                                    .test_http_latency(node_ids_for_ping.clone());
                                                this.sync(cx);
                                            })),
                                        )
                                    }),
                            )
                            .when(!is_collapsed, |d| {
                                d.when(!group_nodes.is_empty(), |d| {
                                    d.child(configured_rows(group_nodes, window, cx))
                                })
                                .when(mesh, |d| d.children(exit_rows.take()))
                            })
                    }))
                })
                .when(node_view == NodeView::OutboundGroups, |container| {
                    let node_index_by_id: HashMap<&str, usize> = model
                        .nodes
                        .iter()
                        .enumerate()
                        .map(|(idx, node)| (node.id.as_str(), idx))
                        .collect();
                    let ordered = ordered_outbound_groups(&model.groups, active_group.as_deref());
                    let collapsed_set = &self.collapsed_subs;
                    let groups_view = cx.entity();
                    let mut group_sections = Vec::new();
                    for group in ordered {
                        let is_default = active_group.as_deref() == Some(group.id.as_str());
                        let group_label = self.outbound_group_label(group);
                        let name_matches = !search_query.is_empty()
                            && group_label.to_lowercase().contains(&search_query);
                        let mut member_indices: Vec<usize> = group
                            .members
                            .iter()
                            .filter_map(|id| node_index_by_id.get(id.as_str()).copied())
                            .filter(|&idx| {
                                name_matches
                                    || node_matches_query(&model.nodes[idx], model, &search_query)
                            })
                            .collect();
                        sort_node_indices(&mut member_indices, node_sort, model, &self.snapshot);
                        let member_count = member_indices.len();
                        if !search_query.is_empty() && member_indices.is_empty() && !name_matches {
                            continue;
                        }
                        let group_key = format!("outbound-group-{}", group.id);
                        let is_collapsed = collapsed_set.contains(&group_key);
                        let group_id_str = group.id.clone();
                        let group_auto = !group.manually_selectable();
                        let group_enabled = group.enabled;
                        let group_active_node = (model.work_mode == WorkMode::Proxy)
                            .then(|| {
                                self.snapshot
                                    .selected_node(Some(&group.id))
                                    .map(|n| n.id.clone())
                            })
                            .flatten();
                        let can_be_default = !is_default
                            && group.enabled
                            && group.members.iter().any(|id| {
                                model
                                    .node(id)
                                    .is_some_and(|n| model.node_usable_with(n, self.snapshot.settings.core))
                            });
                        let member_ids_for_ping: Vec<String> = member_indices
                            .iter()
                            .map(|&i| model.nodes[i].id.clone())
                            .collect();
                        let group_testing = member_ids_for_ping.iter().any(|id| {
                            this_busy(&self.snapshot.busy, &format!("latency:{id}"))
                                || this_busy(&self.snapshot.busy, &format!("download:{id}"))
                        });
                        let policy_label = self.outbound_policy_label(group);
                        let key_for_click = group_key.clone();
                        let edit_group_id = group.id.clone();
                        let default_group_id = group.id.clone();
                        let policy_group_id = group.id.clone();
                        let menu_group_id = group.id.clone();

                        let rows_elem = (!is_collapsed).then(|| {
                            if member_indices.is_empty() {
                                empty_hint(
                                    &p,
                                    self.tr(
                                        "此出站组暂无成员节点，点击「编辑」添加成员",
                                        "No member nodes in this group; click Edit to add members",
                                    ),
                                )
                                .into_any_element()
                            } else {
                                let groups_view = groups_view.clone();
                                let offline_exits = offline_exits_for_groups.clone();
                                let fastest_id = fastest_for_groups.clone();
                                let group_id_for_rows = group_id_str.clone();
                                let expanded_idx = expanded_key.as_deref().and_then(|key| {
                                    member_indices.iter().position(|&index| {
                                        let node = &self.snapshot.model.nodes[index];
                                        format!("grp-{}-{}", group_id_for_rows, node.id) == key
                                    })
                                });
                                let expanded_elem = expanded_idx.map(|pos| {
                                    let index = member_indices[pos];
                                    let node = &self.snapshot.model.nodes[index];
                                    let usable = self
                                        .snapshot
                                        .model
                                        .node_usable_with(node, self.snapshot.settings.core);
                                    self.render_configured_node_row(
                                        ConfiguredRowContext {
                                            index,
                                            row_key: format!(
                                                "grp-{}-{}",
                                                group_id_for_rows, node.id
                                            ),
                                            selected: group_active_node.as_deref()
                                                == Some(node.id.as_str()),
                                            fastest: fastest_id.as_deref()
                                                == Some(node.id.as_str()),
                                            auto_mode: group_auto,
                                            group_context: true,
                                            offline_exit: offline_exits.contains(&node.id),
                                            group_id: (group_enabled && !group_auto && usable)
                                                .then(|| group_id_for_rows.clone()),
                                            menu_group_id: Some(group_id_for_rows.clone()),
                                            core_busy,
                                            tests_busy,
                                            drag_enabled: false,
                                        },
                                        &p,
                                        window,
                                        cx,
                                    )
                                });
                                let member_indices = std::rc::Rc::new(member_indices);
                                split_node_rows(
                                    member_count,
                                    row_height,
                                    row_gap,
                                    expanded_idx,
                                    expanded_elem,
                                    move |range, window, cx| {
                                        groups_view.update(cx, |this, cx| {
                                            member_indices[range]
                                                .iter()
                                                .map(|&index| {
                                                    let node = &this.snapshot.model.nodes[index];
                                                    let usable = this
                                                        .snapshot
                                                        .model
                                                        .node_usable_with(
                                                            node,
                                                            this.snapshot.settings.core,
                                                        );
                                                    this.render_configured_node_row(
                                                        ConfiguredRowContext {
                                                            index,
                                                            row_key: format!(
                                                                "grp-{}-{}",
                                                                group_id_for_rows, node.id
                                                            ),
                                                            selected: group_active_node.as_deref()
                                                                == Some(node.id.as_str()),
                                                            fastest: fastest_id.as_deref()
                                                                == Some(node.id.as_str()),
                                                            auto_mode: group_auto,
                                                            group_context: true,
                                                            offline_exit: offline_exits
                                                                .contains(&node.id),
                                                            group_id: (group_enabled
                                                                && !group_auto
                                                                && usable)
                                                                .then(|| group_id_for_rows.clone()),
                                                            menu_group_id: Some(
                                                                group_id_for_rows.clone(),
                                                            ),
                                                            core_busy,
                                                            tests_busy,
                                                            drag_enabled: false,
                                                        },
                                                        &p,
                                                        window,
                                                        cx,
                                                    )
                                                })
                                                .collect::<Vec<_>>()
                                        })
                                    },
                                )
                            }
                        });

                        let policy_btn = {
                            let mut btn = Button::new(SharedString::from(format!(
                                "group-policy-{}",
                                group.id
                            )))
                            .compact()
                            .h(px(26.))
                            .px_2()
                            .disabled(core_busy)
                            .tooltip(self.tr(
                                "切换此出站组的选点策略",
                                "Change selection policy for this outbound group",
                            ));
                            if group_auto {
                                btn = btn.custom(
                                    ButtonCustomVariant::new(cx)
                                        .color(p.accent)
                                        .foreground(p.primary)
                                        .hover(p.primary.opacity(SEMANTIC_ALPHA + 0.06))
                                        .active(p.primary.opacity(SEMANTIC_ALPHA + 0.12)),
                                );
                            } else {
                                btn = btn.ghost();
                            }
                            btn.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if group_auto {
                                                p.primary
                                            } else {
                                                p.muted_fg
                                            })
                                            .child(policy_label),
                                    )
                                    .child(
                                        Icon::new(IconName::ChevronDown)
                                            .size(px(12.))
                                            .text_color(if group_auto {
                                                p.primary
                                            } else {
                                                p.muted_fg
                                            })
                                            .opacity(0.8),
                                    ),
                            )
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                MenuTarget::GroupSwitchPolicy(policy_group_id),
                            ))
                        };

                        group_sections.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "group-header-{}",
                                            group.id
                                        )))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .flex_wrap()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .hover({
                                            let hover_bg = p.row_bg();
                                            move |s| s.bg(hover_bg)
                                        })
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
                                                .flex_wrap()
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
                                                        .child(group_label),
                                                )
                                                .when(is_default, |d| {
                                                    d.child(pill(
                                                        self.tr("默认", "Default"),
                                                        p.primary,
                                                        p.accent,
                                                    ))
                                                })
                                                .when(!group.enabled, |d| {
                                                    d.child(pill(
                                                        self.tr("已禁用", "Disabled"),
                                                        p.muted_fg,
                                                        p.panel_alt,
                                                    ))
                                                })
                                                .child(pill(
                                                    format!(
                                                        "{}{}",
                                                        member_count,
                                                        self.tr(" 个", "")
                                                    ),
                                                    p.muted_fg,
                                                    p.panel_alt,
                                                )),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "group-actions-{}",
                                                    group.id
                                                )))
                                                .flex()
                                                .items_center()
                                                .flex_wrap()
                                                .gap_1()
                                                .on_click(|_, _, cx| cx.stop_propagation())
                                                .when(can_be_default, |d| {
                                                    d.child(
                                                        Button::new(SharedString::from(format!(
                                                            "group-set-default-{}",
                                                            group.id
                                                        )))
                                                        .compact()
                                                        .ghost()
                                                        .label(self.tr("设为默认", "Set default"))
                                                        .disabled(core_busy)
                                                        .on_click(cx.listener(
                                                            move |this, _, window, cx| {
                                                                this.run_command(
                                                                    Command::SetDefaultOutbound(
                                                                        default_group_id.clone(),
                                                                    ),
                                                                    false,
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                    )
                                                })
                                                .child(policy_btn)
                                                .when(!member_ids_for_ping.is_empty(), |d| {
                                                    d.child(
                                                        Button::new(SharedString::from(format!(
                                                            "ping-group-{}",
                                                            group.id
                                                        )))
                                                        .compact()
                                                        .ghost()
                                                        .icon(IconName::Gauge)
                                                        .loading(group_testing)
                                                        .label(self.tr("测速本组", "Test group"))
                                                        .disabled(tests_busy)
                                                        .tooltip(self.tr(
                                                            "测试本组已配置节点的延迟",
                                                            "Test latency of configured nodes in this group",
                                                        ))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.latency_pending.extend(
                                                                    member_ids_for_ping
                                                                        .iter()
                                                                        .cloned(),
                                                                );
                                                                this.service.test_http_latency(
                                                                    member_ids_for_ping.clone(),
                                                                );
                                                                this.sync(cx);
                                                            },
                                                        )),
                                                    )
                                                })
                                                .child(
                                                    Button::new(SharedString::from(format!(
                                                        "group-edit-{}",
                                                        group.id
                                                    )))
                                                    .compact()
                                                    .ghost()
                                                    .label(self.tr("编辑", "Edit"))
                                                    .disabled(core_busy)
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.open_outbound_group_editor(
                                                                Some(edit_group_id.clone()),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                                )
                                                .child(
                                                    more_button(
                                                        SharedString::from(format!(
                                                            "group-more-{}",
                                                            group.id
                                                        )),
                                                        self.tr("出站组操作", "Group actions"),
                                                        cx,
                                                    )
                                                    .dropdown_menu(menus::builder(
                                                        cx.entity().downgrade(),
                                                        MenuTarget::OutboundGroup(menu_group_id),
                                                    )),
                                                ),
                                        ),
                                )
                                .children(rows_elem),
                        );
                    }

                    container
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .flex_wrap()
                                .gap_2()
                                .px_1()
                                .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                                    "按出站组查看、选点并独立配置自动切换策略",
                                    "View groups, choose exits, and configure per-group auto-switch policies",
                                )))
                                .child(
                                    Button::new("nodes-create-outbound-group")
                                        .compact()
                                        .ghost()
                                        .h(px(28.))
                                        .icon(IconName::Plus)
                                        .label(self.tr("新建出站组", "New group"))
                                        .disabled(core_busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_outbound_group_editor(None, window, cx);
                                        })),
                                ),
                        )
                        .when(group_sections.is_empty() && !search_query.is_empty(), |d| {
                            d.child(empty_hint(
                                &p,
                                self.tr(
                                    "未找到匹配的节点或出站组",
                                    "No matching nodes or outbound groups found",
                                ),
                            ))
                        })
                        .children(group_sections)
                }),
        );

        body
    }
}

struct ConfiguredRowContext {
    index: usize,
    row_key: String,
    selected: bool,
    fastest: bool,
    auto_mode: bool,
    group_context: bool,
    offline_exit: bool,
    group_id: Option<String>,
    menu_group_id: Option<String>,
    core_busy: bool,
    tests_busy: bool,
    drag_enabled: bool,
}

impl AppView {
    pub(super) fn toggle_expanded_node(&mut self, row_key: String, cx: &mut Context<Self>) {
        self.node_click_seq = self.node_click_seq.wrapping_add(1);
        self.pre_click_expanded = None;
        toggle_accordion(&mut self.expanded_node, row_key);
        cx.notify();
    }

    pub(super) fn schedule_row_expand(
        &mut self,
        row_key: String,
        immediate: bool,
        cx: &mut Context<Self>,
    ) {
        self.node_click_seq = self.node_click_seq.wrapping_add(1);
        self.pre_click_expanded = None;
        if immediate {
            toggle_accordion(&mut self.expanded_node, row_key);
            cx.notify();
            return;
        }
        let seq = self.node_click_seq;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let _ = view.update(cx, |this, cx| {
                if this.node_click_seq != seq {
                    return;
                }
                this.pre_click_expanded = Some((row_key.clone(), this.expanded_node.clone()));
                toggle_accordion(&mut this.expanded_node, row_key);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn cancel_row_expand_for_double_click(
        &mut self,
        row_key: &str,
        cx: &mut Context<Self>,
    ) {
        self.node_click_seq = self.node_click_seq.wrapping_add(1);
        if restore_pre_double_click_expand(
            &mut self.expanded_node,
            &mut self.pre_click_expanded,
            row_key,
        ) {
            cx.notify();
        }
    }

    pub(in crate::ui) fn toggle_node_details_for_target(
        &mut self,
        target: &HttpTestTarget,
        row_key: &str,
        cx: &mut Context<Self>,
    ) {
        let key = if !row_key.is_empty() {
            row_key.to_string()
        } else {
            target.key()
        };
        self.node_click_seq = self.node_click_seq.wrapping_add(1);
        self.pre_click_expanded = None;
        toggle_accordion(&mut self.expanded_node, key);
        self.page = Page::Nodes;
        cx.notify();
    }

    fn node_source_and_groups(
        &self,
        node: &Node,
        virtual_exit: Option<&VirtualExit>,
    ) -> (SharedString, SharedString) {
        let lang = self.lang();
        let source = if matches!(virtual_exit, Some(VirtualExit::Direct { .. })) {
            super::tr(lang, "本机直连", "Direct")
        } else if matches!(virtual_exit, Some(VirtualExit::Mesh { .. })) || node.is_mesh_gateway() {
            super::tr(lang, "组网出口", "Mesh exit")
        } else if node.is_chain() {
            super::tr(lang, "代理链", "Proxy chain")
        } else if let Some(sub_id) = node.subscription_id.as_deref() {
            self.snapshot
                .subscriptions
                .iter()
                .find(|s| s.id == sub_id)
                .map(|s| SharedString::from(s.name.clone()))
                .unwrap_or_else(|| super::tr(lang, "节点订阅", "Subscription"))
        } else {
            match node.source {
                xrose_core::model::NodeSource::Import => super::tr(lang, "导入节点", "Imported"),
                xrose_core::model::NodeSource::Subscription => {
                    super::tr(lang, "节点订阅", "Subscription")
                }
                xrose_core::model::NodeSource::Manual => super::tr(lang, "手动节点", "Manual"),
            }
        };

        let group_names: Vec<String> = self
            .snapshot
            .model
            .groups
            .iter()
            .filter(|g| g.members.iter().any(|id| id == &node.id))
            .map(|g| self.outbound_group_label(g).to_string())
            .collect();
        let groups: SharedString = if group_names.is_empty() {
            super::tr(lang, "未加入出站组", "No outbound group")
        } else {
            group_names.join("、").into()
        };
        (source, groups)
    }

    fn direct_takeover_summary(&self) -> SharedString {
        let lang = self.lang();
        let settings = &self.snapshot.settings;
        let mut parts = Vec::new();
        if settings.effective_tun() {
            parts.push(super::tr(lang, "TUN 全局", "TUN").to_string());
        }
        if settings.system_proxy {
            parts.push(super::tr(lang, "系统代理", "System proxy").to_string());
        }
        parts.push(format!("Mixed :{}", settings.mixed_port));
        parts.join(" · ").into()
    }

    fn render_configured_node_row(
        &self,
        ctx: ConfiguredRowContext,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lang = self.lang();
        let hide_ips = self.hide_ips;
        let suffixes = self.snapshot.mesh.magic_dns_suffixes();
        let node = &self.snapshot.model.nodes[ctx.index];
        let usable = self
            .snapshot
            .model
            .node_usable_with(node, self.snapshot.settings.core);
        let chain_subtitle =
            chain_row_subtitle(node, &self.snapshot.model, hide_ips, &suffixes, lang);
        let (source_summary, group_summary) = self.node_source_and_groups(node, None);
        let expanded = self.expanded_node.as_deref() == Some(ctx.row_key.as_str());
        render_node_row(
            RowProps {
                node,
                row_key: ctx.row_key,
                lang,
                selected: ctx.selected,
                expanded,
                virtual_exit: None,
                chain_subtitle,
                fastest: ctx.fastest,
                latency: self
                    .snapshot
                    .latency
                    .get(&node.id)
                    .filter(|entry| entry.is_http()),
                location: self.snapshot.node_location(node),
                download: self.snapshot.downloads.get(&node.id),
                downloading: this_busy(&self.snapshot.busy, &format!("download:{}", node.id)),
                health: self.snapshot.node_health.get(&node.id),
                checking_reachability: this_busy(
                    &self.snapshot.busy,
                    &format!("reachability:{}", node.id),
                ),
                hide_ips,
                magic_dns_suffixes: &suffixes,
                usable,
                auto_mode: ctx.auto_mode,
                group_context: ctx.group_context,
                offline_exit: ctx.offline_exit,
                mesh_provider: node
                    .id
                    .strip_prefix("mesh-exit-")
                    .and_then(|id| {
                        self.snapshot
                            .mesh
                            .profiles
                            .iter()
                            .find(|profile| profile.id == id)
                    })
                    .map(|profile| profile.provider),
                group_id: ctx.group_id,
                menu_group_id: ctx.menu_group_id,
                source_summary,
                group_summary,
                direct_takeover_summary: self.direct_takeover_summary(),
                probe_budget_used_bytes: self.snapshot.probe_budget_used_bytes,
                daily_probe_budget_mb: self.snapshot.settings.daily_probe_budget_mb,
                core_busy: ctx.core_busy,
                latency_busy: ctx.tests_busy,
                testing: this_busy(&self.snapshot.busy, &format!("latency:{}", node.id)),
                batch_mode: self.batch_mode,
                batch_checked: self.batch_selected.contains(&node.id),
                drag_enabled: ctx.drag_enabled,
            },
            p,
            window,
            cx,
        )
        .into_any_element()
    }
}

fn toggle_accordion(current: &mut Option<String>, key: String) {
    if current.as_deref() == Some(key.as_str()) {
        *current = None;
    } else {
        *current = Some(key);
    }
}

fn restore_pre_double_click_expand(
    expanded: &mut Option<String>,
    pre_click: &mut Option<(String, Option<String>)>,
    row_key: &str,
) -> bool {
    if let Some((key, previous)) = pre_click.take()
        && key == row_key
    {
        *expanded = previous;
        true
    } else {
        false
    }
}

/// Keep collapsed rows virtualized while letting the single expanded card lay
/// out in normal flex flow at its intrinsic height.
fn split_node_rows(
    count: usize,
    height: Pixels,
    gap: Pixels,
    expanded_index: Option<usize>,
    expanded_element: Option<AnyElement>,
    render_collapsed: impl Fn(std::ops::Range<usize>, &mut Window, &mut App) -> Vec<AnyElement>
    + Clone
    + 'static,
) -> AnyElement {
    let Some(idx) = expanded_index.filter(|&i| i < count) else {
        return node_rows(count, height, gap, render_collapsed).into_any_element();
    };
    let mut stack = div().flex().flex_col().gap(gap).w_full();
    if idx > 0 {
        let prefix_render = render_collapsed.clone();
        stack = stack.child(node_rows(idx, height, gap, move |range, window, cx| {
            prefix_render(range, window, cx)
        }));
    }
    if let Some(elem) = expanded_element {
        stack = stack.child(elem);
    }
    if idx + 1 < count {
        let suffix_count = count - idx - 1;
        let offset = idx + 1;
        stack = stack.child(node_rows(
            suffix_count,
            height,
            gap,
            move |range, window, cx| {
                render_collapsed((range.start + offset)..(range.end + offset), window, cx)
            },
        ));
    }
    stack.into_any_element()
}

/// The page owns the scroll offset. Lay out only the rows intersecting its clip,
/// using the same row and gap dimensions for hit testing and scroll extent.
fn node_rows(
    count: usize,
    height: Pixels,
    gap: Pixels,
    render: impl FnOnce(std::ops::Range<usize>, &mut Window, &mut App) -> Vec<AnyElement> + 'static,
) -> impl IntoElement {
    let stride = height + gap;
    canvas(
        move |bounds, window, cx| {
            let clip = window.content_mask().bounds;
            if !clip.intersects(&bounds) {
                return Vec::new();
            }
            let range = node_row_range(
                clip.top() - bounds.top(),
                clip.bottom() - bounds.top(),
                stride,
                count,
            );
            let rows = render(range.clone(), window, cx);
            range
                .zip(rows)
                .map(|(index, content)| {
                    let mut row = div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(content)
                        .into_any_element();
                    row.prepaint_as_root(
                        point(bounds.left(), bounds.top() + stride * index as f32),
                        size(
                            AvailableSpace::Definite(bounds.size.width),
                            AvailableSpace::Definite(height),
                        ),
                        window,
                        cx,
                    );
                    row
                })
                .collect::<Vec<_>>()
        },
        |_, mut rows, window, cx| {
            for row in &mut rows {
                row.paint(window, cx);
            }
        },
    )
    .w_full()
    .flex_shrink_0()
    .h(height * count as f32 + gap * count.saturating_sub(1) as f32)
}

// Keep one neighbour on either side for focus traversal at the viewport edge.
fn node_row_range(
    top: Pixels,
    bottom: Pixels,
    stride: Pixels,
    count: usize,
) -> std::ops::Range<usize> {
    let first = ((top / stride).floor() as usize)
        .saturating_sub(1)
        .min(count);
    let end = ((bottom / stride).ceil() as usize)
        .saturating_add(1)
        .min(count);
    first..end
}

fn this_busy(busy: &[String], key: &str) -> bool {
    busy.iter().any(|b| b == key)
}

fn node_matches_query(node: &Node, model: &AppModel, query: &str) -> bool {
    if query.is_empty()
        || node.name.to_lowercase().contains(query)
        || node.endpoint().to_lowercase().contains(query)
        || node.protocol.display_name().to_lowercase().contains(query)
    {
        return true;
    }
    if node.is_chain() {
        if "代理链".contains(query) {
            return true;
        }
        return model
            .chain_hop_summaries(node)
            .iter()
            .any(|hop| hop.label.to_lowercase().contains(query));
    }
    false
}

fn chain_row_subtitle(
    node: &Node,
    model: &AppModel,
    hide_ips: bool,
    suffixes: &[&str],
    lang: Language,
) -> Option<String> {
    if !node.is_chain() {
        return None;
    }
    let parts: Vec<String> = node
        .chain_hops
        .iter()
        .map(|hop| super::chain_editor::format_hop_label(hop, model, hide_ips, suffixes, lang).0)
        .collect();
    Some(parts.join(" ──▶ "))
}

fn sort_node_indices(
    indices: &mut [usize],
    sort: NodeSort,
    model: &AppModel,
    snapshot: &UiSnapshot,
) {
    match sort {
        NodeSort::Order => {}
        NodeSort::Latency => {
            indices.sort_by(|&a, &b| {
                let rtt = |idx: usize| {
                    let n = &model.nodes[idx];
                    snapshot
                        .latency
                        .get(&n.id)
                        .filter(|entry| entry.is_http())
                        .and_then(|l| l.delay_ms)
                };
                match (rtt(a), rtt(b)) {
                    (Some(va), Some(vb)) => va.cmp(&vb).then_with(|| a.cmp(&b)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.cmp(&b),
                }
            });
        }
        NodeSort::Health => {
            indices.sort_by(|&a, &b| {
                let score_pair = |idx: usize| -> Option<(f32, f32)> {
                    let n = &model.nodes[idx];
                    snapshot
                        .node_health
                        .get(&n.id)?
                        .general
                        .as_ref()
                        .map(|g| (g.score, g.short_score))
                };
                match (score_pair(a), score_pair(b)) {
                    (Some(sa), Some(sb)) => sb
                        .0
                        .partial_cmp(&sa.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| sb.1.partial_cmp(&sa.1).unwrap_or(std::cmp::Ordering::Equal))
                        .then_with(|| a.cmp(&b)),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.cmp(&b),
                }
            });
        }
    }
}

fn split_flat_nodes(indices: Vec<usize>, model: &AppModel) -> (Vec<usize>, Vec<usize>) {
    let mut mesh_nodes = Vec::new();
    let mut proxy_nodes = Vec::new();
    for idx in indices {
        if model.nodes[idx].is_mesh_gateway() {
            mesh_nodes.push(idx);
        } else {
            proxy_nodes.push(idx);
        }
    }
    (mesh_nodes, proxy_nodes)
}

fn ordered_outbound_groups<'a>(
    groups: &'a [OutboundGroup],
    default_id: Option<&str>,
) -> Vec<&'a OutboundGroup> {
    let mut ordered = Vec::with_capacity(groups.len());
    if let Some(default_id) = default_id
        && let Some(default_group) = groups.iter().find(|group| group.id == default_id)
    {
        ordered.push(default_group);
    }
    for group in groups {
        if default_id != Some(group.id.as_str()) {
            ordered.push(group);
        }
    }
    ordered
}

/// Helper function to format and optionally mask host addresses for privacy.
pub(super) fn format_endpoint(endpoint: &str, hide: bool) -> String {
    xrose_core::privacy::mask_proxy_endpoint(endpoint, hide)
}

pub(in crate::ui) fn display_node_name(node: &Node, hide_ips: bool, suffixes: &[&str]) -> String {
    if hide_ips {
        xrose_core::privacy::mask_server_in_label(&node.name, &node.server, "•••", suffixes)
    } else {
        node.name.clone()
    }
}

/// Everything a node row needs; computed once per frame by the page.
struct RowProps<'a> {
    node: &'a Node,
    row_key: String,
    lang: Language,
    selected: bool,
    expanded: bool,
    virtual_exit: Option<VirtualExit>,
    chain_subtitle: Option<String>,
    fastest: bool,
    latency: Option<&'a LatencyEntry>,
    location: Option<&'a xrose_core::runtime::location::NodeLocation>,
    download: Option<&'a xrose_core::runtime::probe::DownloadResult>,
    downloading: bool,
    health: Option<&'a xrose_core::runtime::health::NodeHealthView>,
    checking_reachability: bool,
    hide_ips: bool,
    magic_dns_suffixes: &'a [&'a str],
    usable: bool,
    auto_mode: bool,
    group_context: bool,
    offline_exit: bool,
    mesh_provider: Option<xrose_core::mesh::MeshProvider>,
    group_id: Option<String>,
    menu_group_id: Option<String>,
    source_summary: SharedString,
    group_summary: SharedString,
    direct_takeover_summary: SharedString,
    probe_budget_used_bytes: u64,
    daily_probe_budget_mb: u32,
    core_busy: bool,
    latency_busy: bool,
    testing: bool,
    batch_mode: bool,
    batch_checked: bool,
    drag_enabled: bool,
}

#[derive(Clone)]
enum VirtualExit {
    Direct {
        detail: String,
    },
    Mesh {
        profile: String,
        peer: String,
        detail: String,
        online: bool,
        provider: xrose_core::mesh::MeshProvider,
        resumable: bool,
    },
}

impl VirtualExit {
    fn target(&self) -> HttpTestTarget {
        match self {
            Self::Direct { .. } => HttpTestTarget::Direct,
            Self::Mesh { profile, peer, .. } => HttpTestTarget::MeshExit {
                profile: profile.clone(),
                peer: peer.clone(),
            },
        }
    }
    fn detail(&self) -> &str {
        match self {
            Self::Direct { detail } | Self::Mesh { detail, .. } => detail,
        }
    }
}

fn select_row_exit(
    this: &mut AppView,
    exit: Option<&VirtualExit>,
    group: Option<&str>,
    node: &str,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    match exit {
        Some(VirtualExit::Direct { .. }) => this.run_command(
            Command::SelectWorkMode(xrose_core::model::WorkMode::Direct),
            false,
            window,
            cx,
        ),
        Some(VirtualExit::Mesh { profile, peer, .. }) => {
            this.service
                .select_mesh_exit_mode(profile.clone(), peer.clone());
            this.sync(cx);
        }
        None => {
            this.service.select_node(group.unwrap_or(""), node);
            this.sync(cx);
        }
    }
}

fn render_virtual_node_row(
    node: &Node,
    exit: VirtualExit,
    selected: bool,
    busy: bool,
    view: &AppView,
    p: &Palette,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let key = exit.target().key();
    let offline = matches!(&exit, VirtualExit::Mesh { online: false, .. });
    let mesh_provider = match &exit {
        VirtualExit::Mesh { provider, .. } => Some(*provider),
        VirtualExit::Direct { .. } => None,
    };
    let magic_dns_suffixes = view.snapshot.mesh.magic_dns_suffixes();
    let (source_summary, group_summary) = view.node_source_and_groups(node, Some(&exit));
    let expanded = view.expanded_node.as_deref() == Some(key.as_str());
    render_node_row(
        RowProps {
            node,
            row_key: key.clone(),
            lang: view.lang(),
            selected,
            expanded,
            virtual_exit: Some(exit),
            chain_subtitle: None,
            fastest: false,
            latency: view
                .snapshot
                .latency
                .get(&key)
                .filter(|entry| entry.is_http()),
            location: None,
            download: view.snapshot.downloads.get(&key),
            downloading: this_busy(&view.snapshot.busy, &format!("download:{key}")),
            health: view.snapshot.node_health.get(&key),
            checking_reachability: this_busy(&view.snapshot.busy, &format!("reachability:{key}")),
            hide_ips: view.hide_ips,
            magic_dns_suffixes: &magic_dns_suffixes,
            usable: true,
            auto_mode: false,
            group_context: false,
            offline_exit: offline,
            mesh_provider,
            group_id: None,
            menu_group_id: None,
            source_summary,
            group_summary,
            direct_takeover_summary: view.direct_takeover_summary(),
            probe_budget_used_bytes: view.snapshot.probe_budget_used_bytes,
            daily_probe_budget_mb: view.snapshot.settings.daily_probe_budget_mb,
            core_busy: busy,
            latency_busy: [
                "latency",
                "download",
                "reachability",
                "auto-probe",
                "core",
                "mesh",
            ]
            .iter()
            .any(|key| this_busy(&view.snapshot.busy, key)),
            testing: this_busy(&view.snapshot.busy, &format!("latency:{key}")),
            batch_mode: false,
            batch_checked: false,
            drag_enabled: false,
        },
        p,
        window,
        cx,
    )
}

fn primary_badge_label(node: &Node, virtual_exit: Option<&VirtualExit>, lang: Language) -> String {
    if matches!(virtual_exit, Some(VirtualExit::Direct { .. })) {
        super::tr(lang, "直连", "Direct").to_string()
    } else if matches!(virtual_exit, Some(VirtualExit::Mesh { .. }))
        || node.id.starts_with("mesh-exit-")
    {
        super::tr(lang, "组网出口", "Mesh exit").to_string()
    } else if node.is_chain() {
        super::tr(lang, "代理链", "Chain").to_string()
    } else {
        node.protocol.display_name().to_uppercase()
    }
}

#[allow(clippy::too_many_arguments)]
fn row_tooltip(
    node: &Node,
    virtual_exit: Option<&VirtualExit>,
    selected: bool,
    usable: bool,
    auto_mode: bool,
    group_context: bool,
    in_selectable_group: bool,
    offline_exit: bool,
    batch_mode: bool,
    lang: Language,
) -> SharedString {
    if batch_mode {
        return super::tr(
            lang,
            "点击勾选或取消勾选此节点",
            "Click to select or deselect this node",
        );
    }
    if matches!(virtual_exit, Some(VirtualExit::Direct { .. })) {
        return if selected {
            super::tr(
                lang,
                "当前处于本机直连模式；单击展开详情，右键查看更多操作",
                "Direct mode active; click for details, right-click for more actions",
            )
        } else {
            super::tr(
                lang,
                "单击展开详情，双击切换为本机直连；右键查看更多操作",
                "Click for details, double-click to switch to Direct; right-click for more actions",
            )
        };
    }
    if virtual_exit.is_none() && node.is_mesh_gateway() && group_context {
        return if !node.enabled {
            super::tr(
                lang,
                "节点已禁用，可在右键或更多操作中启用",
                "Node is disabled; enable it from the context menu or More actions",
            )
        } else if offline_exit {
            super::tr(
                lang,
                "出口设备离线，请先连接所属组网",
                "Exit device is offline; connect its mesh network first",
            )
        } else if auto_mode {
            super::tr(
                lang,
                "此出站组为自动选择模式；单击展开详情，右键管理节点",
                "This outbound group selects nodes automatically; click for details, right-click to manage",
            )
        } else if !in_selectable_group {
            super::tr(
                lang,
                "当前出站组已禁用或节点不可用",
                "This outbound group is disabled or the node is unavailable",
            )
        } else if selected {
            super::tr(
                lang,
                "当前出站组已选用此组网分流网关；单击展开详情，右键查看更多操作",
                "Selected mesh gateway in this outbound group; click for details, right-click for more actions",
            )
        } else {
            super::tr(
                lang,
                "单击展开详情，双击设为该出站组的分流出口（保持代理规则生效）",
                "Click for details, double-click to use this mesh gateway for this outbound group",
            )
        };
    }
    if matches!(virtual_exit, Some(VirtualExit::Mesh { .. })) || node.is_mesh_gateway() {
        let resumable = matches!(
            virtual_exit,
            Some(VirtualExit::Mesh {
                resumable: true,
                ..
            })
        );
        return if !node.enabled {
            super::tr(
                lang,
                "节点已禁用，可在右键或更多操作中启用",
                "Node is disabled; enable it from the context menu or More actions",
            )
        } else if offline_exit && !resumable {
            super::tr(
                lang,
                "出口设备离线，请先连接所属组网",
                "Exit device is offline; connect its mesh network first",
            )
        } else if selected {
            super::tr(
                lang,
                "当前由该组网出口接管系统流量；单击展开详情，右键查看更多操作",
                "This mesh exit is handling system traffic; click for details, right-click for more actions",
            )
        } else if offline_exit {
            super::tr(
                lang,
                "单击展开详情，双击重连所属组网并由此出口接管系统流量",
                "Click for details, double-click to reconnect its mesh network and use this exit",
            )
        } else {
            super::tr(
                lang,
                "单击展开详情，双击进入组网模式并由此出口接管系统流量",
                "Click for details, double-click to use this exit in Mesh mode",
            )
        };
    }
    if !node.enabled {
        super::tr(
            lang,
            "节点已禁用，可在右键或更多操作中启用",
            "Node is disabled; enable it from the context menu or More actions",
        )
    } else if !usable {
        if node.is_chain() {
            super::tr(
                lang,
                "代理链存在已禁用、缺失或当前内核不支持的跳点，请编辑后使用",
                "One or more hops in this chain are disabled, missing, or unsupported by the current core",
            )
        } else {
            super::tr(
                lang,
                "当前内核不支持此协议，请切换内核后再使用",
                "Unsupported by current core; switch cores to use this node",
            )
        }
    } else if auto_mode {
        if group_context {
            super::tr(
                lang,
                "此出站组为自动选择模式；单击展开详情，右键管理节点",
                "This outbound group selects nodes automatically; click for details, right-click to manage",
            )
        } else {
            super::tr(
                lang,
                "默认出站组为自动选择模式；单击展开详情，右键管理节点",
                "Default outbound group selects nodes automatically; click for details, right-click to manage",
            )
        }
    } else if !in_selectable_group {
        if group_context {
            super::tr(
                lang,
                "当前出站组已禁用，无法手动选点",
                "This outbound group is disabled and cannot be selected manually",
            )
        } else {
            super::tr(
                lang,
                "未加入当前默认出站组，可在右键菜单配置所属出站组",
                "Not in the default outbound group; configure membership from the context menu",
            )
        }
    } else if selected {
        if group_context {
            super::tr(
                lang,
                "当前出站组选中的代理节点；单击展开详情，右键查看更多操作",
                "Selected proxy node in this outbound group; click for details, right-click for more actions",
            )
        } else {
            super::tr(
                lang,
                "当前选中的代理节点；单击展开详情，右键查看更多操作",
                "Currently selected proxy node; click for details, right-click for more actions",
            )
        }
    } else if group_context {
        super::tr(
            lang,
            "单击展开详情，双击设为该出站组的选用节点；右键查看更多操作",
            "Click for details, double-click to select for this outbound group; right-click for more actions",
        )
    } else {
        super::tr(
            lang,
            "单击展开详情，双击激活此节点；右键查看更多操作",
            "Click for details, double-click to activate this node; right-click for more actions",
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn http_row_controls(
    row_key: &str,
    target: HttpTestTarget,
    menu: MenuTarget,
    hover_group: SharedString,
    entry: Option<&LatencyEntry>,
    testing: bool,
    disabled: bool,
    offline: bool,
    lang: Language,
    p: &Palette,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let key = row_key;
    let label = super::theme::latency_label(entry, false, lang);
    let detail: SharedString = entry
        .map(|entry| {
            format!(
                "{} · {}",
                age_text(lang, latency_age_secs(entry)),
                latency_detail_text(lang, entry.detail.as_deref())
            )
        })
        .unwrap_or_else(|| {
            super::tr(
                lang,
                "尚未进行 HTTP 测速",
                "HTTP latency has not been measured",
            )
            .to_string()
        })
        .into();
    div()
        .id(SharedString::from(format!("http-controls-{key}")))
        .flex()
        .items_center()
        .gap_3()
        .flex_shrink_0()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .id(SharedString::from(format!("http-delay-{key}")))
                .w(px(super::theme::NODE_HTTP_WIDTH))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_end()
                .text_sm()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(latency_color(p, entry))
                .when(
                    entry.is_some_and(|entry| latency_age_secs(entry) > LATENCY_STALE_SECS),
                    |d| d.opacity(super::theme::DISABLED_OPACITY),
                )
                .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                .child(if testing {
                    Spinner::new().small().color(p.muted_fg).into_any_element()
                } else {
                    div().child(label).into_any_element()
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .opacity(0.)
                        .group_hover(hover_group, |s| s.opacity(1.))
                        .child(
                            Button::new(SharedString::from(format!("http-test-{key}")))
                                .compact()
                                .h(px(super::theme::CONTROL_HEIGHT))
                                .custom(icon_button_style(p, false, cx))
                                .icon(IconName::Gauge)
                                .tooltip(if offline {
                                    super::tr(
                                        lang,
                                        "出口设备离线，请先连接所属组网",
                                        "Exit device is offline; connect its mesh network first",
                                    )
                                } else if disabled {
                                    super::tr(
                                        lang,
                                        "请先启用节点，并等待当前任务完成",
                                        "Enable the node and wait for the current task to finish",
                                    )
                                } else {
                                    super::tr(lang, "HTTP 测速", "HTTP test")
                                })
                                .accessibility_label(super::tr(lang, "HTTP 测速", "HTTP test"))
                                .loading(testing)
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.service.test_http_target(target.clone());
                                    this.sync(cx);
                                })),
                        ),
                )
                .child(
                    more_button(
                        SharedString::from(format!("exit-more-{key}")),
                        super::tr(lang, "更多操作", "More actions"),
                        cx,
                    )
                    .dropdown_menu(menus::builder(cx.entity().downgrade(), menu)),
                ),
        )
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

fn latency_detail_text(lang: Language, detail: Option<&str>) -> String {
    match (lang, detail) {
        (Language::En, None | Some("HTTP 延迟")) => "HTTP latency".into(),
        (Language::En, Some("HTTP 延迟（统一延迟）")) => "HTTP latency (unified)".into(),
        (Language::En, Some("HTTP 延迟（含首次握手）")) => {
            "HTTP latency (with handshake)".into()
        }
        (Language::En, Some("TCP 延迟")) => "TCP latency".into(),
        (_, Some(text)) => text.to_owned(),
        (Language::Zh, None) => "HTTP 延迟".into(),
    }
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

fn health_tooltip_text(
    health: Option<&xrose_core::runtime::health::NodeHealthView>,
    expanded: bool,
    lang: Language,
) -> SharedString {
    use xrose_core::runtime::health::HealthDim;
    let action_hint = if expanded {
        super::tr(lang, "点击收起详情", "Click to collapse")
    } else {
        super::tr(lang, "点击展开详情", "Click to expand")
    };
    let Some(h) = health else {
        return format!(
            "{} · {action_hint}",
            super::tr(lang, "暂无健康样本", "No health samples")
        )
        .into();
    };
    let mut parts = Vec::new();
    if let Some(eval) = &h.general {
        parts.push(format!(
            "{} {} {} (15m {} / 24h {})",
            super::tr(lang, "综合", "Health"),
            eval.grade.as_str(),
            eval.score.round() as u32,
            eval.short_score.round() as u32,
            eval.long_score.round() as u32,
        ));
        if let Some(gate) = eval.gate {
            let g_label = match lang {
                Language::Zh => gate.label_zh(),
                Language::En => gate.label_en(),
            };
            parts.push(format!("{}: {g_label}", super::tr(lang, "门槛", "Gate")));
        }
        let mut dim_bits = Vec::new();
        for d in &eval.dims {
            let code = match d.dim {
                HealthDim::Availability => "A",
                HealthDim::Latency => "L",
                HealthDim::Stability => "S",
                HealthDim::Throughput => "T",
                HealthDim::Reachability => "R",
            };
            if let Some(s) = d.score {
                dim_bits.push(format!("{code} {}", s.round() as u32));
            }
        }
        if !dim_bits.is_empty() {
            parts.push(dim_bits.join(" "));
        }
        if eval.confidence < 0.40 {
            parts.push(format!(
                "{} {}%",
                super::tr(lang, "样本少", "Low conf"),
                (eval.confidence * 100.0).round() as u32
            ));
        }
    }
    if let Some(ai) = h.ai_score {
        parts.push(format!("AI {}", ai.round() as u32));
    }
    if let Some(media) = h.media_score {
        parts.push(format!(
            "{} {}",
            super::tr(lang, "流媒体", "Media"),
            media.round() as u32
        ));
    }
    parts.push(action_hint.to_string());
    parts.join(" · ").into()
}

fn render_health_capsule(
    health: Option<&xrose_core::runtime::health::NodeHealthView>,
    checking: bool,
    row_key: &str,
    expanded: bool,
    lang: Language,
    p: &Palette,
    cx: &mut Context<AppView>,
) -> Div {
    let tip = health_tooltip_text(health, expanded, lang);
    let inner = if checking {
        div()
            .px_2()
            .py(px(2.))
            .rounded_sm()
            .bg(p.panel_alt)
            .flex()
            .items_center()
            .justify_center()
            .child(Spinner::new().small().color(p.muted_fg))
            .into_any_element()
    } else if let Some(eval) = health.and_then(|h| h.general.as_ref()) {
        let (fg, bg) = super::theme::health_grade_colors(p, eval.grade);
        div()
            .px(px(6.))
            .py(px(2.))
            .rounded_sm()
            .bg(bg)
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .text_color(fg)
            .when(eval.confidence < 0.40, |d| d.opacity(0.75))
            .child(format!(
                "{} {}",
                eval.grade.as_str(),
                eval.score.round() as u32
            ))
            .into_any_element()
    } else {
        div()
            .px(px(6.))
            .py(px(2.))
            .rounded_sm()
            .bg(p.panel_alt)
            .text_xs()
            .font_family(cx.theme().mono_font_family.clone())
            .text_color(p.muted_fg)
            .child("—")
            .into_any_element()
    };

    let toggle_key = row_key.to_string();
    div()
        .w(px(super::theme::NODE_HEALTH_WIDTH))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_end()
        .child(
            div()
                .id(SharedString::from(format!("node-health-{row_key}")))
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_expanded_node(toggle_key.clone(), cx);
                }))
                .child(inner),
        )
}

/// Render a single node row: flush selection bar, radio, metadata, latency and
/// hover-revealed actions.
fn render_node_row(
    props: RowProps<'_>,
    p: &Palette,
    _window: &mut Window,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let RowProps {
        node,
        row_key,
        lang,
        selected,
        expanded,
        virtual_exit,
        chain_subtitle,
        fastest: is_fastest,
        latency,
        location,
        download,
        downloading,
        health,
        checking_reachability,
        hide_ips,
        magic_dns_suffixes,
        usable,
        auto_mode,
        group_context,
        offline_exit,
        mesh_provider,
        group_id,
        menu_group_id,
        source_summary,
        group_summary,
        direct_takeover_summary,
        probe_budget_used_bytes,
        daily_probe_budget_mb,
        core_busy: is_core_busy,
        latency_busy: is_latency_busy,
        testing,
        batch_mode,
        batch_checked: is_batch_checked,
        drag_enabled,
    } = props;
    let node_id_batch = node.id.clone();
    let mesh_gateway = node.is_mesh_gateway();
    let http_target = virtual_exit
        .as_ref()
        .map(VirtualExit::target)
        .unwrap_or_else(|| HttpTestTarget::Node(node.id.clone()));
    let row_menu = if virtual_exit.is_some() {
        MenuTarget::Exit(http_target.clone())
    } else {
        MenuTarget::Node {
            id: node.id.clone(),
            group_id: menu_group_id,
            row_key: row_key.clone(),
        }
    };
    let name = display_node_name(node, hide_ips, magic_dns_suffixes);
    let health_capsule = render_health_capsule(
        health,
        checking_reachability,
        &row_key,
        expanded,
        lang,
        p,
        cx,
    );
    let select_target = virtual_exit.clone();
    let select_group = group_id.clone();
    let select_node = node.id.clone();
    let drag_name = name.clone();
    let expand_key = row_key.clone();
    let chevron_key = row_key.clone();
    let can_select = if group_context {
        group_id.is_some() && node.enabled && !is_core_busy && !offline_exit
    } else {
        (virtual_exit.is_some() || group_id.is_some() || (mesh_gateway && node.enabled))
            && node.enabled
            && !is_core_busy
            && (!offline_exit
                || matches!(
                    &virtual_exit,
                    Some(VirtualExit::Mesh {
                        resumable: true,
                        ..
                    })
                ))
    };
    let auto_mode = if group_context {
        auto_mode
    } else {
        auto_mode && !mesh_gateway && virtual_exit.is_none()
    };
    let row_tip = row_tooltip(
        node,
        virtual_exit.as_ref(),
        selected,
        usable,
        auto_mode,
        group_context,
        group_id.is_some(),
        offline_exit,
        batch_mode,
        lang,
    );
    let detail_panel = expanded.then(|| {
        health_detail::render_node_detail_panel(
            health_detail::DetailPanelProps {
                node,
                row_key: &row_key,
                target: http_target.clone(),
                virtual_exit: virtual_exit.as_ref(),
                chain_subtitle: chain_subtitle.as_deref(),
                location,
                health,
                selected,
                hide_ips,
                offline_exit,
                mesh_provider,
                source_summary,
                group_summary,
                direct_takeover_summary,
                probe_busy: (testing, downloading, checking_reachability),
                any_probe_busy: is_latency_busy || is_core_busy,
                probe_disabled: !node.enabled,
                probe_budget_used_bytes,
                daily_probe_budget_mb,
                lang,
            },
            p,
            cx,
        )
    });
    let drag_tip = super::tr(lang, "拖拽调整节点顺序", "Drag to reorder");
    let pin_tip = super::tr(
        lang,
        "固定位置，不支持拖拽排序",
        "Pinned in place; cannot be reordered",
    );
    let sort_order_tip = super::tr(
        lang,
        "切换回「平铺 · 顺序」可拖拽排序",
        "Switch to Flat · Order to reorder by dragging",
    );
    let chevron_tip = if expanded {
        super::tr(lang, "收起节点详情", "Collapse node details")
    } else {
        super::tr(lang, "展开节点详情", "Expand node details")
    };
    let primary_badge = primary_badge_label(node, virtual_exit.as_ref(), lang);
    let endpoint_display = format_endpoint(&node.endpoint(), hide_ips);
    let is_node_busy = is_core_busy && selected;
    let drop_target = node.id.clone();
    let drag_primary = p.primary;
    let drag_accent = p.accent;
    let hover_bg = p.row_hover();
    let chevron_hover = p.panel_alt;
    let hover_group = SharedString::from(format!("node-group-{row_key}"));
    let is_pinned_exit = virtual_exit.is_some() || mesh_gateway;

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

    div()
        .id(SharedString::from(format!("node-{row_key}")))
        .group(hover_group.clone())
        .relative()
        .flex()
        .flex_col()
        .rounded_lg()
        .border_1()
        .border_color(p.border.opacity(0.))
        .bg(if selected { p.accent } else { p.row_bg() })
        .overflow_hidden()
        .when(!selected && !expanded, |d| d.hover(move |s| s.bg(hover_bg)))
        .when(drag_enabled, |d| {
            d.drag_over::<DraggedNode>(move |style, _, _, _| {
                style.border_color(drag_primary).bg(drag_accent)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedNode, _, cx| {
                if dragged.id != drop_target {
                    this.node_sort = super::NodeSort::Order;
                    this.service.move_node_to(&dragged.id, &drop_target);
                    this.sync(cx);
                }
            }))
        })
        .when(selected, |row| row.child(selection_bar(p.primary)))
        .child(
            div()
                .id(SharedString::from(format!("node-row-{row_key}")))
                .flex()
                .items_center()
                .gap_3()
                .pl_4()
                .pr_3()
                .h(px(super::theme::NODE_ROW_HEIGHT))
                .when(!node.enabled || offline_exit, |d| d.opacity(0.45))
                .tooltip(move |window, cx| Tooltip::new(row_tip.clone()).build(window, cx))
                .cursor_pointer()
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    if batch_mode {
                        if this.batch_selected.contains(&node_id_batch) {
                            this.batch_selected.remove(&node_id_batch);
                        } else {
                            this.batch_selected.insert(node_id_batch.clone());
                        }
                        cx.notify();
                        return;
                    }
                    if event.click_count() >= 2 {
                        this.cancel_row_expand_for_double_click(&expand_key, cx);
                        if can_select && !auto_mode && !selected {
                            select_row_exit(
                                this,
                                select_target.as_ref(),
                                select_group.as_deref(),
                                &select_node,
                                window,
                                cx,
                            );
                        }
                        return;
                    }
                    let immediate = !matches!(event, ClickEvent::Mouse(_));
                    this.schedule_row_expand(expand_key.clone(), immediate, cx);
                }))
                // Leading slot: drag handle for reorderable nodes, pin icon for fixed
                // system exits, or subtle dot when order is governed by sort/group view.
                .child(if drag_enabled {
                    div()
                        .id(SharedString::from(format!("drag-node-{row_key}")))
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .opacity(0.35)
                        .group_hover(hover_group.clone(), |s| s.opacity(1.))
                        .tooltip(move |window, cx| Tooltip::new(drag_tip.clone()).build(window, cx))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .on_drag(
                            DraggedNode {
                                id: node.id.clone(),
                                name: drag_name,
                            },
                            |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                        )
                        .child(
                            Icon::new(IconName::GripVertical)
                                .size(px(14.))
                                .text_color(p.muted_fg),
                        )
                        .into_any_element()
                } else if is_pinned_exit {
                    div()
                        .id(SharedString::from(format!("pin-node-{row_key}")))
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.35)
                        .tooltip(move |window, cx| Tooltip::new(pin_tip.clone()).build(window, cx))
                        .child(
                            Icon::new(IconName::Pin)
                                .size(px(13.))
                                .text_color(p.muted_fg),
                        )
                        .into_any_element()
                } else {
                    div()
                        .id(SharedString::from(format!("fixed-node-{row_key}")))
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .tooltip(move |window, cx| {
                            Tooltip::new(sort_order_tip.clone()).build(window, cx)
                        })
                        .child(
                            div()
                                .size(px(4.))
                                .rounded_full()
                                .bg(p.muted_fg.opacity(0.35)),
                        )
                        .into_any_element()
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
                                    primary_badge,
                                    if matches!(virtual_exit, Some(VirtualExit::Direct { .. })) {
                                        p.muted_fg
                                    } else {
                                        p.primary
                                    },
                                    p.panel,
                                ))
                                .when_some(mesh_provider, |d, provider| {
                                    d.child(pill(provider.label(), p.primary, p.panel))
                                })
                                .when(node.is_chain(), |d| {
                                    let hop_count = node.chain_hops.len();
                                    let hops_label = if lang == Language::Zh {
                                        format!("{hop_count} 跳")
                                    } else {
                                        format!("{hop_count} hops")
                                    };
                                    d.child(pill(hops_label, p.muted_fg, p.panel))
                                })
                                .when(node.is_chain() && node.enabled && !usable, |d| {
                                    d.child(pill(
                                        super::tr(lang, "配置异常", "Invalid"),
                                        p.warn_text,
                                        p.panel,
                                    ))
                                })
                                .when(!node.enabled, |d| {
                                    d.child(pill(
                                        super::tr(lang, "已禁用", "Disabled"),
                                        p.muted_fg,
                                        p.panel_alt,
                                    ))
                                })
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
                        .child(div().flex().items_center().gap_2().min_w(px(0.)).child(
                            if let Some(exit) = &virtual_exit {
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(exit.detail().to_owned())
                                    .into_any_element()
                            } else if let Some(chain_flow) = chain_subtitle {
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
                                            .child(chain_flow),
                                    )
                                    .when_some(location_label, |d, label| {
                                        let tip = label.clone();
                                        d.child(
                                            div().text_color(p.muted_fg.opacity(0.6)).child("·"),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "node-location-{row_key}"
                                                )))
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_color(p.muted_fg)
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(tip.clone()).build(window, cx)
                                                })
                                                .child(label),
                                        )
                                    })
                                    .into_any_element()
                            } else {
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
                                            .child(node.transport_label().to_uppercase()),
                                    )
                                    .when_some(location_label, |d, label| {
                                        let tip = label.clone();
                                        d.child(
                                            div().text_color(p.muted_fg.opacity(0.6)).child("·"),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "node-location-{row_key}"
                                                )))
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_color(p.muted_fg)
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(tip.clone()).build(window, cx)
                                                })
                                                .child(label),
                                        )
                                    })
                                    .into_any_element()
                            },
                        )),
                )
                // Download speed column: fixed width, right-aligned, mono. Placed to the left of health/latency.
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
                .child(health_capsule)
                .child(http_row_controls(
                    &row_key,
                    http_target,
                    row_menu.clone(),
                    hover_group,
                    latency,
                    testing,
                    is_latency_busy || is_core_busy || !node.enabled || offline_exit,
                    offline_exit,
                    lang,
                    p,
                    cx,
                ))
                .child(
                    div()
                        .id(SharedString::from(format!("node-chevron-{row_key}")))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(24.))
                        .h(px(24.))
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |s| s.bg(chevron_hover))
                        .tooltip(move |window, cx| {
                            Tooltip::new(chevron_tip.clone()).build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_expanded_node(chevron_key.clone(), cx);
                        }))
                        .text_color(if expanded { p.primary } else { p.muted_fg })
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(14.)),
                        ),
                ),
        )
        .children(detail_panel)
        .context_menu(menus::builder(cx.entity().downgrade(), row_menu))
}

#[cfg(test)]
mod tests {
    use super::clear_winner;

    #[test]
    fn row_range_is_bounded_by_the_viewport_even_for_large_subscriptions() {
        use gpui_kit::px;
        assert_eq!(
            super::node_row_range(px(-120.), px(160.), px(80.), 10_000),
            0..3
        );
        assert_eq!(
            super::node_row_range(px(80_000.), px(80_640.), px(80.), 10_000),
            999..1009
        );
        assert_eq!(
            super::node_row_range(px(760.), px(1100.), px(80.), 10),
            8..10
        );
    }

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

    #[test]
    fn mesh_exit_badges_match_across_discovered_and_saved_gateway_rows() {
        use super::{VirtualExit, primary_badge_label};
        use xrose_core::{
            mesh::MeshProvider,
            model::{Node, Protocol},
            settings::Language,
        };

        let mut gateway = Node::new("HomeNet · 出口", Protocol::Socks, "127.0.0.1", 19081);
        gateway.id = "mesh-exit-fixture".into();
        gateway.tags = vec!["mesh-exit".into()];

        let discovered = VirtualExit::Mesh {
            profile: "fixture".into(),
            peer: "peer-a".into(),
            detail: "HomeNet".into(),
            online: true,
            provider: MeshProvider::EasyTier,
            resumable: false,
        };

        assert_eq!(
            primary_badge_label(&gateway, None, Language::Zh),
            "组网出口"
        );
        assert_eq!(
            primary_badge_label(&gateway, Some(&discovered), Language::Zh),
            "组网出口"
        );
        assert_eq!(
            primary_badge_label(&gateway, Some(&discovered), Language::En),
            "Mesh exit"
        );

        let direct = VirtualExit::Direct {
            detail: "direct".into(),
        };
        assert_eq!(
            primary_badge_label(&gateway, Some(&direct), Language::Zh),
            "直连"
        );

        let proxy = Node::new("Example", Protocol::Vless, "198.51.100.1", 443);
        assert_eq!(primary_badge_label(&proxy, None, Language::Zh), "VLESS");
    }

    #[test]
    fn ordered_outbound_groups_places_default_first_and_preserves_order() {
        use super::ordered_outbound_groups;
        use xrose_core::model::{GroupKind, OutboundGroup};

        let mut a = OutboundGroup::new("A", GroupKind::Selector);
        a.id = "a".into();
        let mut b = OutboundGroup::new("B", GroupKind::Selector);
        b.id = "b".into();
        let mut c = OutboundGroup::new("C", GroupKind::Selector);
        c.id = "c".into();
        let groups = vec![a, b, c];

        let ids: Vec<_> = ordered_outbound_groups(&groups, Some("b"))
            .into_iter()
            .map(|g| g.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
    }

    #[test]
    fn row_tooltips_describe_action_and_unavailable_reasons() {
        use super::{VirtualExit, row_tooltip};
        use xrose_core::{
            mesh::MeshProvider,
            model::{Node, Protocol},
            settings::Language,
        };

        let proxy = Node::new("Example", Protocol::Vless, "198.51.100.1", 443);
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                true,
                false,
                false,
                true,
                false,
                false,
                Language::Zh
            ),
            "单击展开详情，双击激活此节点；右键查看更多操作"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                true,
                true,
                false,
                false,
                true,
                false,
                false,
                Language::Zh
            ),
            "当前选中的代理节点；单击展开详情，右键查看更多操作"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                Language::Zh
            ),
            "当前内核不支持此协议，请切换内核后再使用"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                true,
                true,
                false,
                false,
                false,
                false,
                Language::Zh
            ),
            "默认出站组为自动选择模式；单击展开详情，右键管理节点"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                Language::Zh
            ),
            "未加入当前默认出站组，可在右键菜单配置所属出站组"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                true,
                false,
                false,
                true,
                false,
                true,
                Language::Zh
            ),
            "点击勾选或取消勾选此节点"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                None,
                false,
                true,
                false,
                true,
                true,
                false,
                false,
                Language::Zh
            ),
            "单击展开详情，双击设为该出站组的选用节点；右键查看更多操作"
        );

        let mut gateway = Node::new("HomeNet · 出口", Protocol::Socks, "127.0.0.1", 19081);
        gateway.id = "mesh-exit-fixture".into();
        gateway.tags = vec!["mesh-exit".into()];
        assert_eq!(
            row_tooltip(
                &gateway,
                None,
                false,
                true,
                false,
                true,
                true,
                false,
                false,
                Language::Zh
            ),
            "单击展开详情，双击设为该出站组的分流出口（保持代理规则生效）"
        );

        let mesh_online = VirtualExit::Mesh {
            profile: "fixture".into(),
            peer: "peer-a".into(),
            detail: "HomeNet".into(),
            online: true,
            provider: MeshProvider::Tailscale,
            resumable: false,
        };
        assert_eq!(
            row_tooltip(
                &proxy,
                Some(&mesh_online),
                false,
                true,
                false,
                false,
                false,
                false,
                false,
                Language::Zh,
            ),
            "单击展开详情，双击进入组网模式并由此出口接管系统流量"
        );
        assert_eq!(
            row_tooltip(
                &proxy,
                Some(&mesh_online),
                false,
                true,
                false,
                false,
                false,
                true,
                false,
                Language::Zh,
            ),
            "出口设备离线，请先连接所属组网"
        );

        let mesh_resumable = VirtualExit::Mesh {
            profile: "fixture".into(),
            peer: "peer-a".into(),
            detail: "HomeNet".into(),
            online: false,
            provider: MeshProvider::Tailscale,
            resumable: true,
        };
        assert_eq!(
            row_tooltip(
                &proxy,
                Some(&mesh_resumable),
                false,
                true,
                false,
                false,
                false,
                true,
                false,
                Language::Zh,
            ),
            "单击展开详情，双击重连所属组网并由此出口接管系统流量"
        );
    }

    #[test]
    fn split_flat_nodes_places_mesh_gateways_before_proxies() {
        use xrose_core::model::{AppModel, Node, Protocol};

        let mut model = AppModel::new("fixture");
        let proxy_a = Node::new("Proxy A", Protocol::Vless, "203.0.113.10", 443);
        let mut mesh_a = Node::new("HomeNet · A", Protocol::Socks, "127.0.0.1", 19081);
        mesh_a.id = "mesh-exit-a".into();
        mesh_a.tags = vec!["mesh-exit".into()];
        let proxy_b = Node::new("Proxy B", Protocol::Vmess, "203.0.113.11", 443);
        let mut mesh_b = Node::new("OfficeNet · B", Protocol::Socks, "127.0.0.1", 19082);
        mesh_b.id = "mesh-exit-b".into();
        mesh_b.tags = vec!["mesh-exit".into()];
        model.nodes = vec![proxy_a, mesh_a, proxy_b, mesh_b];

        let (mesh_nodes, proxy_nodes) = super::split_flat_nodes(vec![0, 1, 2, 3], &model);
        assert_eq!(mesh_nodes, vec![1, 3]);
        assert_eq!(proxy_nodes, vec![0, 2]);
    }

    #[test]
    fn chain_rows_render_badge_subtitle_search_and_tooltip() {
        use xrose_core::model::{AppModel, ChainHop, Node, Protocol};
        use xrose_core::settings::Language;

        let mut model = AppModel::new("fixture");
        let entry = Node::new("HK-Edge", Protocol::Vless, "203.0.113.10", 443);
        let exit = Node::new("US-Home", Protocol::Socks, "203.0.113.20", 1080);
        model.add_nodes([entry.clone(), exit.clone()]);
        let chain = Node::new_chain(
            "AI Residential",
            vec![
                ChainHop::Node(entry.id.clone()),
                ChainHop::Node(exit.id.clone()),
            ],
        );
        model.nodes.push(chain.clone());

        assert_eq!(
            super::primary_badge_label(&chain, None, Language::Zh),
            "代理链"
        );
        assert_eq!(
            super::primary_badge_label(&chain, None, Language::En),
            "Chain"
        );
        assert_eq!(
            super::chain_row_subtitle(&chain, &model, false, &[], Language::Zh).as_deref(),
            Some("HK-Edge ──▶ US-Home")
        );
        assert!(super::node_matches_query(&chain, &model, "hk-edge"));
        assert!(super::node_matches_query(&chain, &model, "代理链"));
        assert!(super::node_matches_query(&chain, &model, "chain"));
        assert_eq!(
            super::row_tooltip(
                &chain,
                None,
                false,
                false,
                false,
                false,
                true,
                false,
                false,
                Language::Zh
            ),
            "代理链存在已禁用、缺失或当前内核不支持的跳点，请编辑后使用"
        );
    }

    #[test]
    fn accordion_toggle_expands_switches_and_collapses_single_card() {
        let mut expanded: Option<String> = None;
        super::toggle_accordion(&mut expanded, "node-a".into());
        assert_eq!(expanded.as_deref(), Some("node-a"));

        super::toggle_accordion(&mut expanded, "node-b".into());
        assert_eq!(expanded.as_deref(), Some("node-b"));

        super::toggle_accordion(&mut expanded, "node-b".into());
        assert_eq!(expanded, None);
    }

    #[test]
    fn double_click_restores_expanded_state_only_for_matching_row() {
        let mut expanded = Some("node-b".to_string());
        let mut pre_click = Some(("node-b".to_string(), Some("node-a".to_string())));
        assert!(super::restore_pre_double_click_expand(
            &mut expanded,
            &mut pre_click,
            "node-b",
        ));
        assert_eq!(expanded.as_deref(), Some("node-a"));
        assert_eq!(pre_click, None);

        let mut expanded = Some("node-b".to_string());
        let mut pre_click = Some(("node-b".to_string(), None));
        assert!(!super::restore_pre_double_click_expand(
            &mut expanded,
            &mut pre_click,
            "node-c",
        ));
        assert_eq!(expanded.as_deref(), Some("node-b"));
        assert_eq!(pre_click, None);
    }
}
