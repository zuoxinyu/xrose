//! Live connections and interactive request diagnostics.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::diagnostics::{
    ConnectionFilterKind, DiagnosticRuleKind, DiagnosticTarget, connection_test_target,
};
use xross_core::model::{CoreKind, RouteAction, RouteActionKind};
use xross_core::runtime::ClashConnection;
use xross_core::service::ToastLevel;
use xross_core::trace::{TracePhase, TraceSession, TraceStatus, TraceStepKind};
use xross_core::util::format_bytes;

use super::menus::{self, MenuTarget};
use super::theme::{Palette, palette};
use super::widgets::*;
use super::{AppView, Page};
use gpui_kit::component::menu::DropdownMenu;

const CONNECTIONS_PAGE_STEP: usize = 50;

impl AppView {
    pub(crate) fn page_connections(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let supported = self.snapshot.settings.core == CoreKind::SingBox;
        let running = self.snapshot.process.phase.is_alive();
        let query = self.connection_search.read(cx).value().to_string();
        let filter = self.connection_filter;
        let limit = self.connections_limit.max(CONNECTIONS_PAGE_STEP);

        // Filter by reference first and only clone the visible slice to keep
        // 400 ms refreshes fast even with 2,000 recorded connections.
        let (total, count, visible): (usize, usize, Vec<(ClashConnection, bool)>) =
            if self.show_recorded_connections {
                let all = &self.snapshot.recording.connections;
                let total = all.len();
                let mut count = 0usize;
                let mut visible = Vec::with_capacity(limit.min(total));
                for entry in all.iter().rev() {
                    if filter.matches(&entry.connection, &query) {
                        if count < limit {
                            visible.push((entry.connection.clone(), entry.closed_at.is_some()));
                        }
                        count += 1;
                    }
                }
                (total, count, visible)
            } else {
                let all = &self.snapshot.runtime.connections;
                let total = all.len();
                let mut count = 0usize;
                let mut visible = Vec::with_capacity(limit.min(total));
                for conn in all.iter() {
                    if filter.matches(conn, &query) {
                        if count < limit {
                            visible.push((conn.clone(), false));
                        }
                        count += 1;
                    }
                }
                (total, count, visible)
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
                        .on_change(cx.listener(|this, index, _, cx| {
                            this.show_recorded_connections = *index == 1;
                            this.connections_limit = CONNECTIONS_PAGE_STEP;
                            cx.notify();
                        })),
                    )
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
                    .child(
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
                            this.connection_filter = [
                                ConnectionFilterKind::All,
                                ConnectionFilterKind::Domain,
                                ConnectionFilterKind::Ip,
                                ConnectionFilterKind::Process,
                            ][*index];
                            this.connections_limit = CONNECTIONS_PAGE_STEP;
                            cx.notify();
                        })),
                    )
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
        for (connection, closed) in visible {
            rows = rows.child(self.connection_row(connection, closed, &p, cx));
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
            .child(header)
            .child(card(&p).child(rows))
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
            .child(self.trace_card(cx));

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
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self
            .selected_connection
            .as_ref()
            .is_some_and(|c| c.id == connection.id && c.start == connection.start);
        let id = connection.id.clone();
        let inspect = connection.clone();
        let test = connection.clone();
        let can_test = connection_test_target(&connection).is_some();
        let process = connection.process_name();
        let hover_group = SharedString::from(format!("conn-group-{id}"));
        let row_hover_bg = p.panel_alt.opacity(0.45);
        let btn_hover_bg = p.panel_alt;
        let muted = p.muted_fg;
        let fg = p.fg;
        let primary = p.primary;
        let inspect_tip = self.tr("链路 / 规则", "Path / rule").to_string();
        let test_tip = self.tr("测试目标", "Test target").to_string();

        div()
            .id(SharedString::from(format!("conn-{id}")))
            .group(hover_group.clone())
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
                            .child(div().text_xs().text_color(p.muted_fg).truncate().child(
                                format!(
                                    "{} · {} / {} · {}:{}",
                                    if process.is_empty() {
                                        self.tr("进程未提供", "Process unavailable").to_string()
                                    } else {
                                        process
                                    },
                                    connection.metadata.network,
                                    connection.metadata.kind,
                                    connection.metadata.destination_ip,
                                    connection.metadata.destination_port
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(format!(
                                "↓{} ↑{}",
                                format_bytes(connection.download),
                                format_bytes(connection.upload)
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .opacity(if selected { 1. } else { 0. })
                                    .group_hover(hover_group.clone(), |s| s.opacity(1.))
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("inspect-{id}")))
                                            .w(px(26.))
                                            .h(px(26.))
                                            .rounded_md()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .text_color(if selected {
                                                primary
                                            } else {
                                                muted.opacity(0.75)
                                            })
                                            .hover(move |s| s.bg(btn_hover_bg).text_color(fg))
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(inspect_tip.clone()).build(window, cx)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.selected_connection = Some(inspect.clone());
                                                this.page = Page::Diagnostics;
                                                cx.notify();
                                            }))
                                            .child(Icon::new(IconName::Route).with_size(px(14.))),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("test-{id}")))
                                            .w(px(26.))
                                            .h(px(26.))
                                            .rounded_md()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(can_test, |d| {
                                                d.cursor_pointer()
                                                    .text_color(muted.opacity(0.75))
                                                    .hover(move |s| {
                                                        s.bg(btn_hover_bg).text_color(fg)
                                                    })
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            if let Some(target) =
                                                                connection_test_target(&test)
                                                            {
                                                                this.trace_input.update(
                                                                    cx,
                                                                    |input, cx| {
                                                                        input.set_value(
                                                                            target.clone(),
                                                                            window,
                                                                            cx,
                                                                        )
                                                                    },
                                                                );
                                                                this.selected_connection =
                                                                    Some(test.clone());
                                                                this.page = Page::Diagnostics;
                                                                this.run_trace(&target, cx);
                                                            }
                                                        },
                                                    ))
                                            })
                                            .when(!can_test, |d| d.text_color(muted.opacity(0.3)))
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(test_tip.clone()).build(window, cx)
                                            })
                                            .child(Icon::new(IconName::Radar).with_size(px(14.))),
                                    ),
                            )
                            .when(!closed && !self.show_recorded_connections, |d| {
                                let danger = p.danger;
                                let tip = self.tr("断开此连接", "Close connection").to_string();
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("kill-{id}")))
                                        .w(px(26.))
                                        .h(px(26.))
                                        .rounded_md()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(danger.opacity(0.65))
                                        .hover(move |s| {
                                            s.bg(danger.opacity(0.14)).text_color(danger)
                                        })
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(tip.clone()).build(window, cx)
                                        })
                                        .on_click(cx.listener(move |this, _, _, _| {
                                            this.service.kill_connection(&id)
                                        }))
                                        .child(Icon::new(IconName::Unplug).with_size(px(14.))),
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
                    ),
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
