//! Modal node & exit health detail dialog: 5-dimension breakdown, AI/streaming
//! reachability status, peak degradation, and 24-hour trend strip.

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::runtime::health::{
    DimBreakdown, HealthDim, NodeHealthView, ProfileEvaluation, ReachabilityCheckStatus,
    ReachabilityReport, ServiceCheckResult,
};
use xrose_core::service::HttpTestTarget;
use xrose_core::settings::Language;
use xrose_core::util::{format_bytes, format_speed};

use crate::ui::dialogs::{dialog_footer, settings_dialog};
use crate::ui::theme::{
    HEALTH_DETAIL_LABEL_WIDTH, HEALTH_HOURLY_BAR_HEIGHT, Palette, SEMANTIC_ALPHA, STATS_BAR_HEIGHT,
    health_grade_colors, latency_color_ms, palette,
};
use crate::ui::widgets::{inset_card, pill};
use crate::ui::{AppView, tr};

pub(in crate::ui) fn open_node_health_dialog(
    target: HttpTestTarget,
    title: String,
    window: &mut Window,
    cx: &mut Context<AppView>,
) {
    let view = cx.entity().downgrade();
    window.open_dialog(cx, move |dialog, window, cx| {
        let Some(view) = view.upgrade() else {
            return dialog;
        };
        let target = target.clone();
        let title = title.clone();
        view.update(cx, |this, cx| {
            let p = palette(cx);
            let lang = this.lang();
            let key = target.key();
            let health = this.snapshot.node_health.get(&key).cloned();
            let hide_region = this.hide_ips || this.snapshot.settings.privacy_mode;
            let key_busy = |busy_key: &str| this.snapshot.busy.iter().any(|k| k == busy_key);
            let http_busy = key_busy(&format!("latency:{key}"));
            let download_busy = key_busy(&format!("download:{key}"));
            let reach_busy = key_busy(&format!("reachability:{key}"));
            let any_probe_busy = [
                "latency",
                "download",
                "reachability",
                "auto-probe",
                "core",
                "mesh",
            ]
            .iter()
            .any(|k| key_busy(k));

            let body = health_detail_body(
                this,
                health.as_ref(),
                (http_busy, download_busy, reach_busy),
                hide_region,
                lang,
                &p,
                cx,
            );
            let title_text = format!("{} · {}", title, tr(lang, "健康详情", "Health details"));
            let footer_actions = div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new("health-dialog-http")
                        .icon(IconName::Gauge)
                        .label(tr(lang, "HTTP 测速", "HTTP test"))
                        .loading(http_busy)
                        .disabled(any_probe_busy)
                        .on_click({
                            let target = target.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.service.test_http_target(target.clone());
                                this.sync(cx);
                            })
                        }),
                )
                .child(
                    Button::new("health-dialog-download")
                        .icon(IconName::ArrowDown)
                        .label(tr(lang, "吞吐测速", "Throughput test"))
                        .loading(download_busy)
                        .disabled(any_probe_busy)
                        .on_click({
                            let target = target.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.service.test_download_target(target.clone());
                                this.sync(cx);
                            })
                        }),
                )
                .child(
                    Button::new("health-dialog-reach")
                        .primary()
                        .icon(IconName::HeartPulse)
                        .label(tr(lang, "检测可达性", "Check reachability"))
                        .loading(reach_busy)
                        .disabled(any_probe_busy)
                        .on_click({
                            let target = target.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.service.test_reachability_target(target.clone(), true);
                                this.sync(cx);
                            })
                        }),
                )
                .child(
                    Button::new("health-dialog-close")
                        .label(tr(lang, "关闭", "Close"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                );

            settings_dialog(dialog, SharedString::from(title_text), body, &p, window)
                .footer(dialog_footer(&p, None, footer_actions))
        })
    });
}

fn health_detail_body(
    this: &AppView,
    view: Option<&NodeHealthView>,
    probe_busy: (bool, bool, bool),
    hide_region: bool,
    lang: Language,
    p: &Palette,
    cx: &App,
) -> Div {
    let (http_busy, download_busy, reach_busy) = probe_busy;
    let mut col = div().flex().flex_col().gap_3();
    let used_bytes = this.snapshot.probe_budget_used_bytes;
    let budget_mb = this.snapshot.settings.daily_probe_budget_mb;
    let budget_text: SharedString = if budget_mb == 0 {
        format!(
            "{}：{} / {}",
            tr(lang, "今日探测流量", "Daily probe usage"),
            format_bytes(used_bytes),
            tr(lang, "不限", "Unlimited")
        )
        .into()
    } else {
        format!(
            "{}：{} / {} MiB",
            tr(lang, "今日探测流量", "Daily probe usage"),
            format_bytes(used_bytes),
            budget_mb
        )
        .into()
    };

    col = col.child(overview_card(view, budget_text, lang, p, cx));

    let Some(view) = view else {
        if http_busy || download_busy || reach_busy {
            return col.child(
                inset_card(p).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(p.muted_fg)
                        .child(Spinner::new().small().color(p.muted_fg))
                        .child(tr(
                            lang,
                            "正在执行检测，请稍候…",
                            "Running probe, please wait…",
                        )),
                ),
            );
        }
        return col.child(
            inset_card(p).child(div().text_sm().text_color(p.muted_fg).child(tr(
                lang,
                "暂无健康样本。可点击下方按钮执行 HTTP 测速、吞吐测速或可达性检测。",
                "No health samples yet. Run an HTTP, throughput, or reachability check below.",
            ))),
        );
    };

    if let Some(eval) = &view.general {
        if let Some(gate) = eval.gate {
            let label = match lang {
                Language::Zh => gate.label_zh(),
                Language::En => gate.label_en(),
            };
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(p.danger.opacity(0.4))
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

        col = col.child(dimensions_card(eval, probe_busy, lang, p, cx));

        if !eval.deductions.is_empty() || view.peak_degradation.is_some() {
            col = col.child(deductions_and_peak_card(eval, view, lang, p));
        }
    }

    col = col.child(reachability_card(
        view.reachability.as_ref(),
        hide_region,
        lang,
        p,
    ));

    if view.hourly_profile.iter().any(|s| s.avg_p50_ms.is_some()) {
        col = col.child(hourly_card(&view.hourly_profile, lang, p, cx));
    }

    col
}

fn overview_card(
    view: Option<&NodeHealthView>,
    budget_text: SharedString,
    lang: Language,
    p: &Palette,
    cx: &App,
) -> Div {
    let eval = view.and_then(|v| v.general.as_ref());
    let grade_badge = match eval {
        Some(ev) => {
            let (fg, bg) = health_grade_colors(p, ev.grade);
            let grade_name = match lang {
                Language::Zh => ev.grade.label_zh(),
                Language::En => ev.grade.label_en(),
            };
            pill(
                SharedString::from(format!(
                    "{} {} · {}",
                    ev.grade.as_str(),
                    ev.score.round() as u32,
                    grade_name
                )),
                fg,
                bg,
            )
        }
        None => pill(tr(lang, "— 未探测", "— Untested"), p.muted_fg, p.panel_alt),
    };

    let meta_text: SharedString = match eval {
        Some(ev) => {
            let mut parts = vec![format!(
                "15m {} · 24h {} · {} {}%",
                ev.short_score.round() as u32,
                ev.long_score.round() as u32,
                tr(lang, "置信度", "Confidence"),
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

    inset_card(p)
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w(px(0.))
                .child(grade_badge)
                .child(
                    div()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(p.muted_fg)
                        .child(meta_text),
                ),
        )
        .child(
            div()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(p.muted_fg)
                .child(budget_text),
        )
}

fn dimensions_card(
    eval: &ProfileEvaluation,
    probe_busy: (bool, bool, bool),
    lang: Language,
    p: &Palette,
    cx: &App,
) -> Div {
    let (http_busy, download_busy, reach_busy) = probe_busy;
    let mut box_div = inset_card(p).flex().flex_col().gap_2().child(
        div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "五维健康指标",
            "Five-dimension breakdown",
        )),
    );
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
                        .w(px(36.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_right()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(p.fg)
                        .child(score_str),
                )
                .child(
                    div()
                        .w(px(196.))
                        .flex_shrink_0()
                        .truncate()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
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

fn deductions_and_peak_card(
    eval: &ProfileEvaluation,
    view: &NodeHealthView,
    lang: Language,
    p: &Palette,
) -> Div {
    let mut box_div = inset_card(p).flex().flex_col().gap_1().child(
        div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "扣分说明与时段特征",
            "Deductions & peak alerts",
        )),
    );
    for d in &eval.deductions {
        let msg = match lang {
            Language::Zh => &d.zh,
            Language::En => &d.en,
        };
        box_div = box_div.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .child(div().text_color(p.fg).child(msg.clone()))
                .child(
                    div()
                        .text_color(p.warn_text)
                        .child(format!("-{:.1}", d.points_lost)),
                ),
        );
    }
    if let Some(peak) = &view.peak_degradation {
        box_div = box_div.child(div().text_xs().text_color(p.warn_text).child(match lang {
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
    box_div
}

fn reachability_card(
    report: Option<&ReachabilityReport>,
    hide_region: bool,
    lang: Language,
    p: &Palette,
) -> Div {
    let mut header = div().flex().items_center().justify_between().child(
        div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "场景可达性（AI / 流媒体 / 出口环境）",
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
            .gap_3()
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
            "尚未执行可达性检测；点击右下角「检测可达性」即可一次测完（同出口 IP 缓存 6 小时）。",
            "Not tested yet. Click Check reachability below (6h shared cache per exit IP).",
        )),
    };

    inset_card(p)
        .flex()
        .flex_col()
        .gap_2()
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

fn hourly_card(
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

    inset_card(p)
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_xs().text_color(p.muted_fg).child(tr(
            lang,
            "24 小时延迟分布",
            "24-hour latency profile",
        )))
        .child(bars)
        .child(labels)
}
