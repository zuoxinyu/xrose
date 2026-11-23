//! Inline expandable node detail panel: connection metadata (IP, protocol,
//! transport/TLS, location), 5-dimension health breakdown, service reachability,
//! and quick probe actions.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{Node, Transport};
use xrose_core::runtime::health::{
    DimBreakdown, HealthDim, NodeHealthView, ReachabilityCheckStatus, ReachabilityReport,
    ServiceCheckResult,
};
use xrose_core::runtime::location::NodeLocation;
use xrose_core::service::HttpTestTarget;
use xrose_core::settings::Language;
use xrose_core::util::{format_bytes, format_speed};

use super::{VirtualExit, format_endpoint};
use crate::ui::theme::{
    HEALTH_DETAIL_LABEL_WIDTH, HEALTH_HOURLY_BAR_HEIGHT, Palette, SEMANTIC_ALPHA, STATS_BAR_HEIGHT,
    health_grade_colors, latency_color_ms,
};
use crate::ui::widgets::pill;
use crate::ui::{AppView, tr};

pub(super) struct DetailPanelProps<'a> {
    pub node: &'a Node,
    pub row_key: &'a str,
    pub target: HttpTestTarget,
    pub virtual_exit: Option<&'a VirtualExit>,
    pub chain_subtitle: Option<&'a str>,
    pub location: Option<&'a NodeLocation>,
    pub health: Option<&'a NodeHealthView>,
    pub selected: bool,
    pub hide_ips: bool,
    pub offline_exit: bool,
    pub mesh_provider: Option<xrose_core::mesh::MeshProvider>,
    pub source_summary: SharedString,
    pub group_summary: SharedString,
    pub direct_takeover_summary: SharedString,
    pub probe_busy: (bool, bool, bool),
    pub any_probe_busy: bool,
    pub probe_disabled: bool,
    pub probe_budget_used_bytes: u64,
    pub daily_probe_budget_mb: u32,
    pub lang: Language,
}

struct InfoItem {
    label: SharedString,
    value: SharedString,
    mono: bool,
}

fn format_protocol_summary(node: &Node, lang: Language) -> String {
    let mut parts = vec![
        node.protocol.display_name().to_uppercase(),
        node.transport_label().to_uppercase(),
    ];
    if node.tls.reality.is_some() {
        parts.push("Reality".into());
    } else if node.tls.enabled {
        parts.push("TLS".into());
    } else if !matches!(
        node.protocol,
        xrose_core::model::Protocol::Wireguard
            | xrose_core::model::Protocol::Hysteria2
            | xrose_core::model::Protocol::Tuic
            | xrose_core::model::Protocol::Anytls
            | xrose_core::model::Protocol::Openconnect
    ) {
        parts.push(tr(lang, "无 TLS", "No TLS").to_string());
    }
    parts.join(" · ")
}

fn format_transport_extra(node: &Node, hide_ips: bool, lang: Language) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(flow) = node
        .credentials
        .flow
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        parts.push(flow.trim().to_string());
    }
    if let Some(method) = node
        .credentials
        .method
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        parts.push(method.trim().to_string());
    }
    if let Some(fp) = node
        .tls
        .fingerprint
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        parts.push(format!("uTLS {}", fp.trim()));
    }
    if !node.tls.alpn.is_empty() {
        parts.push(format!("ALPN {}", node.tls.alpn.join(",")));
    }
    if node.tls.insecure {
        parts.push(tr(lang, "跳过证书校验", "Skip TLS verify").to_string());
    }
    match &node.transport {
        Transport::WebSocket { path, host, .. }
        | Transport::Http2 { path, host }
        | Transport::HttpUpgrade { path, host } => {
            if !path.is_empty() && path != "/" {
                parts.push(format!("path {}", if hide_ips { "•••" } else { path }));
            }
            if let Some(h) = host.as_deref().filter(|s| !s.trim().is_empty()) {
                parts.push(format!("host {}", if hide_ips { "•••" } else { h.trim() }));
            }
        }
        Transport::Grpc { service_name } if !service_name.trim().is_empty() => {
            parts.push(format!(
                "service {}",
                if hide_ips {
                    "•••"
                } else {
                    service_name.trim()
                }
            ));
        }
        _ => {}
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn format_sni(node: &Node, hide_ips: bool) -> Option<String> {
    let sni = node
        .tls
        .server_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(if hide_ips {
        xrose_core::privacy::mask_magic_dns_suffix(sni)
    } else {
        sni.to_string()
    })
}

fn format_location_and_egress(
    location: Option<&NodeLocation>,
    egress_region: Option<&str>,
    hide_ips: bool,
    lang: Language,
) -> String {
    if hide_ips {
        return tr(lang, "隐私模式已隐藏", "Hidden in privacy mode").to_string();
    }
    let mut parts = Vec::new();
    if let Some(loc) = location {
        if loc.pending {
            parts.push(tr(lang, "探测中…", "Locating…").to_string());
        } else if let Some(label) = loc.label.as_deref().filter(|s| !s.is_empty()) {
            parts.push(label.to_string());
        }
    }
    if let Some(region) = egress_region.filter(|s| !s.is_empty()) {
        parts.push(format!("{} {region}", tr(lang, "落地", "Exit")));
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(" · ")
    }
}

fn build_info_items(props: &DetailPanelProps<'_>) -> Vec<InfoItem> {
    let lang = props.lang;
    let egress_region = props
        .health
        .and_then(|h| h.reachability.as_ref())
        .and_then(|r| r.egress_region.as_deref());

    if let Some(VirtualExit::Direct { .. }) = props.virtual_exit {
        let egress = if props.hide_ips {
            tr(lang, "隐私模式已隐藏", "Hidden in privacy mode").to_string()
        } else if let Some(region) = egress_region {
            format!("{} {region}", tr(lang, "落地", "Exit"))
        } else {
            tr(lang, "本机网络", "Local network").to_string()
        };
        return vec![
            InfoItem {
                label: tr(lang, "出口模式", "Exit mode"),
                value: tr(lang, "本机直连 (Direct)", "Direct (Local)"),
                mono: false,
            },
            InfoItem {
                label: tr(lang, "入站与接管", "Inbound & takeover"),
                value: props.direct_takeover_summary.clone(),
                mono: true,
            },
            InfoItem {
                label: tr(lang, "落地地区", "Egress region"),
                value: egress.into(),
                mono: false,
            },
        ];
    }

    if let Some(VirtualExit::Mesh {
        detail,
        online,
        provider,
        ..
    }) = props.virtual_exit
    {
        let status = if *online {
            tr(lang, "在线", "Online")
        } else {
            tr(lang, "离线", "Offline")
        };
        let egress = if props.hide_ips {
            tr(lang, "隐私模式已隐藏", "Hidden in privacy mode").to_string()
        } else {
            egress_region
                .map(|r| format!("{} {r}", tr(lang, "落地", "Exit")))
                .unwrap_or_else(|| "—".into())
        };
        return vec![
            InfoItem {
                label: tr(lang, "组网地址 / IP", "Mesh address / IP"),
                value: detail.clone().into(),
                mono: true,
            },
            InfoItem {
                label: tr(lang, "组网协议", "Mesh protocol"),
                value: format!(
                    "{} · {}",
                    tr(lang, "组网出口", "Mesh exit"),
                    provider.label()
                )
                .into(),
                mono: false,
            },
            InfoItem {
                label: tr(lang, "设备状态 / 落地", "Device & egress"),
                value: format!("{status} · {egress}").into(),
                mono: false,
            },
        ];
    }

    if props.node.is_chain() {
        let hop_count = props.node.chain_hops.len();
        let hops_text = if lang == Language::Zh {
            format!("代理链 · {hop_count} 跳")
        } else {
            format!("Proxy chain · {hop_count} hops")
        };
        return vec![
            InfoItem {
                label: tr(lang, "转发路径", "Forwarding path"),
                value: props.chain_subtitle.unwrap_or("—").to_string().into(),
                mono: true,
            },
            InfoItem {
                label: tr(lang, "协议与跳数", "Protocol & hops"),
                value: hops_text.into(),
                mono: false,
            },
            InfoItem {
                label: tr(lang, "归属地 / 落地", "Location / egress"),
                value: format_location_and_egress(
                    props.location,
                    egress_region,
                    props.hide_ips,
                    lang,
                )
                .into(),
                mono: false,
            },
            InfoItem {
                label: tr(lang, "所属出站组", "Outbound groups"),
                value: props.group_summary.clone(),
                mono: false,
            },
        ];
    }

    let endpoint = format_endpoint(&props.node.endpoint(), props.hide_ips);
    let protocol_summary = if let Some(provider) = props.mesh_provider {
        format!(
            "{} · {} ({})",
            tr(lang, "组网分流网关", "Mesh gateway"),
            provider.label(),
            props.node.protocol.display_name().to_uppercase()
        )
    } else {
        format_protocol_summary(props.node, lang)
    };
    let mut items = vec![
        InfoItem {
            label: tr(lang, "服务器地址 / IP", "Server address / IP"),
            value: if endpoint.is_empty() {
                "—".into()
            } else {
                endpoint.into()
            },
            mono: true,
        },
        InfoItem {
            label: tr(lang, "协议与传输", "Protocol & transport"),
            value: protocol_summary.into(),
            mono: true,
        },
        InfoItem {
            label: tr(lang, "归属地 / 落地", "Location / egress"),
            value: format_location_and_egress(props.location, egress_region, props.hide_ips, lang)
                .into(),
            mono: false,
        },
    ];

    if let Some(sni) = format_sni(props.node, props.hide_ips) {
        items.push(InfoItem {
            label: "SNI".into(),
            value: sni.into(),
            mono: true,
        });
    }
    if let Some(extra) = format_transport_extra(props.node, props.hide_ips, lang) {
        items.push(InfoItem {
            label: tr(lang, "加密与参数", "TLS & parameters"),
            value: extra.into(),
            mono: true,
        });
    }
    items.push(InfoItem {
        label: tr(lang, "UDP / 出站组", "UDP / Outbound groups"),
        value: format!(
            "{} · {}",
            if props.node.udp {
                tr(lang, "UDP 开启", "UDP on")
            } else {
                tr(lang, "UDP 关闭", "UDP off")
            },
            props.group_summary
        )
        .into(),
        mono: false,
    });
    items
}

pub(super) fn render_node_detail_panel(
    props: DetailPanelProps<'_>,
    p: &Palette,
    cx: &mut Context<AppView>,
) -> Stateful<Div> {
    let lang = props.lang;
    let mono = cx.theme().mono_font_family.clone();
    let nested_bg = p.detail_surface(props.selected);
    let info_items = build_info_items(&props);
    let (http_busy, download_busy, reach_busy) = props.probe_busy;
    let row_key = props.row_key.to_string();

    let budget_text: SharedString = if props.daily_probe_budget_mb == 0 {
        format!(
            "{}：{} / {}",
            tr(lang, "今日探测流量", "Daily probe usage"),
            format_bytes(props.probe_budget_used_bytes),
            tr(lang, "不限", "Unlimited")
        )
        .into()
    } else {
        format!(
            "{}：{} / {} MiB",
            tr(lang, "今日探测流量", "Daily probe usage"),
            format_bytes(props.probe_budget_used_bytes),
            props.daily_probe_budget_mb
        )
        .into()
    };

    let info_grid = div().grid().grid_cols(3).gap_x_4().gap_y_2p5().children(
        info_items.into_iter().enumerate().map(|(idx, item)| {
            let tip = item.value.clone();
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w(px(0.))
                .child(div().text_xs().text_color(p.muted_fg).child(item.label))
                .child(
                    div()
                        .id(SharedString::from(format!("node-info-{row_key}-{idx}")))
                        .min_w(px(0.))
                        .truncate()
                        .text_xs()
                        .when(item.mono, |d| d.font_family(mono.clone()))
                        .text_color(p.fg)
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .child(item.value),
                )
        }),
    );

    let mut detail = div()
        .id(SharedString::from(format!("node-detail-{row_key}")))
        .on_click(|_, _, cx| cx.stop_propagation())
        .border_t_1()
        .border_color(p.hairline_border())
        .px_4()
        .pt_3()
        .pb_4()
        .flex()
        .flex_col()
        .gap_3()
        // Section 1: IP, protocol, address & connection metadata
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_fg)
                                .child(tr(lang, "节点信息", "Node information")),
                        )
                        .when(!props.source_summary.is_empty(), |d| {
                            d.child(pill(props.source_summary.clone(), p.muted_fg, p.panel_alt))
                        }),
                )
                .child(info_grid),
        )
        // Section 2: 5-dimension health details
        .child(health_section(
            props.health,
            props.probe_busy,
            nested_bg,
            lang,
            p,
            cx,
        ))
        // Section 3: Service reachability
        .child(reachability_section(
            props.health.and_then(|h| h.reachability.as_ref()),
            props.hide_ips,
            nested_bg,
            lang,
            p,
        ));

    // Section 4: Footer with probe budget & quick actions
    let target_http = props.target.clone();
    let target_dl = props.target.clone();
    let target_reach = props.target.clone();
    let disabled = props.any_probe_busy || props.probe_disabled || props.offline_exit;
    let reach_btn_style = ButtonCustomVariant::new(cx)
        .color(p.accent)
        .foreground(p.primary)
        .hover(p.primary.opacity(SEMANTIC_ALPHA + 0.06))
        .active(p.primary.opacity(SEMANTIC_ALPHA + 0.12))
        .shadow(false);

    detail = detail.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_wrap()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_family(mono)
                    .text_color(p.muted_fg)
                    .child(budget_text),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("detail-http-{row_key}")))
                            .compact()
                            .h(px(28.))
                            .icon(IconName::Gauge)
                            .label(tr(lang, "HTTP 测速", "HTTP test"))
                            .loading(http_busy)
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.service.test_http_target(target_http.clone());
                                this.sync(cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("detail-dl-{row_key}")))
                            .compact()
                            .h(px(28.))
                            .icon(IconName::ArrowDown)
                            .label(tr(lang, "吞吐测速", "Throughput"))
                            .loading(download_busy)
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.service.test_download_target(target_dl.clone());
                                this.sync(cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("detail-reach-{row_key}")))
                            .compact()
                            .h(px(28.))
                            .custom(reach_btn_style)
                            .icon(IconName::HeartPulse)
                            .label(tr(lang, "检测可达性", "Check reachability"))
                            .loading(reach_busy)
                            .disabled(disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.service
                                    .test_reachability_target(target_reach.clone(), true);
                                this.sync(cx);
                            })),
                    ),
            ),
    );

    detail
}

fn health_section(
    view: Option<&NodeHealthView>,
    probe_busy: (bool, bool, bool),
    surface_bg: Hsla,
    lang: Language,
    p: &Palette,
    cx: &App,
) -> Div {
    let (http_busy, download_busy, reach_busy) = probe_busy;
    let eval = view.and_then(|v| v.general.as_ref());
    let mono = cx.theme().mono_font_family.clone();

    let summary_text: SharedString = match eval {
        Some(ev) => {
            let mut parts = vec![format!(
                "{} {} · 15m {} / 24h {} · {} {}%",
                ev.grade.as_str(),
                ev.score.round() as u32,
                ev.short_score.round() as u32,
                ev.long_score.round() as u32,
                tr(lang, "置信度", "Conf"),
                (ev.confidence * 100.0).round() as u32
            )];
            if let Some(ai) = view.and_then(|v| v.ai_score) {
                parts.push(format!("AI {}", ai.round() as u32));
            }
            if let Some(media) = view.and_then(|v| v.media_score) {
                parts.push(format!(
                    "{} {}",
                    tr(lang, "流媒体", "Media"),
                    media.round() as u32
                ));
            }
            parts.join(" · ").into()
        }
        None => tr(lang, "尚未完成评分", "Not yet scored"),
    };

    let mut box_div = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(surface_bg)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.muted_fg)
                        .child(tr(lang, "五维健康详情", "Five-dimension health")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when_some(eval, |d, ev| {
                            let (fg, bg) = health_grade_colors(p, ev.grade);
                            let grade_name = match lang {
                                Language::Zh => ev.grade.label_zh(),
                                Language::En => ev.grade.label_en(),
                            };
                            d.child(pill(grade_name, fg, bg))
                        })
                        .child(
                            div()
                                .text_xs()
                                .font_family(mono.clone())
                                .text_color(p.muted_fg)
                                .child(summary_text),
                        ),
                ),
        );

    let Some(view) = view else {
        return box_div.child(if http_busy || download_busy || reach_busy {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(p.muted_fg)
                .child(Spinner::new().small().color(p.muted_fg))
                .child(tr(
                    lang,
                    "正在执行检测，请稍候…",
                    "Running probe, please wait…",
                ))
        } else {
            div().text_xs().text_color(p.muted_fg).child(tr(
                lang,
                "暂无健康样本。点击下方按钮可执行 HTTP 测速、吞吐测速或可达性检测。",
                "No health samples yet. Use the actions below to run probes.",
            ))
        });
    };

    let Some(eval) = &view.general else {
        return box_div.child(div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "暂无综合评分样本。",
            "No health evaluation samples yet.",
        )));
    };

    if let Some(gate) = eval.gate {
        let label = match lang {
            Language::Zh => gate.label_zh(),
            Language::En => gate.label_en(),
        };
        box_div = box_div.child(
            div()
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .bg(p.danger.opacity(SEMANTIC_ALPHA))
                .text_xs()
                .text_color(p.danger_text)
                .child(format!(
                    "{}：{label}（{} {}）",
                    tr(lang, "触发硬门槛", "Hard gate active"),
                    tr(lang, "综合分封顶", "Score capped at"),
                    gate.cap() as u32
                )),
        );
    }

    for dim in &eval.dims {
        let label = match dim.dim {
            HealthDim::Availability => tr(lang, "可用性 A", "Avail A"),
            HealthDim::Latency => tr(lang, "延迟 L", "Latency L"),
            HealthDim::Stability => tr(lang, "稳定性 S", "Stability S"),
            HealthDim::Throughput => tr(lang, "吞吐 T", "Speed T"),
            HealthDim::Reachability => tr(lang, "可达性 R", "Reach R"),
        };
        let fill = dim
            .score
            .map(|s| (s / 100.0).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let bar_color = match dim.score {
            Some(s) if s >= 85.0 => p.success,
            Some(s) if s >= 70.0 => p.primary,
            Some(s) if s >= 50.0 => p.warning,
            Some(_) => p.danger,
            None => p.muted_fg.opacity(0.3),
        };
        let score_str = dim
            .score
            .map(|s| format!("{:>3}", s.round() as u32))
            .unwrap_or_else(|| " —".into());
        let dim_busy = match dim.dim {
            HealthDim::Latency => http_busy,
            HealthDim::Throughput => download_busy,
            HealthDim::Reachability => reach_busy,
            HealthDim::Availability | HealthDim::Stability => false,
        };
        let detail = dim_metric_summary(dim, lang);
        box_div = box_div.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(HEALTH_DETAIL_LABEL_WIDTH))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(p.fg)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(STATS_BAR_HEIGHT))
                        .rounded_full()
                        .bg(p.panel_alt)
                        .overflow_hidden()
                        .when(fill > 0.0, |track| {
                            track.child(div().h_full().w(relative(fill.max(0.03))).bg(bar_color))
                        }),
                )
                .child(
                    div()
                        .w(px(32.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_right()
                        .font_family(mono.clone())
                        .text_color(p.fg)
                        .child(score_str),
                )
                .child(
                    div()
                        .w(px(184.))
                        .flex_shrink_0()
                        .truncate()
                        .text_xs()
                        .font_family(mono.clone())
                        .text_color(p.muted_fg)
                        .child(if dim_busy {
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(Spinner::new().small().color(p.muted_fg))
                                .child(if dim.dim == HealthDim::Reachability {
                                    tr(lang, "检测中…", "Checking…")
                                } else {
                                    tr(lang, "测速中…", "Testing…")
                                })
                                .into_any_element()
                        } else {
                            div().child(detail).into_any_element()
                        }),
                ),
        );
    }

    if !eval.deductions.is_empty() || view.peak_degradation.is_some() {
        let mut notes = div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_1()
            .border_t_1()
            .border_color(p.hairline_border());
        for d in &eval.deductions {
            let msg = match lang {
                Language::Zh => &d.zh,
                Language::En => &d.en,
            };
            notes = notes.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .child(div().text_color(p.muted_fg).child(msg.clone()))
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_color(p.warn_text)
                            .child(format!("-{:.1}", d.points_lost)),
                    ),
            );
        }
        if let Some(peak) = &view.peak_degradation {
            notes = notes.child(div().text_xs().text_color(p.warn_text).child(match lang {
                Language::Zh => format!(
                    "高峰劣化：{:02}:00–{:02}:00 平均 P50 {} ms（较日均 +{}%）",
                    peak.start_hour, peak.end_hour, peak.peak_p50_ms, peak.increase_pct
                ),
                Language::En => format!(
                    "Peak slowdown {:02}:00–{:02}:00: avg P50 {} ms (+{}% vs baseline)",
                    peak.start_hour, peak.end_hour, peak.peak_p50_ms, peak.increase_pct
                ),
            }));
        }
        box_div = box_div.child(notes);
    }

    if view.hourly_profile.iter().any(|s| s.avg_p50_ms.is_some()) {
        box_div = box_div.child(hourly_strip(&view.hourly_profile, lang, p, cx));
    }

    box_div
}

fn dim_metric_summary(dim: &DimBreakdown, lang: Language) -> SharedString {
    match dim.dim {
        HealthDim::Availability => dim
            .success_rate
            .map(|sr| {
                format!(
                    "{:.0}% ({} / {})",
                    sr * 100.0,
                    dim.active_samples,
                    dim.passive_samples
                )
            })
            .unwrap_or_else(|| tr(lang, "无样本", "No data").to_string()),
        HealthDim::Latency => match (dim.p50_ms, dim.p90_ms) {
            (Some(p50), Some(p90)) => format!("P50 {p50} ms · P90 {p90} ms"),
            _ => tr(lang, "未测速", "Untested").to_string(),
        },
        HealthDim::Stability => {
            let mut parts = Vec::new();
            if let Some(j) = dim.jitter_ms {
                parts.push(format!("±{j} ms"));
            }
            if let Some(l) = dim.loss_rate {
                parts.push(format!("loss {:.1}%", l * 100.0));
            }
            if let Some(d) = dim.drop_rate {
                parts.push(format!("drop {:.1}%", d * 100.0));
            }
            if parts.is_empty() {
                tr(lang, "样本不足", "Pending").to_string()
            } else {
                parts.join(" · ")
            }
        }
        HealthDim::Throughput => match dim.throughput_bps {
            Some(bps) => {
                let kind = if dim.passive_throughput {
                    tr(lang, "真实流", "Passive")
                } else {
                    tr(lang, "测速", "Probe")
                };
                format!("{} ({kind})", format_speed(bps))
            }
            None => tr(lang, "未测速", "Untested").to_string(),
        },
        HealthDim::Reachability => {
            if dim.score.is_some() {
                tr(lang, "已检测", "Checked").to_string()
            } else {
                tr(lang, "未检测", "Untested").to_string()
            }
        }
    }
    .into()
}

fn reachability_section(
    report: Option<&ReachabilityReport>,
    hide_region: bool,
    surface_bg: Hsla,
    lang: Language,
    p: &Palette,
) -> Div {
    let mut header = div().flex().items_center().justify_between().child(
        div()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.muted_fg)
            .child(tr(
                lang,
                "服务可达性（AI / 流媒体 / 出口环境）",
                "Service reachability (AI / Streaming / Environment)",
            )),
    );
    if let Some(region) = report
        .and_then(|r| r.egress_region.as_deref())
        .filter(|_| !hide_region)
    {
        header = header.child(pill(
            SharedString::from(format!("{} {region}", tr(lang, "落地", "Exit"))),
            p.fg,
            p.panel_alt,
        ));
    }

    let grid = match report {
        Some(r) => div()
            .grid()
            .grid_cols(3)
            .gap_4()
            .child(reachability_group(
                tr(lang, "AI 服务", "AI services"),
                [
                    ("ChatGPT".into(), &r.chatgpt),
                    ("Claude".into(), &r.claude),
                    ("Gemini".into(), &r.gemini),
                    ("Copilot".into(), &r.copilot),
                ],
                hide_region,
                lang,
                p,
            ))
            .child(reachability_group(
                tr(lang, "流媒体", "Streaming"),
                [
                    ("Netflix".into(), &r.netflix),
                    ("YouTube".into(), &r.youtube),
                    ("Disney+".into(), &r.disney),
                    ("TikTok".into(), &r.tiktok),
                ],
                hide_region,
                lang,
                p,
            ))
            .child(reachability_group(
                tr(lang, "出口环境", "Environment"),
                [
                    (tr(lang, "搜索免验证", "Web clean"), &r.web_clean),
                    (tr(lang, "谷歌学术", "Google Scholar"), &r.scholar),
                    ("GitHub".into(), &r.dev_hub),
                    ("UDP STUN".into(), &r.udp_stun),
                ],
                hide_region,
                lang,
                p,
            )),
        None => div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "尚未执行可达性检测；点击下方「检测可达性」即可一次测完（同出口 IP 缓存 6 小时）。",
            "Not tested yet. Click Check reachability below (6h shared cache per exit IP).",
        )),
    };

    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(surface_bg)
        .child(header)
        .child(grid)
}

fn reachability_group<'a>(
    heading: SharedString,
    items: [(SharedString, &'a ServiceCheckResult); 4],
    hide_region: bool,
    lang: Language,
    p: &Palette,
) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(p.muted_fg).child(heading));
    for (name, res) in items {
        if name.is_empty() {
            continue;
        }
        let (sym, fg) = match res.status {
            ReachabilityCheckStatus::Pass => ("✓", p.ok_text),
            ReachabilityCheckStatus::Limited => ("△", p.warn_text),
            ReachabilityCheckStatus::Blocked => ("✕", p.danger_text),
            ReachabilityCheckStatus::Untested => ("—", p.muted_fg),
        };
        let mut status_text = sym.to_string();
        if !hide_region && let Some(reg) = res.region.as_deref() {
            status_text.push_str(&format!(" {reg}"));
        }
        if let Some(detail) = res.detail.as_deref()
            && res.status != ReachabilityCheckStatus::Pass
        {
            status_text.push_str(&format!(" ({detail})"));
        } else if res.status == ReachabilityCheckStatus::Pass && res.region.is_none() {
            status_text.push_str(match lang {
                Language::Zh => " 可用",
                Language::En => " OK",
            });
        }
        col = col.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(div().text_color(p.fg).child(name))
                .child(div().truncate().text_color(fg).child(status_text)),
        );
    }
    col
}

fn hourly_strip(
    slots: &[xrose_core::runtime::health::HourlySummarySlot],
    lang: Language,
    p: &Palette,
    cx: &App,
) -> Div {
    let max_rtt = slots
        .iter()
        .filter_map(|s| s.avg_p50_ms)
        .max()
        .unwrap_or(1)
        .max(1);
    let mut bars = div()
        .h(px(HEALTH_HOURLY_BAR_HEIGHT))
        .w_full()
        .flex()
        .items_end()
        .gap_1();
    for slot in slots {
        let ratio = slot
            .avg_p50_ms
            .map(|r| (r as f32 / max_rtt as f32).clamp(0.18, 1.0))
            .unwrap_or(0.08);
        let color = if slot.avg_p50_ms.is_some() {
            latency_color_ms(p, slot.avg_p50_ms)
        } else {
            p.panel_alt
        };
        let tip: SharedString = match slot.avg_p50_ms {
            Some(rtt) => format!("{:02}:00 · P50 {} ms", slot.hour, rtt).into(),
            None => format!("{:02}:00 · {}", slot.hour, tr(lang, "无数据", "No data")).into(),
        };
        bars = bars.child(
            div()
                .id(SharedString::from(format!("health-hour-{}", slot.hour)))
                .flex_1()
                .h(px((HEALTH_HOURLY_BAR_HEIGHT * ratio).max(3.)))
                .rounded_sm()
                .bg(color)
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
        );
    }
    let labels = div()
        .flex()
        .justify_between()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(p.muted_fg)
        .child("00:00")
        .child("06:00")
        .child("12:00")
        .child("18:00")
        .child("23:00");

    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .pt_1()
        .border_t_1()
        .border_color(p.hairline_border())
        .child(div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "24 小时延迟分布",
            "24-hour latency profile",
        )))
        .child(bars)
        .child(labels)
}

#[cfg(test)]
mod tests {
    use super::{
        format_location_and_egress, format_protocol_summary, format_sni, format_transport_extra,
    };
    use xrose_core::model::{Node, Protocol, RealityOptions, Transport};
    use xrose_core::runtime::location::NodeLocation;
    use xrose_core::settings::Language;

    #[test]
    fn protocol_and_tls_summary_reflects_reality_tls_and_plain() {
        let mut vless = Node::new("V", Protocol::Vless, "198.51.100.1", 443);
        vless.tls.enabled = true;
        vless.tls.reality = Some(RealityOptions {
            public_key: "pub".into(),
            short_id: "01".into(),
        });
        assert_eq!(
            format_protocol_summary(&vless, Language::Zh),
            "VLESS · TCP · Reality"
        );

        let mut vmess = Node::new("M", Protocol::Vmess, "198.51.100.2", 80);
        vmess.transport = Transport::WebSocket {
            path: "/ws".into(),
            host: Some("edge.example.com".into()),
            headers: Default::default(),
        };
        assert_eq!(
            format_protocol_summary(&vmess, Language::En),
            "VMESS · WS · No TLS"
        );
        assert_eq!(
            format_transport_extra(&vmess, false, Language::En).as_deref(),
            Some("path /ws · host edge.example.com")
        );
        assert_eq!(
            format_transport_extra(&vmess, true, Language::En).as_deref(),
            Some("path ••• · host •••")
        );
    }

    #[test]
    fn privacy_mode_masks_sni_location_and_egress() {
        let mut node = Node::new("V", Protocol::Vless, "198.51.100.1", 443);
        node.tls.server_name = Some("cdn.alpha.example.com".into());
        assert_eq!(
            format_sni(&node, false).as_deref(),
            Some("cdn.alpha.example.com")
        );
        assert_eq!(format_sni(&node, true).as_deref(), Some("***.***.***.com"));

        let loc = NodeLocation {
            label: Some("日本 · 东京".into()),
            country: Some("JP".into()),
            pending: false,
            checked_at: 0,
        };
        assert_eq!(
            format_location_and_egress(Some(&loc), Some("JP"), false, Language::Zh),
            "日本 · 东京 · 落地 JP"
        );
        assert_eq!(
            format_location_and_egress(Some(&loc), Some("JP"), true, Language::Zh),
            "隐私模式已隐藏"
        );
    }
}
