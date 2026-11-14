mod direct_entry;
mod mesh_exits;
mod outbound_groups;
mod switching;

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
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

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NodeSection<'a> {
    Manual,
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
                                    .label(self.tr("下载 Core", "Install core"))
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

        let sort_active = has_nodes && self.sort_by_latency;
        let grouped = has_nodes && self.group_by_subscription;

        let search_query = if has_nodes {
            self.node_search.read(cx).value().trim().to_lowercase()
        } else {
            String::new()
        };
        let mut display_nodes: Vec<usize> = model
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| !node.is_mesh_gateway() || !discovered_gateways.contains(&node.id))
            .filter(|(_, n)| {
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
            .map(|(index, _)| index)
            .collect();

        // Sorting only changes the view; the stored order is left alone.
        if sort_active {
            display_nodes.sort_by_key(|&index| {
                let n = &model.nodes[index];
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
        let batch_selected = &self.batch_selected;
        let latency_busy = this_busy(&self.snapshot.busy, "latency");
        let tests_busy = latency_busy
            || this_busy(&self.snapshot.busy, "download")
            || this_busy(&self.snapshot.busy, "auto-probe");
        let exit_busy = this_busy(&self.snapshot.busy, "mesh-exit");
        let core_busy = is_core_busy || exit_busy;
        let node_count = model
            .nodes
            .iter()
            .filter(|node| !node.is_mesh_gateway() || !discovered_gateways.contains(&node.id))
            .count()
            + exits.len();
        let row_gap = rems(if grouped { 0.5 } else { 0.75 }).to_pixels(window.rem_size());
        let exits: Vec<_> = exits
            .into_iter()
            .filter(|exit| exit.matches(&search_query, hide_ips))
            .collect();
        let shown_exits = exits.len();
        let shown = display_nodes.len() + shown_exits;
        let mut exit_rows = (shown_exits != 0).then(|| {
            let busy = core_busy || this_busy(&self.snapshot.busy, "mesh") || batch_mode;
            node_rows(
                shown_exits,
                px(super::theme::NODE_ROW_HEIGHT),
                row_gap,
                cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                    exits[range]
                        .iter()
                        .map(|exit| exit.row(busy, hide_ips, lang, &p, cx).into_any_element())
                        .collect()
                }),
            )
        });

        let offline_exits: std::collections::HashSet<_> = self
            .snapshot
            .mesh
            .exits()
            .filter(|(_, peer, _)| peer.exit_selected && !peer.online)
            .map(|(profile, _, _)| format!("mesh-exit-{}", profile.id))
            .collect();
        let row_group = active_group.clone();
        let view = cx.entity();
        let render_rows = std::rc::Rc::new(
            move |indices: &[usize], window: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    indices
                        .iter()
                        .map(|&index| {
                            let node = &this.snapshot.model.nodes[index];
                            render_node_row(
                                RowProps {
                                    node,
                                    lang,
                                    selected: active_node.as_deref() == Some(node.id.as_str())
                                        && (!node.is_mesh_gateway()
                                            || this.snapshot.model.work_mode
                                                == xrose_core::model::WorkMode::Mesh),
                                    running: this.snapshot.runtime.running_proxy_node.as_deref()
                                        == Some(node.id.as_str()),
                                    fastest: fastest_node_id.as_deref() == Some(node.id.as_str()),
                                    latency: this.snapshot.latency.get(&node.id),
                                    location: this.snapshot.node_location(node),
                                    download: this.snapshot.downloads.get(&node.id),
                                    downloading: this_busy(
                                        &this.snapshot.busy,
                                        &format!("download:{}", node.id),
                                    ),
                                    hide_ips,
                                    auto_mode,
                                    offline_exit: offline_exits.contains(&node.id),
                                    mesh_provider: node
                                        .id
                                        .strip_prefix("mesh-exit-")
                                        .and_then(|id| {
                                            this.snapshot
                                                .mesh
                                                .profiles
                                                .iter()
                                                .find(|profile| profile.id == id)
                                        })
                                        .map(|profile| profile.provider),
                                    group_id: row_group
                                        .as_ref()
                                        .filter(|_| {
                                            selectable_members.contains(&node.id)
                                                && node.usable_with(this.snapshot.settings.core)
                                        })
                                        .cloned(),
                                    core_busy,
                                    latency_busy: tests_busy,
                                    testing: latency_busy
                                        && node.enabled
                                        && this.latency_pending.contains(&node.id),
                                    batch_mode,
                                    batch_checked: this.batch_selected.contains(&node.id),
                                    drag_enabled: !sort_active,
                                },
                                &p,
                                window,
                                cx,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                })
            },
        );

        let configured_rows = |indices: Vec<usize>| {
            let render_rows = render_rows.clone();
            // The outer border adds one pixel on either side of the row.
            node_rows(
                indices.len(),
                px(super::theme::NODE_ROW_HEIGHT + 2.),
                row_gap,
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
                                        format!("{} {}", shown, self.tr("个", ""))
                                    } else {
                                        format!("{}/{} {}", shown, node_count, self.tr("个", ""))
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
                                toolbar_icon_button(
                                    "test-all",
                                    IconName::Gauge,
                                    self.tr("全部节点测速", "Test all nodes"),
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
                                        this.service.test_latency(LatencyScope::All);
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
                            })
                            .child(self.privacy_toggle_button(
                                "toggle-privacy",
                                self.tr("隐藏服务器地址与位置", "Hide addresses and location"),
                                cx,
                            )),
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
                    container
                        .when(!display_nodes.is_empty(), |d| {
                            d.child(configured_rows(std::mem::take(&mut display_nodes)))
                        })
                        .children(exit_rows.take())
                })
                .when(grouped, |container| {
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
                            NodeSection::Manual => "manual".into(),
                            NodeSection::Subscription(id) => format!("subscription-{id}"),
                        };
                        let is_collapsed = collapsed_set.contains(&group_key);
                        let sub_title = match section {
                            NodeSection::MeshExits => {
                                self.tr("组网出口节点", "Mesh exit nodes").to_string()
                            }
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
                                        header.child(
                                            Button::new(SharedString::from(format!(
                                                "ping-sub-{}",
                                                group_key
                                            )))
                                            .compact()
                                            .ghost()
                                            .label(self.tr("测速本组", "Test group"))
                                            .disabled(tests_busy)
                                            .tooltip(self.tr(
                                                "测试本组已配置节点的延迟",
                                                "Test latency of configured nodes in this group",
                                            ))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.latency_pending
                                                    .extend(node_ids_for_ping.iter().cloned());
                                                this.service.test_latency(LatencyScope::Nodes(
                                                    node_ids_for_ping.clone(),
                                                ));
                                                this.sync(cx);
                                            })),
                                        )
                                    }),
                            )
                            .when(!is_collapsed, |d| {
                                d.when(!group_nodes.is_empty(), |d| {
                                    d.child(configured_rows(group_nodes))
                                })
                                .when(mesh, |d| d.children(exit_rows.take()))
                            })
                    }))
                }),
        );

        body
    }
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
    running: bool,
    fastest: bool,
    latency: Option<&'a LatencyEntry>,
    location: Option<&'a xrose_core::runtime::location::NodeLocation>,
    download: Option<&'a xrose_core::runtime::probe::DownloadResult>,
    downloading: bool,
    hide_ips: bool,
    auto_mode: bool,
    offline_exit: bool,
    mesh_provider: Option<xrose_core::mesh::MeshProvider>,
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
        running,
        fastest: is_fastest,
        latency,
        location,
        download,
        downloading,
        hide_ips,
        auto_mode,
        offline_exit,
        mesh_provider,
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
    let mesh_gateway = node.is_mesh_gateway();
    let can_select =
        (group_id.is_some() || (mesh_gateway && node.enabled)) && !is_core_busy && !offline_exit;
    let auto_mode = auto_mode && !mesh_gateway;
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
        .when(selected, |row| row.child(selection_bar(p.primary)))
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
                    if mesh_gateway || group_id.is_some() {
                        this.service.select_node(
                            if mesh_gateway {
                                ""
                            } else {
                                group_id.as_deref().unwrap_or("")
                            },
                            &node_id_select,
                        );
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
                                .when_some(mesh_provider, |d, provider| {
                                    d.child(pill(provider.label(), p.primary, p.panel))
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
                                        .custom(icon_button_style(p, false, cx))
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
                                cx,
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
                            .disabled(running)
                            .tooltip(if running {
                                super::tr(
                                    lang,
                                    "节点正在使用，请先切换节点或停止内核",
                                    "Switch nodes or stop the core before disabling this node",
                                )
                            } else {
                                super::tr(
                                    lang,
                                    "允许此节点参与连接与自动选择",
                                    "Allow this node for connections and automatic selection",
                                )
                            })
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
}
