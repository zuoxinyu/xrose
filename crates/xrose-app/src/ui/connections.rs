//! Live connections and interactive request diagnostics.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::diagnostics::{
    DiagnosticRuleKind, DiagnosticTarget, connection_matches_query, connection_test_target,
};
use xrose_core::flow::{ConnectionMetrics, ConnectionSort, ProcessLiveTraffic};
use xrose_core::model::{CoreKind, RouteAction, RouteActionKind, WorkMode};
use xrose_core::runtime::ClashConnection;
use xrose_core::service::ToastLevel;
use xrose_core::trace::{
    TracePhase, TraceProtocol, TraceSession, TraceStatus, TraceStepKind, parse_target,
};
use xrose_core::util::format_bytes;

use super::menus::{self, MenuTarget};
use super::theme::{Palette, palette};
use super::widgets::*;
use super::{AppView, Page};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};

const CONNECTIONS_PAGE_STEP: usize = 50;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ConnectionsTab {
    #[default]
    Live,
    Processes,
    Recorded,
    Statistics,
}

enum MonitorRow<'a> {
    Connection(&'a ClashConnection, bool, Option<ConnectionMetrics>),
    Process(&'a ProcessLiveTraffic),
}

impl MonitorRow<'_> {
    fn icon_key<'a>(&'a self, connections: &'a [ClashConnection]) -> &'a str {
        let (path, name) = match self {
            Self::Connection(c, ..) => (c.process_path(), c.process_basename()),
            Self::Process(p) => (
                p.connection_with_path
                    .and_then(|i| connections.get(i))
                    .map_or("", ClashConnection::process_path),
                p.process_name.as_str(),
            ),
        };
        if path.is_empty() { name } else { path }
    }
}

impl AppView {
    pub(super) fn connection_search_placeholder(&self) -> SharedString {
        if self.connections_tab == ConnectionsTab::Processes {
            self.tr("搜索进程名…", "Search processes…")
        } else {
            self.tr(
                "搜索域名、IP、端口、进程或规则…",
                "Search domain, IP, port, process or rule…",
            )
        }
    }

    fn select_connections_tab(
        &mut self,
        tab: ConnectionsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.connections_tab == tab {
            return;
        }
        self.connections_tab = tab;
        self.exact_process_filter = None;
        self.connections_limit = CONNECTIONS_PAGE_STEP;
        let placeholder = self.connection_search_placeholder();
        self.connection_search.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        if tab == ConnectionsTab::Statistics {
            self.refresh_statistics(false, cx);
        } else {
            cx.notify();
        }
    }

    fn reset_connection_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.exact_process_filter = None;
        self.connections_limit = CONNECTIONS_PAGE_STEP;
        cx.notify();
    }

    fn toggle_process_icons(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(icons) = self.process_icons.take() {
            icons.clear(window, cx);
        } else {
            self.process_icons = Some(Default::default());
        }
        cx.notify();
    }

    pub(crate) fn page_connections(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let tab = self.connections_tab;
        let supported = self
            .snapshot
            .process
            .core
            .unwrap_or(self.snapshot.settings.core)
            == CoreKind::SingBox;
        let running = self.snapshot.process.phase.is_alive();
        let query = self.connection_search.read(cx).value().trim().to_string();
        let limit = self.connections_limit.max(CONNECTIONS_PAGE_STEP);

        let live = &self.snapshot.runtime.live_traffic;
        let matches = |c: &ClashConnection| {
            if query.is_empty() {
                true
            } else if tab == ConnectionsTab::Live
                && self.exact_process_filter.as_ref() == Some(&query)
            {
                c.process_name() == query
            } else {
                connection_matches_query(c, &query)
            }
        };
        // Store indices in display order. The list resolves rich rows and icons only
        // when it measures or paints them, including variable-height wrapped rows.
        let (visible, count): (Vec<usize>, usize) = match tab {
            ConnectionsTab::Statistics => (Vec::new(), 0),
            ConnectionsTab::Recorded => {
                let all = &self.snapshot.recording.connections;
                let mut rows = (0..all.len())
                    .rev()
                    .filter(|&i| matches(&all[i].connection));
                let visible: Vec<_> = rows.by_ref().take(limit).collect();
                let count = if query.is_empty() {
                    all.len()
                } else {
                    visible.len() + rows.count()
                };
                (visible, count)
            }
            ConnectionsTab::Processes => {
                let text = query.to_lowercase();
                let mut rows = (0..live.processes.len()).filter(|&i| {
                    text.is_empty()
                        || live.processes[i]
                            .process_name
                            .to_lowercase()
                            .contains(&text)
                });
                let visible: Vec<_> = rows.by_ref().take(limit).collect();
                let count = if query.is_empty() {
                    live.processes.len()
                } else {
                    visible.len() + rows.count()
                };
                (visible, count)
            }
            ConnectionsTab::Live => {
                let all = &self.snapshot.runtime.connections;
                let mut rows = live
                    .ordered(self.connection_sort)
                    .iter()
                    .copied()
                    .filter(|&i| all.get(i).is_some_and(matches));
                let visible: Vec<_> = rows.by_ref().take(limit).collect();
                let count = if query.is_empty() {
                    all.len()
                } else {
                    visible.len() + rows.count()
                };
                (visible, count)
            }
        };
        let shown = visible.len();
        let is_live_tab = matches!(tab, ConnectionsTab::Live | ConnectionsTab::Processes);
        let has_hint = tab != ConnectionsTab::Statistics
            && ((is_live_tab && (!supported || !running)) || count == 0);
        let has_footer =
            tab != ConnectionsTab::Statistics && (count > shown || limit > CONNECTIONS_PAGE_STEP);
        let panel_count = usize::from(has_hint) + shown + usize::from(has_footer);
        let item_count = 1 + panel_count;
        let old_count = self.connection_list.item_count();
        if old_count != item_count {
            self.connection_list.splice(
                old_count.min(item_count)..old_count,
                item_count.saturating_sub(old_count),
            );
        }
        let filtered = !query.is_empty();
        list(
            self.connection_list.clone(),
            cx.processor(move |this, index: usize, _, cx| {
                let pad = px(super::theme::PAGE_PAD);
                let first = index == 0;
                let last = index + 1 == item_count;
                let content = if first {
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(this.connection_header(supported, running, cx))
                        .when(tab == ConnectionsTab::Statistics, |panel| {
                            panel.child(this.traffic_statistics_content(&p, cx))
                        })
                        .into_any_element()
                } else if has_hint && index == 1 {
                    this.connection_hint(supported, running, filtered, count, &p, cx)
                        .into_any_element()
                } else if last && has_footer {
                    this.connection_footer(count, shown, limit, &p, cx)
                        .into_any_element()
                } else {
                    let i = visible[index - 1 - usize::from(has_hint)];
                    let row = match tab {
                        ConnectionsTab::Recorded => {
                            let r = &this.snapshot.recording.connections[i];
                            MonitorRow::Connection(&r.connection, r.closed_at.is_some(), None)
                        }
                        ConnectionsTab::Processes => {
                            MonitorRow::Process(&this.snapshot.runtime.live_traffic.processes[i])
                        }
                        ConnectionsTab::Live | ConnectionsTab::Statistics => {
                            MonitorRow::Connection(
                                &this.snapshot.runtime.connections[i],
                                false,
                                this.snapshot.runtime.live_traffic.metrics.get(i).copied(),
                            )
                        }
                    };
                    let icon = this
                        .process_icons
                        .as_ref()
                        .map(|icons| icons.view(row.icon_key(&this.snapshot.runtime.connections)));
                    match row {
                        MonitorRow::Connection(connection, closed, metrics) => this
                            .connection_row(connection.clone(), closed, metrics, icon, &p, cx)
                            .into_any_element(),
                        MonitorRow::Process(process) => this
                            .live_process_row(process, icon, &p, cx)
                            .into_any_element(),
                    }
                };
                div()
                    .px(pad)
                    .when(first, |d| d.pt(pad))
                    .when(last, |d| d.pb(pad))
                    .child(
                        div()
                            .px_4()
                            .border_x_1()
                            .bg(p.panel)
                            .border_color(p.border)
                            .when(first, |d| d.pt_4().border_t_1().rounded_t_lg())
                            .when(first && !last, |d| d.pb_3())
                            .when(!first && !last, |d| d.pb_2())
                            .when(last, |d| d.pb_4().border_b_1().rounded_b_lg())
                            .child(content),
                    )
                    .into_any_element()
            }),
        )
        .size_full()
    }

    fn connection_header(
        &self,
        supported: bool,
        running: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let p = palette(cx);
        let tab = self.connections_tab;
        let recording_active = self.snapshot.recording.active;
        let recording_started = self.snapshot.recording.started_at.is_some();
        let recorded_conn_len = self.snapshot.recording.connections.len();
        let evicted_conns = self.snapshot.recording.evicted_connections;
        let evicted_traces = self.snapshot.recording.evicted_traces;
        let export_busy = self
            .snapshot
            .busy
            .iter()
            .any(|key| key == "recording-export");
        let active_conn_len = self.snapshot.runtime.connections.len();
        let active_process_len = self.snapshot.runtime.live_traffic.processes.len();

        let sort_choice = ChoiceGroup::new(
            "connection-sort",
            vec![
                self.tr("最新建立", "Newest"),
                self.tr("实时速率", "Speed"),
                self.tr("累计流量", "Total traffic"),
            ],
            match self.connection_sort {
                ConnectionSort::Newest => 0,
                ConnectionSort::Speed => 1,
                ConnectionSort::Total => 2,
            },
        )
        .on_change(cx.listener(|this, index, _, cx| {
            this.connection_sort = [
                ConnectionSort::Newest,
                ConnectionSort::Speed,
                ConnectionSort::Total,
            ][*index];
            this.connections_limit = CONNECTIONS_PAGE_STEP;
            cx.notify();
        }));
        let limited_capture = matches!(tab, ConnectionsTab::Live | ConnectionsTab::Processes)
            && running
            && self.snapshot.model.work_mode == WorkMode::Direct
            && !self.snapshot.settings.tun_enabled;
        let title = div().flex().items_center().flex_wrap().gap_2().child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.fg)
                .child(self.tr("实时连接与统计", "Live connections & statistics")),
        );

        let selected_tab = match tab {
            ConnectionsTab::Live => 0,
            ConnectionsTab::Processes => 1,
            ConnectionsTab::Recorded => 2,
            ConnectionsTab::Statistics => 3,
        };
        let primary_tabs = div().flex().items_center().child(
            SegmentedTabs::new(
                "connections-primary-tabs",
                vec![
                    SegmentedTab::new(
                        "connections-tab-live",
                        self.tr("实时连接", "Live"),
                        active_conn_len.to_string(),
                    ),
                    SegmentedTab::new(
                        "connections-tab-processes",
                        self.tr("活跃进程", "Active processes"),
                        active_process_len.to_string(),
                    ),
                    SegmentedTab::new(
                        "connections-tab-recorded",
                        self.tr("录制回放", "Recorded"),
                        recorded_conn_len.to_string(),
                    ),
                    SegmentedTab::new(
                        "connections-tab-stats",
                        self.tr("流量统计", "Traffic stats"),
                        self.traffic_statistics_badge(),
                    )
                    .icon(Some(IconName::ChartColumn)),
                ],
                Some(selected_tab),
            )
            .on_change(cx.listener(|this, index, window, cx| {
                let next = [
                    ConnectionsTab::Live,
                    ConnectionsTab::Processes,
                    ConnectionsTab::Recorded,
                    ConnectionsTab::Statistics,
                ][*index];
                this.select_connections_tab(next, window, cx);
            })),
        );

        let process_icons_shown = self.process_icons.is_some();
        let refresh_busy = self.service.is_busy("connections-refresh");
        let build_toolbar_controls = |include_refresh: bool, cx: &mut Context<Self>| {
            let refresh_button = toolbar_icon_button(
                "connections-refresh",
                IconName::RefreshCw,
                self.tr("刷新", "Refresh"),
                cx,
            )
            .disabled(!running || !supported)
            .loading(refresh_busy)
            .on_click(cx.listener(|this, _, _, cx| {
                this.service.refresh_connections();
                this.sync(cx);
            }));
            let process_icons_button = icon_button_toggled(
                toolbar_icon_button(
                    "connections-process-icons",
                    IconName::AppWindow,
                    self.tr("显示应用图标", "Show application icons"),
                    cx,
                ),
                process_icons_shown,
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_process_icons(window, cx);
            }));
            let privacy_button = self.privacy_toggle_button(
                "connections-privacy",
                self.tr("隐藏目标地址与节点名", "Hide addresses and node names"),
                cx,
            );
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .when(include_refresh, |d| d.child(refresh_button))
                .child(process_icons_button)
                .child(privacy_button)
        };

        let search_input = div()
            .flex_1()
            .min_w(px(180.))
            .child(Input::new(&self.connection_search));

        let mut header = div()
            .flex()
            .flex_col()
            .gap_3()
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
                            .flex_col()
                            .gap_1()
                            .child(title)
                            .child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                                "查看实时连接详情、录制瞬时连接或按维度分析历史流量归因",
                                "Inspect live connections, record short-lived links, or analyze traffic attribution",
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                more_button(
                                    "connection-actions",
                                    self.tr("连接操作", "Connection actions"),
                                    cx,
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::Connections,
                                )),
                            )
                            .child(
                                Button::new("export-recording")
                                    .label(self.tr("导出录制", "Export recording"))
                                    .disabled(!recording_started)
                                    .loading(export_busy)
                                    .on_click(
                                        cx.listener(|this, _, _, _| {
                                            this.service.export_recording()
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("record-connections")
                                    .when(recording_active, |b| b.danger())
                                    .label(if recording_active {
                                        self.tr("停止录制", "Stop recording")
                                    } else {
                                        self.tr("开始录制", "Start recording")
                                    })
                                    .icon(if recording_active {
                                        IconName::Square
                                    } else {
                                        IconName::Play
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.snapshot.recording.active {
                                            this.service.stop_recording();
                                        } else {
                                            this.service.start_recording();
                                        }
                                        this.sync(cx);
                                    })),
                            ),
                    ),
            )
            .child(primary_tabs);
        match tab {
            ConnectionsTab::Live => {
                let toolbar_controls = build_toolbar_controls(true, cx);
                header = header.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .when(supported, |d| d.child(sort_choice))
                        .child(search_input)
                        .child(toolbar_controls),
                );
            }
            ConnectionsTab::Processes => {
                let toolbar_controls = build_toolbar_controls(true, cx);
                header = header.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(search_input)
                        .child(toolbar_controls),
                );
            }
            ConnectionsTab::Recorded => {
                let toolbar_controls = build_toolbar_controls(false, cx);
                header = header
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(search_input)
                            .child(toolbar_controls),
                    )
                    .when(recording_started || evicted_conns + evicted_traces > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .flex_wrap()
                                .when(recording_started, |n| {
                                    n.child(div().text_xs().text_color(p.muted_fg).child(
                                        self.tr(
                                            "仅保留本次会话的连接与测试信息，不采集正文。",
                                            "Stores connection and test metadata for this session, without message bodies.",
                                        ),
                                    ))
                                })
                                .when(evicted_conns + evicted_traces > 0, |n| {
                                    n.child(div().text_xs().text_color(p.warn_text).child(
                                        self.tr(
                                            "容量已满，较早的记录已移除。",
                                            "Capacity reached; older records have been removed.",
                                        ),
                                    ))
                                }),
                        )
                    });
            }
            ConnectionsTab::Statistics => {}
        }
        header.when(limited_capture, |d| {
            let hint = self.tr(
                "当前仅捕获系统代理流量；开启 TUN 模式可监控本机全部进程连接",
                "Only local proxy traffic is visible. Enable TUN to monitor connections from all processes.",
            );
            let enable = Button::new("connections-enable-tun")
                .compact()
                .ghost()
                .label(self.tr("开启 TUN", "Enable TUN"))
                .disabled(self.snapshot.busy.iter().any(|b| b == "core"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.service.set_tun_enabled(true);
                    this.sync(cx);
                }));
            d.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .child(div().text_xs().text_color(p.muted_fg).child(hint))
                    .child(enable),
            )
        })
    }

    fn connection_hint(
        &self,
        supported: bool,
        running: bool,
        filtered: bool,
        count: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut rows = div().flex().flex_col().gap_2().w_full();
        let tab = self.connections_tab;
        let is_live_tab = matches!(tab, ConnectionsTab::Live | ConnectionsTab::Processes);
        if is_live_tab && !supported {
            rows = rows.child(
                empty_state(
                    p,
                    IconName::Info,
                    self.tr("当前 Core 不提供连接列表", "Connection list unavailable"),
                    self.tr(
                        "可使用 sing-box 查看连接，或前往测试页面",
                        "Use sing-box to inspect connections, or open Diagnostics",
                    ),
                )
                .child(
                    Button::new("connections-diagnostics")
                        .primary()
                        .label(self.tr("前往测试", "Open diagnostics"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Diagnostics;
                            cx.notify();
                        })),
                ),
            );
        } else if is_live_tab && !running {
            rows = rows.child(
                empty_state(
                    p,
                    IconName::Power,
                    self.tr("Core 未运行", "Core stopped"),
                    self.tr(
                        "启动后可查看实时连接",
                        "Start the core to view live connections",
                    ),
                )
                .child(
                    Button::new("connections-start")
                        .custom(start_button(cx))
                        .bg(p.primary)
                        .label(self.tr("启动", "Start"))
                        .loading(self.snapshot.busy.iter().any(|key| key == "core"))
                        .on_click(cx.listener(|this, _, _, _| this.service.start_core())),
                ),
            );
        } else if count == 0 {
            rows = rows.child(
                empty_state(
                    p,
                    IconName::ArrowLeftRight,
                    if filtered {
                        if tab == ConnectionsTab::Processes {
                            self.tr("没有符合条件的进程", "No matching processes")
                        } else {
                            self.tr("没有符合条件的连接", "No matching connections")
                        }
                    } else if tab == ConnectionsTab::Processes {
                        self.tr("暂无活跃进程", "No active processes yet")
                    } else if tab == ConnectionsTab::Recorded {
                        self.tr("暂无已录制的连接快照", "No recorded connections yet")
                    } else {
                        self.tr("暂无连接记录", "No connections yet")
                    },
                    if filtered {
                        self.tr("尝试清除筛选条件", "Try clearing the filters")
                    } else if tab == ConnectionsTab::Recorded {
                        self.tr(
                            "点击右上角「开始录制」可捕获瞬时短连接与已结束连接的历史记录",
                            "Click \"Start recording\" above to capture short-lived and closed connections",
                        )
                    } else {
                        self.tr(
                            "连接产生后会显示在这里",
                            "Connections will appear here when available",
                        )
                    },
                )
                .when(filtered, |d| {
                    d.child(
                        Button::new("empty-reset-connections")
                            .primary()
                            .label(self.tr("清除筛选", "Clear filters"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_connection_filters(window, cx);
                            })),
                    )
                }),
            )
        }
        rows
    }

    fn connection_footer(
        &self,
        count: usize,
        shown: usize,
        limit: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .pt_2()
            .flex_wrap()
            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                "{} {shown} / {count}",
                self.tr("当前显示", "Showing")
            )))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(count > shown, |d| {
                        d.child(
                            Button::new("load-more-connections")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .ghost()
                                .label(self.tr("加载更多 (+50)", "Load 50 more"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connections_limit += CONNECTIONS_PAGE_STEP;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(limit > CONNECTIONS_PAGE_STEP, |d| {
                        d.child(
                            Button::new("collapse-connections")
                                .compact()
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .ghost()
                                .label(self.tr("收起", "Show less"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connections_limit = CONNECTIONS_PAGE_STEP;
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    pub(crate) fn page_diagnostics(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        if let Some(selected) = self
            .selected_connection
            .as_mut()
            .filter(|_| self.connections_tab != ConnectionsTab::Recorded)
        {
            if let Some(latest) = self
                .snapshot
                .runtime
                .connections
                .iter()
                .find(|c| c.id == selected.id && c.start == selected.start)
            {
                *selected = latest.clone();
            }
        }
        let inspector = if let Some(connection) = self.selected_connection.clone() {
            self.connection_inspector(&connection, cx)
                .into_any_element()
        } else {
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .flex_wrap()
                        .child(section_title(
                            &p,
                            self.tr("连接透视", "Connection inspector"),
                            self.tr(
                                "在「连接」页面点击某条连接的透视按钮，可在此查看发起方、命中规则与出站链并快速生成规则",
                                "Click a connection's inspect button on the Connections page to view its source, matched rule and outbound chain",
                            ),
                        ))
                        .child(
                            Button::new("go-to-connections")
                                .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .ghost()
                                .icon(Page::Connections.icon())
                                .label(self.tr("前往连接列表", "Go to connections"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.page = Page::Connections;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element()
        };

        let mut body = div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .child(
                div()
                    .id("trace-section")
                    .anchor_scroll(Some(self.trace_anchor.clone()))
                    .child(self.trace_card(cx)),
            )
            .child(
                div()
                    .id("inspector-section")
                    .anchor_scroll(Some(self.inspector_anchor.clone()))
                    .child(inspector),
            )
            .child(self.dns_comparison_card(cx))
            .child(self.system_report_card(cx))
            .child(self.stun_diagnostic_card(cx))
            .child(self.settings_helper_card(cx));

        let traces = self.snapshot.recording.traces.clone();
        if !traces.is_empty() {
            body = body.child(
                card(&p)
                    .child(section_title(
                        &p,
                        self.tr("录制的测试", "Recorded tests"),
                        self.tr(
                            "选择记录查看完整链路及失败阶段",
                            "Select a recording to review its path and failure stage",
                        ),
                    ))
                    .children(traces.iter().rev().map(|trace| {
                        let id = trace.id;
                        Button::new(SharedString::from(format!("replay-trace-{id}")))
                            .compact()
                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .ghost()
                            .label(format!(
                                "{} · {}",
                                if trace.success == Some(true) {
                                    "✓"
                                } else {
                                    "!"
                                },
                                trace.target
                            ))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(trace) =
                                    this.snapshot.recording.traces.iter().find(|t| t.id == id)
                                {
                                    let target = trace.target.clone();
                                    this.trace_input.update(cx, |input, cx| {
                                        input.set_value(target, window, cx)
                                    });
                                }
                                this.service.inspect_recorded_trace(id);
                                this.sync(cx);
                            }))
                    })),
            );
        }
        body
    }

    fn connection_row(
        &self,
        connection: ClashConnection,
        closed: bool,
        metrics: Option<ConnectionMetrics>,
        icon: Option<AnyElement>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self
            .selected_connection
            .as_ref()
            .is_some_and(|c| c.id == connection.id && c.start == connection.start);
        let id = connection.id.clone();
        let rule_menu = MenuTarget::ConnectionRules(DiagnosticTarget::from_connection(&connection));
        let inspect = connection.clone();
        let test = connection.clone();
        let test_target = connection_test_target(&connection);
        let process = connection.process_name();
        let row_hover_bg = p.panel_alt.opacity(0.45);
        let inspect_tip = self.tr("连接透视", "Inspect connection");
        let test_tip = self.tr("测试目标", "Test target");
        let host_label = if connection.metadata.host.is_empty()
            && connection.metadata.sniff_host.is_empty()
            && self.hide_ips
        {
            self.tr("已隐藏 IP", "IP hidden").to_string()
        } else {
            connection.host()
        };
        let endpoint = if self.hide_ips || connection.metadata.destination_ip.is_empty() {
            format!(":{}", connection.metadata.destination_port)
        } else {
            format!(
                "{}:{}",
                connection.metadata.destination_ip, connection.metadata.destination_port
            )
        };

        div()
            .id(SharedString::from(format!("conn-{id}")))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(p.bg)
            .border_1()
            .border_color(if selected { p.primary } else { p.border })
            .hover(move |s| s.bg(row_hover_bg))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_wrap()
                    .child(dot(if closed { p.muted_fg } else { p.success }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.fg)
                                    .truncate()
                                    .child(host_label),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .min_w_0()
                                    .children(icon)
                                    .child(
                                        div().text_xs().text_color(p.muted_fg).truncate().child(
                                            format!(
                                                "{} · {} / {} · {endpoint}",
                                                if process.is_empty() {
                                                    self.tr("进程未提供", "Process unavailable")
                                                        .to_string()
                                                } else {
                                                    process
                                                },
                                                connection.metadata.network,
                                                connection.metadata.kind,
                                            ),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_1()
                            .flex_shrink_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .when_some(metrics, |d, m| {
                                d.child(rate_pair(m.up_speed, m.down_speed, p))
                            })
                            .child(div().text_color(p.muted_fg).child(format!(
                                "↓{} ↑{}",
                                format_bytes(connection.download),
                                format_bytes(connection.upload)
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .child(
                                Button::new(SharedString::from(format!("inspect-{id}")))
                                    .compact()
                                    .custom(icon_button_style(p, false, cx))
                                    .icon(IconName::Route)
                                    .tooltip(inspect_tip.clone())
                                    .accessibility_label(inspect_tip)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.inspect_connection(inspect.clone(), window, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("test-{id}")))
                                    .compact()
                                    .custom(icon_button_style(p, false, cx))
                                    .icon(IconName::Radar)
                                    .tooltip(test_tip.clone())
                                    .accessibility_label(test_tip)
                                    .disabled(test_target.is_none())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(target) = &test_target {
                                            this.selected_connection = Some(test.clone());
                                            this.page = Page::Diagnostics;
                                            this.replay_connection(target, window, cx);
                                        }
                                    })),
                            )
                            .when(
                                !closed && self.connections_tab != ConnectionsTab::Recorded,
                                |d| {
                                    d.child(
                                        danger_icon_button(
                                            SharedString::from(format!("close-{id}")),
                                            IconName::Unplug,
                                            self.tr("断开此连接", "Close connection"),
                                            cx,
                                        )
                                        .disabled(
                                            self.snapshot
                                                .busy
                                                .iter()
                                                .any(|b| b == "connection-close"),
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, _| {
                                                this.service.close_connection(&id)
                                            }),
                                        ),
                                    )
                                },
                            )
                            .child(
                                more_button(
                                    SharedString::from(format!(
                                        "connection-rules-{}",
                                        connection.id
                                    )),
                                    self.tr("防火墙规则", "Firewall rules"),
                                    cx,
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    rule_menu.clone(),
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(pill(
                        if closed {
                            self.tr("已结束", "Closed")
                        } else if self.connections_tab == ConnectionsTab::Recorded
                            && !self.snapshot.recording.active
                        {
                            self.tr("录制停止时活动", "Active at stop")
                        } else {
                            self.tr("活动", "Active")
                        },
                        p.muted_fg,
                        p.panel_alt,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_xs()
                            .text_color(p.muted_fg)
                            .truncate()
                            .child(format!(
                                "{} → {}",
                                rule_label(
                                    &connection,
                                    self.tr("core 未提供规则", "Rule unavailable").as_ref()
                                ),
                                self.connection_chain(&connection)
                            )),
                    )
                    .when_some(metrics, |d, m| {
                        d.child(div().text_xs().text_color(p.muted_fg).child(format!(
                            "{} {} s",
                            self.tr("持续", "Duration"),
                            m.duration_secs
                        )))
                    }),
            )
            .context_menu(menus::builder(cx.entity().downgrade(), rule_menu))
    }

    fn live_process_row(
        &self,
        process: &ProcessLiveTraffic,
        icon: Option<AnyElement>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = &process.process_name;
        let filter = name.clone();
        let target = DiagnosticTarget {
            process: name.clone(),
            ..Default::default()
        };
        let label: SharedString = if name.is_empty() {
            self.tr("进程未提供", "Process unavailable")
        } else {
            name.clone().into()
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .flex_wrap()
            .p_3()
            .rounded_lg()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_1()
                    .min_w(px(0.))
                    .children(icon)
                    .child(
                        Button::new(SharedString::from(format!("process-details-{name}")))
                            .compact()
                            .ghost()
                            .label(label)
                            .disabled(name.is_empty())
                            .tooltip(self.tr("查看此进程的连接", "View process connections"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.connections_tab = ConnectionsTab::Live;
                                this.exact_process_filter = Some(filter.clone());
                                let placeholder = this.connection_search_placeholder();
                                this.connection_search.update(cx, |input, cx| {
                                    input.set_placeholder(placeholder, window, cx);
                                    input.set_value(filter.clone(), window, cx);
                                });
                                this.connections_limit = CONNECTIONS_PAGE_STEP;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                "{} {}",
                process.active_connections,
                if process.active_connections == 1 {
                    self.tr("个活动连接", "active connection")
                } else {
                    self.tr("个活动连接", "active connections")
                }
            )))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(rate_pair(process.up_speed, process.down_speed, p))
                    .child(div().text_color(p.muted_fg).child(format!(
                        "↓{} ↑{}",
                        format_bytes(process.down_total),
                        format_bytes(process.up_total)
                    ))),
            )
            .child(
                Button::new(SharedString::from(format!("process-block-{name}")))
                    .compact()
                    .ghost()
                    .label(self.tr("阻止联网", "Block process"))
                    .disabled(
                        name.is_empty()
                            || self
                                .snapshot
                                .busy
                                .iter()
                                .any(|k| k == "firewall-rule" || k == "core"),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_command(
                            menus::Command::BlockConnection(
                                target.clone(),
                                DiagnosticRuleKind::Process,
                            ),
                            false,
                            window,
                            cx,
                        )
                    })),
            )
    }

    fn connection_chain(&self, connection: &ClashConnection) -> String {
        if connection.chains.is_empty() {
            return self
                .tr("core 未提供出站链", "Outbound chain unavailable")
                .to_string();
        }
        connection
            .chains
            .iter()
            .map(|tag| {
                self.snapshot
                    .model
                    .nodes
                    .iter()
                    .find(|n| n.outbound_tag() == *tag)
                    .map(|n| n.name.clone())
                    .or_else(|| {
                        self.snapshot
                            .model
                            .groups
                            .iter()
                            .find(|g| g.tag() == *tag)
                            .map(|g| g.name.clone())
                    })
                    .unwrap_or_else(|| tag.clone())
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }

    fn connection_inspector(
        &self,
        c: &ClashConnection,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let target = connection_test_target(c);
        let probing = self
            .snapshot
            .trace
            .as_ref()
            .is_some_and(|t| matches!(t.phase, TracePhase::Request | TracePhase::Response));
        let p = palette(cx);
        let m = &c.metadata;
        let process = c.process_name();
        let fields = [
            (
                self.tr("01 · 发起方", "01 · Source"),
                format!(
                    "{}\n{}:{}\n{}",
                    if process.is_empty() {
                        self.tr("进程未提供", "Process unavailable").to_string()
                    } else {
                        process
                    },
                    m.source_ip,
                    m.source_port,
                    m.process_path
                ),
            ),
            (
                self.tr("02 · 协议 / 入站", "02 · Protocol / inbound"),
                format!("{} · {}", m.network.to_uppercase(), m.kind),
            ),
            (
                self.tr("03 · DNS 解析", "03 · DNS resolver"),
                self.tr(
                    "内核未暴露此连接的 DNS 解析器",
                    "DNS resolver metadata is not exposed by the core",
                )
                .to_string(),
            ),
            (
                self.tr("04 · 命中规则", "04 · Matched rule"),
                rule_label(c, self.tr("core 未提供规则", "Rule unavailable").as_ref()),
            ),
            (
                self.tr("05 · 出站链", "05 · Outbound chain"),
                self.connection_chain(c),
            ),
            (
                self.tr("06 · 目标 / 返回", "06 · Target / return"),
                format!(
                    "{} → {}:{}\n{}\n↓{} · ↑{}",
                    c.host(),
                    m.destination_ip,
                    m.destination_port,
                    self.tr("经原连接返回", "Returns over the established connection"),
                    format_bytes(c.download),
                    format_bytes(c.upload)
                ),
            ),
        ];
        card(&p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(section_title(
                        &p,
                        self.tr("连接透视", "Connection inspector"),
                        self.tr(
                            "来自 core 的连接元数据；缺失信息会明确标注",
                            "Core connection metadata; unavailable fields are labeled",
                        ),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Button::new("replay-connection")
                                .compact().ghost().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .icon(IconName::Radar)
                                .label(self.tr("重放测试", "Replay test"))
                                .disabled(probing || target.is_none())
                                .tooltip(if target.is_none() { self.tr("缺少目标地址或端口", "Destination address or port unavailable") }
                                    else { self.tr("使用此连接的目标和协议运行链路测试", "Run a link trace to this destination using its protocol") })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(target) = &target { this.replay_connection(target, window, cx); }
                                })))
                            .child(
                                Button::new("back-to-connections")
                                    .compact()
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .ghost()
                                    .icon(Page::Connections.icon())
                                    .label(self.tr("返回连接", "Back to connections"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.page = Page::Connections;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("close-connection-inspector")
                                    .compact()
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .custom(icon_button_style(&p, false, cx))
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected_connection = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .children(fields.into_iter().map(|(label, value)| {
                        div()
                            .flex_1()
                            .min_w(px(220.))
                            .p_3()
                            .rounded_lg()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(div().text_xs().text_color(p.primary).child(label))
                            .child(div().mt_2().text_sm().text_color(p.fg).child(value))
                    })),
            )
            .child(self.diagnostic_rule_bar(DiagnosticTarget::from_connection(c), "connection", cx))
    }

    fn diagnostic_rule_bar(
        &self,
        target: DiagnosticTarget,
        prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let kind = self.diagnostic_rule_kind;
        let value = target.value(kind).to_string();
        let outbound_label = self
            .diagnostic_rule_group
            .as_deref()
            .map(|id| {
                self.snapshot
                    .model
                    .group(id)
                    .map(|group| self.outbound_group_label(group))
                    .unwrap_or_else(|| self.tr("出站组已删除", "Outbound group removed"))
            })
            .unwrap_or_else(|| self.tr("默认出站组", "Default outbound group"));
        let kinds = [
            (DiagnosticRuleKind::Domain, self.tr("域名", "Domain")),
            (DiagnosticRuleKind::Ip, self.tr("IP", "IP")),
            (DiagnosticRuleKind::Process, self.tr("进程", "Process")),
        ];
        let actions = [
            (RouteActionKind::Proxy, self.tr("代理", "Proxy")),
            (RouteActionKind::Direct, self.tr("直连", "Direct")),
            (RouteActionKind::Block, self.tr("拦截", "Block")),
        ];
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(p.primary.opacity(0.06))
            .border_1()
            .border_color(p.primary.opacity(0.25))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.fg)
                    .child(self.tr("快速添加规则", "Quick rule")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_sm()
                            .text_color(p.muted_fg)
                            .child(self.tr("匹配条件：", "Match type:")),
                    )
                    .child(
                        ChoiceGroup::new(
                            (ElementId::from(prefix), "rule-kind"),
                            kinds.iter().map(|(_, label)| label.clone()).collect(),
                            kinds.iter().position(|(k, _)| *k == kind),
                        )
                        .small()
                        .on_change(cx.listener(move |this, index: &usize, _, cx| {
                            this.diagnostic_rule_kind = kinds[*index].0;
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_xs()
                            .text_color(p.muted_fg)
                            .truncate()
                            .child(if value.is_empty() {
                                self.tr("当前匹配条件不可用", "This match is unavailable")
                                    .to_string()
                            } else {
                                value.clone()
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(div().text_sm().text_color(p.muted_fg).child(self.tr("路由规则：", "Routing rule:")))
                    .child(
                        ChoiceGroup::new(
                            (ElementId::from(prefix), "rule-action"),
                            actions.iter().map(|(_, label)| label.clone()).collect(),
                            actions.iter().position(|(a, _)| *a == self.diagnostic_rule_action),
                        )
                        .small()
                        .on_change(cx.listener(move |this, index: &usize, _, cx| {
                            this.diagnostic_rule_action = actions[*index].0;
                            cx.notify();
                        })),
                    )
                    .when(self.diagnostic_rule_action == RouteActionKind::Proxy, |d| {
                        d.child(Button::new(SharedString::from(format!("{prefix}-rule-outbound")))
                            .compact().h(px(super::theme::CONTROL_HEIGHT))
                            .tooltip(outbound_label.clone())
                            .child(prefixed_dropdown_content(self.tr("出站目标", "Outbound"), outbound_label, &p))
                            .dropdown_menu(menus::builder(cx.entity().downgrade(), MenuTarget::DiagnosticOutbound)))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new(SharedString::from(format!("{prefix}-add-rule")))
                            .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .primary()
                            .label(self.tr("置顶添加", "Add at top"))
                            .disabled(value.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let action = RouteAction {
                                    kind: this.diagnostic_rule_action,
                                    group: if this.diagnostic_rule_action == RouteActionKind::Proxy {
                                        this.diagnostic_rule_group.clone()
                                    } else { None },
                                };
                                if let Err(err) = this.service.add_diagnostic_rule(
                                    &target,
                                    this.diagnostic_rule_kind,
                                    action,
                                ) {
                                    this.service.toast(ToastLevel::Error, err.to_string());
                                }
                                this.sync(cx);
                            })),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(if self.snapshot.model.work_mode == WorkMode::Mesh {
                        self.tr("保存至本机规则；组网模式暂停本地代理分流，不会修改远端出口规则", "Saved locally; Mesh mode pauses local proxy routing and does not change the remote exit's rules")
                    } else {
                        self.tr("置顶保存至自定义规则（优先级低于局域网与大陆绕过），应用配置后生效", "Saved at the top of custom rules (below built-in LAN/China bypasses); apply config to take effect")
                    })),
            )
    }

    fn inspect_connection(
        &mut self,
        connection: ClashConnection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_connection = Some(connection);
        self.page = Page::Diagnostics;
        self.scroll_diagnostics_to(|view| &view.inspector_anchor, window, cx);
        cx.notify();
    }

    fn replay_connection(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.trace_input.update(cx, |input, cx| {
            input.set_value(target.to_owned(), window, cx)
        });
        self.run_trace(target, cx);
        self.scroll_diagnostics_to(|view| &view.trace_anchor, window, cx);
    }

    fn scroll_diagnostics_to(
        &self,
        anchor: fn(&Self) -> &ScrollAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The list shortcut first mounts Diagnostics; let the target card lay out before scrolling.
        let view = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            if let Some(view) = view.upgrade() {
                let view = view.read(cx);
                if view.page == Page::Diagnostics {
                    anchor(view).clone().scroll_to(window, cx);
                }
            }
        });
    }

    fn run_trace(&mut self, target: &str, cx: &mut Context<Self>) {
        match self.service.start_trace(target) {
            Ok(()) => {
                self.expanded_trace_step = None;
            }
            Err(err) => self.service.toast(ToastLevel::Error, err.to_string()),
        }
        self.sync(cx);
    }

    fn trace_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let trace = self.snapshot.trace.clone();
        let active = trace
            .as_ref()
            .is_some_and(|t| matches!(t.phase, TracePhase::Request | TracePhase::Response));
        let success = trace.as_ref().and_then(|t| t.success);
        let presets = [
            (TraceProtocol::Https, "https://www.gstatic.com/generate_204"),
            (TraceProtocol::Tcp, "tcp://1.1.1.1:443"),
            (TraceProtocol::Udp, "udp://8.8.8.8:53"),
            (TraceProtocol::Icmp, "icmp://1.1.1.1"),
        ];
        let protocol = parse_target(&self.trace_input.read(cx).value())
            .ok()
            .map(|target| target.protocol);
        let protocol_group = ChoiceGroup::new(
            "trace-protocol",
            presets
                .iter()
                .map(|(proto, _)| proto.label().into())
                .collect(),
            presets
                .iter()
                .position(|(proto, _)| Some(*proto) == protocol),
        )
        .on_change(cx.listener(move |this, index: &usize, window, cx| {
            this.trace_input.update(cx, |input, cx| {
                input.set_value(presets[*index].1, window, cx)
            });
        }));
        card(&p)
            .child(div().flex().items_center().justify_between().gap_3().flex_wrap()
                .child(section_title(&p, self.tr("链路测试", "Link trace"), self.tr("发起探测请求，依次检查 DNS 解析、路由匹配与出站连通性", "Send a probe request to inspect DNS resolution, route matching, and outbound connectivity")))
                .child(pill(if active { self.tr("● 探测中", "● Probing") } else if success == Some(true) { self.tr("✓ 请求完成", "✓ Completed") } else if success == Some(false) { self.tr("! 发现问题", "! Issue found") } else { self.tr("就绪", "Ready") }, if success == Some(false) { p.danger } else { p.primary }, p.primary.opacity(0.1))))
            .child(div().flex().items_center().gap_2().flex_wrap()
                .child(protocol_group)
                .child(div().flex_1().min_w(px(200.)).child(Input::new(&self.trace_input)))
                .child(Button::new("trace-start").compact().h(px(crate::ui::theme::CONTROL_HEIGHT)).primary().label(if trace.is_some() { self.tr("重新测试", "Run again") } else { self.tr("开始测试", "Start test") }).loading(active)
                    .on_click(cx.listener(|this, _, _, cx| { let target = this.trace_input.read(cx).value().to_string(); this.run_trace(&target, cx); })))
                .child(Button::new("trace-clear").compact().h(px(crate::ui::theme::CONTROL_HEIGHT)).ghost().label(self.tr("清除", "Clear"))
                    .on_click(cx.listener(|this, _, _, cx| { this.service.clear_trace(); this.expanded_trace_step = None; this.sync(cx); }))))
            .child(div().text_xs().text_color(p.muted_fg).child(if self.snapshot.model.work_mode == WorkMode::Mesh {
                self.tr("通过组网链路测试；本地代理分流已暂停，远端出口的分流结果需在出口设备查看。ICMP 暂不支持。", "Test through the mesh; local proxy routing is paused. Inspect remote routing on the exit device. ICMP is unavailable.")
            } else { self.tr("点击阶段查看完整详情", "Select a stage for full details") }))
            .child(self.trace_graph(trace.as_ref(), cx))
            .when_some(trace.as_ref(), |d, trace| {
                let failed = trace.steps.iter().find(|s| s.status == TraceStatus::Error);
                let focused = self.expanded_trace_step.and_then(|kind| trace.step(kind)).or(failed);
                d.when_some(focused, |d, step| d.child(div().flex().flex_col().gap_2().p_3().rounded_lg().bg(p.bg).border_l_2().border_color(status_color(step.status, &p))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(status_color(step.status, &p)).child(self.trace_step_label(step.kind)))
                    .child(div().text_sm().text_color(p.fg).child(step.detail.clone()))))
                    .when_some(trace.error.as_ref().filter(|error| focused.is_none_or(|step| step.detail != **error)), |d, error| d.child(div().text_sm().text_color(p.danger).child(error.clone())))
                    .child(div().text_xs().text_color(if trace.success == Some(false) { p.danger } else { p.muted_fg }).child(trace.summary.clone().unwrap_or_default()))
                    .when_some(trace.blocked_rule_id.as_ref(), |d, id| {
                        let enabled = self.snapshot.model.routing.effective_rules().any(|rule| &rule.id == id);
                        let rule_id = id.clone();
                        let group = self.snapshot.model.routing.rule_groups.iter()
                            .find(|g| g.rules.iter().any(|r| r.id == *id)).map(|g| g.id.clone());
                        d.child(div().flex().gap_2().flex_wrap()
                            .child(Button::new("trace-open-hit").compact().ghost()
                                .label(self.tr("查看拦截规则","View blocking rule"))
                                .on_click(cx.listener(move |this,_,_,cx|{
                                    this.rule_show_all = false;
                                    this.rule_group_id = group.clone();
                                    this.page = Page::Routing;
                                    this.sync(cx);
                                })))
                            .child(Button::new("trace-disable-hit").compact().disabled(!enabled)
                                .label(self.tr("停用此规则","Disable this rule"))
                                .on_click(cx.listener(move |this,_,_,cx|{
                                    this.service.set_rule_enabled(&rule_id,false);
                                    this.sync(cx);
                                }))))
                    })
                    .child(self.diagnostic_rule_bar(DiagnosticTarget::from_trace(trace), "trace", cx))
            })
    }

    fn trace_graph(
        &self,
        trace: Option<&TraceSession>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let kinds = [
            TraceStepKind::Source,
            TraceStepKind::Inbound,
            TraceStepKind::Dns,
            TraceStepKind::Route,
            TraceStepKind::Outbound,
            TraceStepKind::Remote,
        ];
        let colors: Vec<_> = kinds
            .iter()
            .map(|kind| {
                status_color(
                    trace
                        .and_then(|t| t.step(*kind))
                        .map_or(TraceStatus::Pending, |s| s.status),
                    &p,
                )
            })
            .collect();
        let active =
            trace.is_some_and(|t| matches!(t.phase, TracePhase::Request | TracePhase::Response));
        let phase = trace.map(|t| t.phase);
        let animation_phase = std::rc::Rc::new(std::cell::Cell::new(0.0f32));
        let paint_phase = animation_phase.clone();
        let response = trace.and_then(|t| t.step(TraceStepKind::Response));
        let response_color = response.map_or(p.muted_fg, |s| status_color(s.status, &p));
        let detail = response
            .map(|s| s.detail.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                self.tr(
                    "返回路径将在请求完成后显示",
                    "The return path appears after the request completes",
                )
                .to_string()
            });
        let graph = canvas(
            move |_, _, _| (),
            move |bounds, _, window, _| {
                let anim = paint_phase.get();
                let gap = 10.;
                let padding = super::theme::TRACE_CANVAS_PAD;
                let width = (f32::from(bounds.size.width) - padding * 2.).max(0.);
                let cell = (width + gap) / 6.;
                let node_w = cell - gap;
                let at = |x: f32, y: f32| {
                    point(bounds.origin.x + px(padding + x), bounds.origin.y + px(y))
                };
                for index in 0..6 {
                    let x = index as f32 * cell + node_w / 2.;
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(at(x, 12.));
                    line.line_to(at(x, 222.));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, p.border.opacity(0.3));
                    }
                }
                for (index, color) in colors.iter().take(5).enumerate() {
                    let x1 = index as f32 * cell + node_w;
                    let x2 = (index + 1) as f32 * cell;
                    for (stroke, opacity) in [(7., 0.06), (3., 0.15), (1.5, 0.8)] {
                        let mut line = PathBuilder::stroke(px(stroke));
                        line.move_to(at(x1, 104.));
                        line.line_to(at(x2, 104.));
                        if let Ok(path) = line.build() {
                            window.paint_path(path, color.opacity(opacity));
                        }
                    }
                    if active && phase == Some(TracePhase::Request) {
                        let x = x1 + (x2 - x1) * ((anim + index as f32 * 0.17) % 1.);
                        window.paint_quad(
                            fill(Bounds::new(at(x - 3., 101.), size(px(6.), px(6.))), p.info)
                                .corner_radii(Corners::all(px(3.))),
                        );
                    }
                }
                let start = node_w / 2.;
                let end = 5. * cell + node_w / 2.;
                let mut path = PathBuilder::stroke(px(1.5)).dash_array(&[px(5.), px(5.)]);
                path.move_to(at(end, 184.));
                path.cubic_bezier_to(at(start, 184.), at(end, 244.), at(start, 244.));
                if let Ok(path) = path.build() {
                    window.paint_path(path, response_color.opacity(0.8));
                }
                if active && phase == Some(TracePhase::Response) {
                    let t = anim;
                    let inv = 1. - t;
                    let x = inv.powi(3) * end
                        + 3. * inv.powi(2) * t * end
                        + 3. * inv * t.powi(2) * start
                        + t.powi(3) * start;
                    let y = inv.powi(3) * 184.
                        + 3. * inv.powi(2) * t * 244.
                        + 3. * inv * t.powi(2) * 244.
                        + t.powi(3) * 184.;
                    window.paint_quad(
                        fill(
                            Bounds::new(at(x - 4., y - 4.), size(px(8.), px(8.))),
                            response_color,
                        )
                        .corner_radii(Corners::all(px(4.))),
                    );
                }
            },
        )
        .absolute()
        .inset_0()
        .w_full()
        .h_full();
        let graph = if active {
            graph
                .with_animation(
                    "trace-flow",
                    Animation::new(std::time::Duration::from_millis(1600))
                        .repeat()
                        .with_max_fps(30.),
                    move |canvas, phase| {
                        animation_phase.set(phase);
                        canvas
                    },
                )
                .into_any_element()
        } else {
            graph.into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(246.))
                    .rounded_lg()
                    .bg(p.info.opacity(0.045))
                    .child(graph)
                    .child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .left(px(super::theme::TRACE_CANVAS_PAD))
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr("请求发出 →", "REQUEST SENT →")),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(36.))
                            .left(px(super::theme::TRACE_CANVAS_PAD))
                            .right(px(super::theme::TRACE_CANVAS_PAD))
                            .flex()
                            .gap(px(10.))
                            .children(kinds.into_iter().enumerate().map(|(index, kind)| {
                                let step = trace.and_then(|t| t.step(kind));
                                let status = step.map_or(TraceStatus::Pending, |s| s.status);
                                let color = status_color(status, &p);
                                let detail = step
                                    .map(|s| s.detail.clone())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| {
                                        self.tr("等待测试", "Awaiting test").to_string()
                                    });
                                div()
                                    .id(SharedString::from(format!("trace-stage-{index}")))
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h(px(148.))
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .p_2p5()
                                    .rounded_lg()
                                    .bg(if status == TraceStatus::Active {
                                        p.accent
                                    } else {
                                        p.bg
                                    })
                                    .border_1()
                                    .border_color(color.opacity(0.7))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.expanded_trace_step = Some(kind);
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_1()
                                            .child(div().text_xs().text_color(color).child(
                                                format!(
                                                    "{:02}  {}",
                                                    index + 1,
                                                    match status {
                                                        TraceStatus::Ok => "✓",
                                                        TraceStatus::Active => "●",
                                                        TraceStatus::Error | TraceStatus::Warn => {
                                                            "!"
                                                        }
                                                        TraceStatus::Skipped => "–",
                                                        TraceStatus::Pending => "○",
                                                    }
                                                ),
                                            ))
                                            .when_some(
                                                step.and_then(|s| s.duration_ms),
                                                |d, ms| {
                                                    d.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(p.muted_fg)
                                                            .child(format!("{ms} ms")),
                                                    )
                                                },
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.fg)
                                            .truncate()
                                            .child(self.trace_step_label(kind)),
                                    )
                                    .child(
                                        div()
                                            .min_w(px(0.))
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .line_clamp(3)
                                            .child(detail),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .absolute()
                            .bottom(px(4.))
                            .left(px(super::theme::TRACE_CANVAS_PAD))
                            .text_xs()
                            .text_color(response_color)
                            .child(self.tr("← 响应返回", "← RESPONSE RECEIVED")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(response_color.opacity(0.06))
                    .child(dot(response_color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(detail),
                    ),
            )
    }
}

fn rate_pair(up: u64, down: u64, p: &Palette) -> Div {
    div()
        .flex()
        .gap_2()
        .child(
            div()
                .text_color(if down > 0 { p.data_down } else { p.muted_fg })
                .child(format!("↓{}/s", format_bytes(down))),
        )
        .child(
            div()
                .text_color(if up > 0 { p.data_up } else { p.muted_fg })
                .child(format!("↑{}/s", format_bytes(up))),
        )
}

fn status_color(status: TraceStatus, p: &Palette) -> Hsla {
    match status {
        TraceStatus::Ok => p.ok_text,
        TraceStatus::Active => p.primary,
        TraceStatus::Warn => p.warn_text,
        TraceStatus::Error => p.danger_text,
        TraceStatus::Skipped => p.muted_fg,
        TraceStatus::Pending => p.muted_fg,
    }
}

fn rule_label(c: &ClashConnection, fallback: &str) -> String {
    if c.rule.is_empty() {
        fallback.to_string()
    } else if c.rule_payload.is_empty() {
        c.rule.clone()
    } else {
        format!("{} · {}", c.rule, c.rule_payload)
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionsTab;

    #[test]
    fn default_connections_tab_is_live() {
        assert!(ConnectionsTab::default() == ConnectionsTab::Live);
    }
}
