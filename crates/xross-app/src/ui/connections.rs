use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::CoreKind;
use xross_core::util::{format_bytes, format_speed};

use super::theme::palette;
use super::widgets::*;
use super::AppView;

impl AppView {
    pub(crate) fn page_connections(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let settings = self.snapshot.settings.clone();
        let runtime = self.snapshot.runtime.clone();
        let running = self.snapshot.process.phase.is_alive();
        let supported = settings.core == CoreKind::SingBox;

        let mut body = div().flex().flex_col().gap_4().w_full();

        body = body.child(self.trace_card(cx));

        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("连接", "Connections"),
                            self.tr(
                                "来自 Clash API（/connections）。Xray 的 StatsService 只能给流量，不能列连接。",
                                "Sourced from the Clash API (/connections). Xray's StatsService only exposes traffic counters.",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("refresh-connections")
                                        .compact()
                                        .label(self.tr("刷新", "Refresh"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.refresh_connections();
                                        })),
                                )
                                .child(
                                    Button::new("kill-all")
                                        .compact()
                                        .danger()
                                        .label(self.tr("全部断开", "Close all"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.kill_all_connections();
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.fg)
                                        .child(format!(
                                            "↓ {}   ↑ {}",
                                            format_speed(runtime.down_speed),
                                            format_speed(runtime.up_speed)
                                        )),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(format!(
                                            "{}：{}  ·  {}：{}",
                                            self.tr("累计下行", "down total"),
                                            format_bytes(runtime.down_total),
                                            self.tr("累计上行", "up total"),
                                            format_bytes(runtime.up_total)
                                        )),
                                ),
                        )
                        .child(pill(
                            format!("{} {}", runtime.connections.len(), self.tr("活动连接", "active")),
                            p.muted_fg,
                            p.panel_alt,
                        )),
                )
                .child(self.sparkline(&p)),
        );

        if !running {
            body = body.child(card(&p).child(empty_hint(
                &p,
                self.tr(
                    "core 未运行，启动后可查看连接",
                    "Start the core to see connections",
                ),
            )));
        } else if !supported {
            body = body.child(
                card(&p).child(section_title(
                    &p,
                    self.tr("Xray 不提供连接列表", "Xray has no connection list"),
                    self.tr(
                        "切换到 sing-box 后可查看按进程/域名的连接并一键断开；Xray 仅提供流量统计。",
                        "Switch to sing-box for per-process/domain connections and one-click close; Xray only exposes counters.",
                    ),
                )),
            );
        }

        if running && supported {
            let mut rows = div().flex().flex_col().gap_1().w_full();
            if runtime.connections.is_empty() {
                rows = rows.child(empty_hint(
                    &p,
                    self.tr("当前没有活动连接", "No active connections"),
                ));
            }
            for connection in runtime.connections.iter().take(300) {
                let id = connection.id.clone();
                let host = connection.host();
                let process = connection.process_name();
                let network = format!(
                    "{}/{}",
                    connection.metadata.network, connection.metadata.kind
                );
                let destination = format!(
                    "{}:{}",
                    connection.metadata.destination_ip, connection.metadata.destination_port
                );
                let chain = connection
                    .chains
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "—".into());
                let rule = if connection.rule.is_empty() {
                    "—".to_string()
                } else if connection.rule_payload.is_empty() {
                    connection.rule.clone()
                } else {
                    format!("{} ({})", connection.rule, connection.rule_payload)
                };
                rows =
                    rows.child(
                        div()
                            .id(SharedString::from(format!("conn-{id}")))
                            .flex()
                            .items_center()
                            .gap_3()
                            .p_2()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .flex_1()
                                    .child(div().text_sm().text_color(p.fg).child(host))
                                    .child(
                                        div().text_xs().text_color(p.muted_fg).child(format!(
                                            "{process} · {network} · {destination}"
                                        )),
                                    ),
                            )
                            .child(pill(chain, p.muted_fg, p.panel_alt))
                            .child(
                                div()
                                    .w(px(150.))
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(rule),
                            )
                            .child(div().w(px(120.)).text_xs().text_color(p.muted_fg).child(
                                format!(
                                    "↓{} ↑{}",
                                    format_bytes(connection.download),
                                    format_bytes(connection.upload)
                                ),
                            ))
                            .child(
                                Button::new(SharedString::from(format!("kill-{id}")))
                                    .compact()
                                    .ghost()
                                    .label("✕")
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.service.kill_connection(&id);
                                    })),
                            ),
                    );
            }
            body = body.child(card(&p).child(rows));
        }

        let _ = lang;
        body
    }
    fn trace_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let trace = self.service.trace_snapshot();
        let active = trace
            .as_ref()
            .map(|t| {
                !matches!(
                    t.phase,
                    xross_core::trace::TracePhase::Done | xross_core::trace::TracePhase::Failed
                )
            })
            .unwrap_or(false);
        let summary = trace.as_ref().and_then(|t| t.summary.clone());
        let success = trace.as_ref().and_then(|t| t.success).unwrap_or(false);
        let target_label = trace
            .as_ref()
            .map(|t| format!("{} · {}", t.protocol.label(), t.target))
            .unwrap_or_default();

        let examples = [
            ("HTTPS", "https://www.gstatic.com/generate_204"),
            ("TCP", "tcp://1.1.1.1:443"),
            ("UDP", "udp://8.8.8.8:53"),
            ("ICMP", "icmp://1.1.1.1"),
        ];

        card(&p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_title(
                        &p,
                        self.tr("测试链接", "Test a link"),
                        self.tr(
                            "真实发起请求，动态展示 请求源 → 入站 → DNS → 路由 → 节点 → 目标 → 响应 链路",
                            "Runs a real request and animates source → inbound → DNS → route → node → target → response",
                        ),
                    ))
                    .when(trace.is_some(), |d| {
                        d.child(pill(
                            target_label.clone(),
                            p.muted_fg,
                            p.panel_alt,
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.trace_input)))
                    .child(
                        Button::new("trace-start")
                            .compact()
                            .primary()
                            .label(if trace.is_some() {
                                self.tr("重新测试", "Run again")
                            } else {
                                self.tr("开始测试", "Start test")
                            })
                            .loading(active)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                let target = this.trace_input.read(cx).value().to_string();
                                match this.service.start_trace(&target) {
                                    Ok(()) => {
                                        this.trace_anim = 0.0;
                                    }
                                    Err(err) => this.service.toast(
                                        xross_core::service::ToastLevel::Error,
                                        err.to_string(),
                                    ),
                                }
                                this.sync(cx);
                            })),
                    )
                    .child(
                        Button::new("trace-clear")
                            .compact()
                            .ghost()
                            .label(self.tr("清除", "Clear"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.service.clear_trace();
                                this.sync(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr("示例：", "Examples:")),
                    )
                    .children(examples.into_iter().map(|(label, value)| {
                        let value = value.to_string();
                        div()
                            .id(SharedString::from(format!("trace-example-{label}")))
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(p.panel_alt)
                            .text_xs()
                            .text_color(p.muted_fg)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let value = value.clone();
                                this.trace_input.update(cx, |input, cx| {
                                    input.set_value(value, window, cx);
                                });
                                cx.notify();
                            }))
                            .child(label)
                    })),
            )
            .child(self.trace_graph(trace.as_ref(), cx))
            .when(trace.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(if success { p.success } else { p.warning })
                                .child(summary.unwrap_or_default()),
                        )
                        .when(trace.as_ref().and_then(|t| t.request_ms).is_some(), |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(format!(
                                        "{} {} ms · {} {} ms",
                                        self.tr("请求", "request"),
                                        trace.as_ref().and_then(|t| t.request_ms).unwrap_or(0),
                                        self.tr("响应", "response"),
                                        trace.as_ref().and_then(|t| t.response_ms).unwrap_or(0)
                                    )),
                            )
                        }),
                )
            })
    }

    fn trace_graph(
        &self,
        trace: Option<&xross_core::trace::TraceSession>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use xross_core::trace::{TracePhase, TraceStatus, TraceStepKind};

        let p = palette(cx);
        let kinds = [
            TraceStepKind::Source,
            TraceStepKind::Inbound,
            TraceStepKind::Dns,
            TraceStepKind::Route,
            TraceStepKind::Outbound,
            TraceStepKind::Remote,
        ];
        let centers = [62.0f32, 214.0, 366.0, 518.0, 670.0, 822.0];
        let node_w = 122.0f32;
        let node_h = 92.0f32;
        let node_top = 36.0f32;
        let req_y = 82.0f32;
        let resp_y = 238.0f32;
        let width = 900.0f32;
        let height = 300.0f32;

        let mut nodes = Vec::new();
        let mut statuses = Vec::new();
        for (index, kind) in kinds.iter().enumerate() {
            let step = trace.and_then(|t| t.step(*kind));
            let status = step.map(|s| s.status).unwrap_or(TraceStatus::Pending);
            statuses.push(status);
            let color = match status {
                TraceStatus::Ok => p.success,
                TraceStatus::Active => p.primary,
                TraceStatus::Warn => p.warning,
                TraceStatus::Error => p.danger,
                TraceStatus::Skipped => p.muted_fg,
                TraceStatus::Pending => p.border,
            };
            let detail = step
                .map(|s| truncate_trace(&s.detail, 26))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.tr("等待测试…", "waiting…").to_string());
            let duration = step
                .and_then(|s| s.duration_ms)
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_default();
            nodes.push(
                div()
                    .absolute()
                    .left(px(centers[index] - node_w / 2.0))
                    .top(px(node_top))
                    .w(px(node_w))
                    .h(px(node_h))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .rounded_md()
                    .bg(if status == TraceStatus::Active {
                        p.accent
                    } else {
                        p.bg
                    })
                    .border_1()
                    .border_color(color)
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1()
                            .child(
                                div().flex().items_center().gap_1().child(dot(color)).child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.fg)
                                        .child(self.trace_step_label(*kind)),
                                ),
                            )
                            .when(!duration.is_empty(), |d| {
                                d.child(div().text_xs().text_color(color).child(duration))
                            }),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(detail)),
            );
        }

        let line_colors: Vec<Hsla> = statuses
            .iter()
            .map(|status| match status {
                TraceStatus::Ok => p.success,
                TraceStatus::Active => p.primary,
                TraceStatus::Warn => p.warning,
                TraceStatus::Error => p.danger,
                _ => p.border,
            })
            .collect();
        let anim = self.trace_anim;
        let phase = trace.map(|t| t.phase).unwrap_or(TracePhase::Request);
        let has_trace = trace.is_some();
        let response_status = trace
            .and_then(|t| t.step(TraceStepKind::Response))
            .map(|s| s.status)
            .unwrap_or(TraceStatus::Pending);
        let response_color = match response_status {
            TraceStatus::Ok => p.success,
            TraceStatus::Active => p.primary,
            TraceStatus::Warn => p.warning,
            TraceStatus::Error => p.danger,
            TraceStatus::Skipped => p.muted_fg,
            TraceStatus::Pending => p.border,
        };
        let response_detail = trace
            .and_then(|t| t.step(TraceStepKind::Response))
            .map(|s| truncate_trace(&s.detail, 96))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                self.tr(
                    "响应会沿节点原路返回 Xross",
                    "Response travels back through the node",
                )
                .to_string()
            });

        let graph = canvas(
            move |_bounds, _window, _cx| (),
            move |bounds, _, window, _cx| {
                let origin = bounds.origin;
                let at = |x: f32, y: f32| point(origin.x + px(x), origin.y + px(y));
                for index in 0..5 {
                    let mut builder = PathBuilder::stroke(px(1.5));
                    let x1 = centers[index] + node_w / 2.0;
                    let x2 = centers[index + 1] - node_w / 2.0;
                    builder.move_to(at(x1, req_y));
                    builder.curve_to(at(x2, req_y), at((x1 + x2) / 2.0, req_y - 16.0));
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, line_colors[index]);
                    }
                }
                let mut response = PathBuilder::stroke(px(1.5)).dash_array(&[px(6.0), px(5.0)]);
                response.move_to(at(centers[5], node_top + node_h));
                response.cubic_bezier_to(
                    at(centers[0], resp_y),
                    at(centers[5] - 120.0, resp_y + 26.0),
                    at(centers[0] + 80.0, resp_y),
                );
                if let Ok(path) = response.build() {
                    window.paint_path(path, response_color);
                }
                if has_trace {
                    let request_x = if matches!(phase, TracePhase::Request) {
                        centers[0] + (centers[5] - centers[0]) * anim
                    } else {
                        centers[5]
                    };
                    let request =
                        Bounds::new(at(request_x - 4.0, req_y - 4.0), size(px(8.0), px(8.0)));
                    window.paint_quad(fill(request, p.primary).corner_radii(Corners::all(px(4.0))));
                    if matches!(
                        phase,
                        TracePhase::Response | TracePhase::Done | TracePhase::Failed
                    ) {
                        let response_x = if matches!(phase, TracePhase::Done | TracePhase::Failed) {
                            centers[0]
                        } else {
                            centers[5] - (centers[5] - centers[0]) * anim
                        };
                        let dot =
                            Bounds::new(at(response_x - 4.0, resp_y - 4.0), size(px(8.0), px(8.0)));
                        window.paint_quad(
                            fill(dot, response_color).corner_radii(Corners::all(px(4.0))),
                        );
                    }
                }
            },
        )
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(width))
        .h(px(height));

        div().w_full().overflow_x_scrollbar().child(
            div()
                .w(px(width))
                .h(px(height))
                .relative()
                .child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top(px(8.0))
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr("请求路径", "Request path")),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top(px(200.0))
                        .w(px(width - 32.0))
                        .text_xs()
                        .text_color(response_color)
                        .child(response_detail),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top(px(220.0))
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr("响应路径", "Response path")),
                )
                .child(graph)
                .children(nodes),
        )
    }
}

fn truncate_trace(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max {
        value.to_string()
    } else {
        let mut out: String = chars.into_iter().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
