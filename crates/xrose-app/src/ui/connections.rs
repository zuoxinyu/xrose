//! Live connections and interactive request diagnostics.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::diagnostics::{
    ConnectionFilterKind, DiagnosticRuleKind, DiagnosticTarget, connection_test_target,
};
use xrose_core::flow::{ConnectionMetrics, ConnectionSort, ProcessLiveTraffic};
use xrose_core::model::{CoreKind, RouteAction, RouteActionKind, WorkMode};
use xrose_core::runtime::ClashConnection;
use xrose_core::service::ToastLevel;
use xrose_core::trace::{TracePhase, TraceSession, TraceStatus, TraceStepKind};
use xrose_core::util::format_bytes;

use super::menus::{self, MenuTarget};
use super::theme::{Palette, palette};
use super::widgets::*;
use super::{AppView, Page};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};

const CONNECTIONS_PAGE_STEP: usize = 50;

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
        if self.show_live_processes && !self.show_recorded_connections {
            self.tr("过滤进程名…", "Filter process name…")
        } else {
            self.tr("过滤域名、IP 或进程…", "Filter domain, IP or process…")
        }
    }

    pub(crate) fn page_connections(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let supported = self
            .snapshot
            .process
            .core
            .unwrap_or(self.snapshot.settings.core)
            == CoreKind::SingBox;
        let running = self.snapshot.process.phase.is_alive();
        let query = self.connection_search.read(cx).value().trim().to_string();
        let filter = self.connection_filter;
        let limit = self.connections_limit.max(CONNECTIONS_PAGE_STEP);

        let processes = !self.show_recorded_connections && self.show_live_processes;
        let live = &self.snapshot.runtime.live_traffic;
        let matches = |c: &ClashConnection| {
            if query.is_empty() {
                true
            } else if filter == ConnectionFilterKind::Process
                && self.exact_process_filter.as_ref() == Some(&query)
            {
                c.process_name() == query
            } else {
                filter.matches(c, &query)
            }
        };
        let (total, rows): (usize, Box<dyn Iterator<Item = MonitorRow<'_>> + '_>) = if self
            .show_recorded_connections
        {
            let all = &self.snapshot.recording.connections;
            (
                all.len(),
                Box::new(
                    all.iter()
                        .rev()
                        .filter(|r| matches(&r.connection))
                        .map(|r| {
                            MonitorRow::Connection(&r.connection, r.closed_at.is_some(), None)
                        }),
                ),
            )
        } else if processes {
            let text = query.trim().to_lowercase();
            (
                live.processes.len(),
                Box::new(
                    live.processes
                        .iter()
                        .filter(move |p| {
                            text.is_empty() || p.process_name.to_lowercase().contains(&text)
                        })
                        .map(MonitorRow::Process),
                ),
            )
        } else {
            let all = &self.snapshot.runtime.connections;
            (
                all.len(),
                Box::new(
                    live.ordered(self.connection_sort)
                        .iter()
                        .filter_map(|&index| {
                            let c = all.get(index)?;
                            matches(c).then(|| {
                                MonitorRow::Connection(c, false, live.metrics.get(index).copied())
                            })
                        }),
                ),
            )
        };
        let (visible, count) = {
            let mut rows = rows;
            let visible: Vec<_> = rows.by_ref().take(limit).collect();
            let count = if query.is_empty() {
                total
            } else {
                visible.len() + rows.count()
            };
            (visible, count)
        };
        let recording_active = self.snapshot.recording.active;
        let recording_started = self.snapshot.recording.started_at.is_some();
        let recorded_conn_len = self.snapshot.recording.connections.len();
        let recorded_trace_len = self.snapshot.recording.traces.len();
        let evicted_conns = self.snapshot.recording.evicted_connections;
        let evicted_traces = self.snapshot.recording.evicted_traces;
        let export_busy = self
            .snapshot
            .busy
            .iter()
            .any(|key| key == "recording-export");
        let active_conn_len = self.snapshot.runtime.connections.len();

        let view_choice = ChoiceGroup::new(
            "live-connection-view",
            vec![
                self.tr("连接明细", "Connection details"),
                self.tr("活跃进程", "Active processes"),
            ],
            usize::from(processes),
        )
        .disabled(self.show_recorded_connections)
        .on_change(cx.listener(|this, index, window, cx| {
            this.show_live_processes = *index == 1;
            this.connection_filter = ConnectionFilterKind::All;
            this.exact_process_filter = None;
            let placeholder = this.connection_search_placeholder();
            this.connection_search.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
                input.set_value("", window, cx);
            });
            this.connections_limit = CONNECTIONS_PAGE_STEP;
            cx.notify();
        }));
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
        let limited_capture = running
            && self.snapshot.model.work_mode == WorkMode::Direct
            && !self.snapshot.settings.tun_enabled;

        let header = card(&p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(section_title(
                        &p,
                        self.tr("连接", "Connections"),
                        self.tr(
                            "查看连接详情，录制后可查看已结束的连接",
                            "Inspect connections and record them for later review",
                        ),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("record-connections")
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
                                more_button(
                                    "connection-actions",
                                    self.tr("连接操作", "Connection actions"),
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::Connections,
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        ChoiceGroup::new(
                            "connection-view",
                            vec![
                                self.tr("实时", "Live"),
                                format!("{} {recorded_conn_len}", self.tr("录制", "Recorded"))
                                    .into(),
                            ],
                            usize::from(self.show_recorded_connections),
                        )
                        .on_change(cx.listener(|this, index, window, cx| {
                            this.show_recorded_connections = *index == 1;
                            let placeholder = this.connection_search_placeholder();
                            this.connection_search.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
                            this.connections_limit = CONNECTIONS_PAGE_STEP;
                            cx.notify();
                        })),
                    )
                    .child(view_choice)
                    .child(pill(
                        format!("{active_conn_len} {}", self.tr("活动连接", "active")),
                        p.primary,
                        p.accent,
                    ))
                    .child(div().text_xs().text_color(p.muted_fg).child(format!(
                        "{} · {recorded_trace_len} {} · {count} / {total}",
                        if recording_active {
                            self.tr("录制中", "Recording")
                        } else {
                            self.tr("录制已停止", "Recording off")
                        },
                        self.tr("次测试", "tests")
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .when(!processes, |d| d.child(
                        ChoiceGroup::new(
                            "connection-filter",
                            vec![
                                self.tr("全部", "All"),
                                self.tr("域名", "Domain"),
                                self.tr("IP", "IP"),
                                self.tr("进程", "Process"),
                            ],
                            [
                                ConnectionFilterKind::All,
                                ConnectionFilterKind::Domain,
                                ConnectionFilterKind::Ip,
                                ConnectionFilterKind::Process,
                            ]
                            .iter()
                            .position(|kind| *kind == filter)
                            .unwrap_or(0),
                        )
                        .on_change(cx.listener(|this, index, _, cx| {
                            this.exact_process_filter = None;
                            this.connection_filter = [
                                ConnectionFilterKind::All,
                                ConnectionFilterKind::Domain,
                                ConnectionFilterKind::Ip,
                                ConnectionFilterKind::Process,
                            ][*index];
                            this.connections_limit = CONNECTIONS_PAGE_STEP;
                            cx.notify();
                        })),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(super::theme::FORM_FIELD_WIDTH))
                            .child(Input::new(&self.connection_search)),
                    )
                    .child(
                        Button::new("clear-connection-filter")
                            .ghost()
                            .label(self.tr("重置", "Reset"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connection_search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.connection_filter = ConnectionFilterKind::All;
                                this.connections_limit = CONNECTIONS_PAGE_STEP;
                                cx.notify();
                            })),
                    ),
            )
            .when(!self.show_recorded_connections && !processes && supported, |d| {
                d.child(div().flex().items_center().gap_2()
                    .child(div().text_xs().text_color(p.muted_fg).child(self.tr("排序", "Sort by")))
                    .child(sort_choice))
            })
            .when(limited_capture, |d| {
                let hint = self.tr(
                    "当前仅捕获系统代理流量；开启 TUN 模式可监控本机全部进程连接",
                    "Only local proxy traffic is visible. Enable TUN to monitor connections from all processes.",
                );
                let enable = Button::new("connections-enable-tun")
                    .compact().ghost().label(self.tr("开启 TUN", "Enable TUN"))
                    .disabled(self.snapshot.busy.iter().any(|b| b == "core"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.service.set_tun_enabled(true);
                        this.sync(cx);
                    }));
                d.child(div().flex().items_center().justify_between().gap_2().flex_wrap()
                    .child(div().text_xs().text_color(p.muted_fg).child(hint))
                    .child(enable))
            })
            .when(recording_started, |d| {
                d.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                    "仅保留本次会话的连接与测试信息，不采集正文。",
                    "Stores connection and test metadata for this session, without message bodies.",
                )))
            })
            .when(evicted_conns + evicted_traces > 0, |d| {
                d.child(div().text_xs().text_color(p.warn_text).child(self.tr(
                    "容量已满，较早的记录已移除。",
                    "Capacity reached; older records have been removed.",
                )))
            });

        let shown = visible.len();
        let mut rows = div().flex().flex_col().gap_2().w_full();
        if !self.show_recorded_connections && !supported {
            rows = rows.child(
                empty_state(
                    &p,
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
        } else if !self.show_recorded_connections && !running {
            rows = rows.child(
                empty_state(
                    &p,
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
            let filtered = !query.trim().is_empty() || filter != ConnectionFilterKind::All;
            rows = rows.child(
                empty_state(
                    &p,
                    IconName::ArrowLeftRight,
                    if filtered {
                        self.tr("没有符合条件的连接", "No matching connections")
                    } else {
                        self.tr("暂无连接记录", "No connections yet")
                    },
                    if filtered {
                        self.tr("尝试清除筛选条件", "Try clearing the filters")
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
                                this.connection_search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                                this.connection_filter = ConnectionFilterKind::All;
                                cx.notify();
                            })),
                    )
                }),
            )
        }
        for row in visible {
            let icon = self
                .process_icons
                .as_ref()
                .map(|icons| icons.view(row.icon_key(&self.snapshot.runtime.connections)));
            rows = match row {
                MonitorRow::Connection(connection, closed, metrics) => rows
                    .child(self.connection_row(connection.clone(), closed, metrics, icon, &p, cx)),
                MonitorRow::Process(process) => {
                    rows.child(self.live_process_row(process, icon, &p, cx))
                }
            };
        }
        if count > shown || limit > CONNECTIONS_PAGE_STEP {
            rows = rows.child(
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
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .flex_shrink_0()
            .child(header.flex_shrink_0())
            .child(self.traffic_statistics_card(&p, cx))
            .child(card(&p).flex_shrink_0().child(rows))
    }

    pub(crate) fn page_diagnostics(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        if let Some(selected) = self
            .selected_connection
            .as_mut()
            .filter(|_| !self.show_recorded_connections)
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
            .child(inspector)
            .child(self.trace_card(cx))
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
                                    .child(connection.host()),
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
                                                "{} · {} / {} · {}:{}",
                                                if process.is_empty() {
                                                    self.tr("进程未提供", "Process unavailable")
                                                        .to_string()
                                                } else {
                                                    process
                                                },
                                                connection.metadata.network,
                                                connection.metadata.kind,
                                                connection.metadata.destination_ip,
                                                connection.metadata.destination_port
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
                                    .ghost()
                                    .icon(IconName::Route)
                                    .tooltip(inspect_tip.clone())
                                    .accessibility_label(inspect_tip)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected_connection = Some(inspect.clone());
                                        this.page = Page::Diagnostics;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("test-{id}")))
                                    .compact()
                                    .ghost()
                                    .icon(IconName::Radar)
                                    .tooltip(test_tip.clone())
                                    .accessibility_label(test_tip)
                                    .disabled(test_target.is_none())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(target) = &test_target {
                                            this.trace_input.update(cx, |input, cx| {
                                                input.set_value(target.clone(), window, cx)
                                            });
                                            this.selected_connection = Some(test.clone());
                                            this.page = Page::Diagnostics;
                                            this.run_trace(target, cx);
                                        }
                                    })),
                            )
                            .child(
                                more_button(
                                    SharedString::from(format!("connection-rules-{id}")),
                                    self.tr("防火墙规则", "Firewall rules"),
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    rule_menu.clone(),
                                )),
                            )
                            .when(!closed && !self.show_recorded_connections, |d| {
                                d.child(
                                    danger_icon_button(
                                        SharedString::from(format!("close-{id}")),
                                        IconName::Unplug,
                                        self.tr("断开此连接", "Close connection"),
                                        cx,
                                    )
                                    .disabled(
                                        self.snapshot.busy.iter().any(|b| b == "connection-close"),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, _| this.service.close_connection(&id),
                                    )),
                                )
                            }),
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
                        } else if self.show_recorded_connections && !self.snapshot.recording.active
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
                                this.show_live_processes = false;
                                this.connection_filter = ConnectionFilterKind::Process;
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
                                    .ghost()
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
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(self.tr("快速添加规则", "Quick rule")),
                    )
                    .children(
                        [
                            (DiagnosticRuleKind::Domain, self.tr("域名", "Domain")),
                            (DiagnosticRuleKind::Ip, self.tr("IP", "IP")),
                            (DiagnosticRuleKind::Process, self.tr("进程", "Process")),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, (k, label))| {
                            chip(format!("{prefix}-rule-kind-{i}"), label, kind == k, &p).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.diagnostic_rule_kind = k;
                                    cx.notify();
                                }),
                            )
                        }),
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
                    .children(
                        [
                            (RouteActionKind::Proxy, self.tr("代理", "Proxy")),
                            (RouteActionKind::Direct, self.tr("直连", "Direct")),
                            (RouteActionKind::Block, self.tr("拦截", "Block")),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, (action, label))| {
                            chip(
                                format!("{prefix}-rule-action-{i}"),
                                label,
                                self.diagnostic_rule_action == action,
                                &p,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.diagnostic_rule_action = action;
                                    cx.notify();
                                },
                            ))
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{prefix}-add-rule")))
                            .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .primary()
                            .label(self.tr("置顶添加", "Add at top"))
                            .disabled(value.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let action = RouteAction {
                                    kind: this.diagnostic_rule_action,
                                    group: None,
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
                    .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                        "置顶保存至自定义规则（优先级低于局域网与大陆绕过），应用配置后生效",
                        "Saved at the top of custom rules (below built-in LAN/China bypasses); apply config to take effect",
                    ))),
            )
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
        card(&p)
            .child(div().flex().items_center().justify_between().gap_3().flex_wrap()
                .child(section_title(&p, self.tr("链路测试", "Link trace"), self.tr("发起探测请求，依次检查 DNS 解析、路由匹配与出站连通性", "Send a probe request to inspect DNS resolution, route matching, and outbound connectivity")))
                .child(pill(if active { self.tr("● 探测中", "● Probing") } else if success == Some(true) { self.tr("✓ 请求完成", "✓ Completed") } else if success == Some(false) { self.tr("! 发现问题", "! Issue found") } else { self.tr("就绪", "Ready") }, if success == Some(false) { p.danger } else { p.primary }, p.primary.opacity(0.1))))
            .child(div().flex().items_center().gap_2().flex_wrap()
                .child(div().flex_1().min_w(px(200.)).child(Input::new(&self.trace_input)))
                .child(Button::new("trace-start").compact().h(px(crate::ui::theme::CONTROL_HEIGHT)).primary().label(if trace.is_some() { self.tr("重新测试", "Run again") } else { self.tr("开始测试", "Start test") }).loading(active)
                    .on_click(cx.listener(|this, _, _, cx| { let target = this.trace_input.read(cx).value().to_string(); this.run_trace(&target, cx); })))
                .child(Button::new("trace-clear").compact().h(px(crate::ui::theme::CONTROL_HEIGHT)).ghost().label(self.tr("清除", "Clear"))
                    .on_click(cx.listener(|this, _, _, cx| { this.service.clear_trace(); this.expanded_trace_step = None; this.sync(cx); }))))
            .child(div().flex().items_center().gap_2().flex_wrap()
                .children([("HTTPS", "https://www.gstatic.com/generate_204"), ("TCP", "tcp://1.1.1.1:443"), ("UDP", "udp://8.8.8.8:53"), ("ICMP", "icmp://1.1.1.1")].into_iter().map(|(label, value)| chip(format!("trace-example-{label}"), label, false, &p)
                    .on_click(cx.listener(move |this, _, window, cx| { this.trace_input.update(cx, |input, cx| input.set_value(value, window, cx)); cx.notify(); }))))
                .child(div().text_xs().text_color(p.muted_fg).child(self.tr("点击阶段查看完整详情", "Select a stage for full details"))))
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
                for index in 0..5 {
                    let x1 = index as f32 * cell + node_w;
                    let x2 = (index + 1) as f32 * cell;
                    for (stroke, opacity) in [(7., 0.06), (3., 0.15), (1.5, 0.8)] {
                        let mut line = PathBuilder::stroke(px(stroke));
                        line.move_to(at(x1, 104.));
                        line.line_to(at(x2, 104.));
                        if let Ok(path) = line.build() {
                            window.paint_path(path, colors[index].opacity(opacity));
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

fn chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    p: &Palette,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .px_2()
        .py_1()
        .rounded_full()
        .flex_shrink_0()
        .text_xs()
        .cursor_pointer()
        .bg(if selected {
            p.primary.opacity(0.15)
        } else {
            p.panel_alt
        })
        .text_color(if selected { p.primary } else { p.muted_fg })
        .child(label.into())
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
