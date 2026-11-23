//! Shared traffic ranking rendered as horizontal stacked dual-color bars or a table.

use std::time::{Duration, Instant};

use gpui_base::{Disableable, Selectable};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::CoreKind;
use xrose_core::traffic_statistics::{
    self as stats, TrafficDimension, TrafficEntityKind, TrafficQuery, TrafficRow, TrafficWindow,
};
use xrose_core::util::format_bytes;

use super::connections::ConnectionsTab;
use super::menus::{self, MenuTarget};
use super::theme::{self, Palette};
use super::widgets::*;
use super::{AppView, Page, text_input};

const BAR_PAGE_SIZE: usize = 10;
const TABLE_PAGE_SIZE: usize = 50;
const MIN_NONZERO_BAR_FILL: f32 = 0.008;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TrafficDisplayMode {
    #[default]
    Bar,
    Table,
}

pub(super) struct TrafficUi {
    display: TrafficDisplayMode,
    query: TrafficQuery,
    dimension_chosen: bool,
    help_open: bool,
    page: usize,
    table_limit: usize,
    date_input: Entity<InputState>,
    date_error: bool,
    last_query: Option<Instant>,
    focused: Option<usize>,
    focus_handles: Vec<FocusHandle>,
}

impl TrafficUi {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        let focus_handles: Vec<_> = (0..BAR_PAGE_SIZE).map(|_| cx.focus_handle()).collect();
        for (index, handle) in focus_handles.iter().enumerate() {
            cx.on_focus(handle, window, move |this, _, cx| {
                this.traffic_ui.focused = Some(index);
                cx.notify();
            })
            .detach();
        }
        Self {
            display: TrafficDisplayMode::Bar,
            query: TrafficQuery::default(),
            dimension_chosen: false,
            help_open: false,
            page: 0,
            table_limit: TABLE_PAGE_SIZE,
            date_input: text_input(window, cx, "yyyy-MM-dd"),
            date_error: false,
            last_query: None,
            focused: None,
            focus_handles,
        }
    }
}

impl AppView {
    pub(super) fn ensure_default_traffic_dimension(&mut self) {
        if self.traffic_ui.dimension_chosen {
            return;
        }
        let core = self
            .snapshot
            .process
            .core
            .unwrap_or(self.snapshot.settings.core);
        self.traffic_ui.query.dimension = if core.capabilities().clash_connections
            && self.snapshot.model.work_mode == xrose_core::model::WorkMode::Direct
        {
            TrafficDimension::Process
        } else {
            TrafficDimension::Node
        };
    }

    pub(super) fn refresh_statistics(&mut self, reset_page: bool, cx: &mut Context<Self>) {
        self.ensure_default_traffic_dimension();
        if reset_page {
            self.traffic_ui.page = 0;
            self.traffic_ui.table_limit = TABLE_PAGE_SIZE;
            self.traffic_ui.focused = None;
        }
        self.traffic_ui.query.limit = self
            .traffic_ui
            .table_limit
            .max((self.traffic_ui.page + 1) * BAR_PAGE_SIZE);
        self.service
            .query_traffic_statistics(self.traffic_ui.query.clone());
        self.refresh_snapshot();
        self.traffic_ui.last_query = Some(Instant::now());
        cx.notify();
    }

    pub(super) fn statistics_tick(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Connections || self.snapshot.traffic_statistics.loading {
            return;
        }
        let due = match self.traffic_ui.last_query {
            None => true,
            Some(last) => {
                if self.connections_tab == ConnectionsTab::Statistics {
                    last.elapsed() >= Duration::from_secs(5)
                } else {
                    self.snapshot.settings.traffic_statistics_enabled
                        && last.elapsed() >= Duration::from_secs(10)
                }
            }
        };
        if due {
            self.refresh_statistics(false, cx);
        }
    }

    pub(super) fn traffic_statistics_badge(&self) -> String {
        let view = &self.snapshot.traffic_statistics;
        if view.range_start.is_some() || view.total.total() > 0 {
            format_bytes(view.total.total())
        } else {
            "—".to_string()
        }
    }

    pub(super) fn traffic_statistics_header_actions(&self, cx: &mut Context<Self>) -> Div {
        let enabled = self.snapshot.settings.traffic_statistics_enabled;
        let collection_label = self.tr("启用流量统计", "Collect traffic statistics");
        let clear_busy = self.snapshot.busy.iter().any(|key| key == "traffic-clear");
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(
                more_button(
                    "traffic-statistics-actions",
                    self.tr("统计操作", "Statistics actions"),
                    cx,
                )
                .loading(clear_busy)
                .dropdown_menu(menus::builder(
                    cx.entity().downgrade(),
                    MenuTarget::TrafficStatistics,
                )),
            )
            .child(
                Switch::new("traffic-enabled")
                    .accessibility_label(collection_label.clone())
                    .label(self.tr("流量采样", "Sampling"))
                    .tooltip(collection_label)
                    .checked(enabled)
                    .disabled(
                        self.snapshot
                            .busy
                            .iter()
                            .any(|key| key == "traffic-setting"),
                    )
                    .on_change(cx.listener(|this, checked, _, _| {
                        this.service.set_traffic_statistics_enabled(*checked)
                    })),
            )
    }

    pub(super) fn traffic_statistics_content(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let enabled = self.snapshot.settings.traffic_statistics_enabled;
        let view = &self.snapshot.traffic_statistics;
        let help_open = self.traffic_ui.help_open;
        let rules_summary = self.tr(
            "记录内核处理的代理和直连流量，关闭后保留历史。分类根据连接采样汇总，短连接可能未被采样；无法确定归属的流量计入“未归因”。",
            "Records proxy and direct traffic handled by the core; history is retained when paused. Short connections may miss sampling and unknown ownership counts as unattributed.",
        );
        let help_variant = if help_open {
            ButtonCustomVariant::new(cx)
                .color(p.accent)
                .foreground(p.primary)
                .hover(p.accent)
                .active(p.accent)
        } else {
            ButtonCustomVariant::new(cx)
                .foreground(p.muted_fg)
                .hover(p.neutral_hover())
                .active(p.neutral_active())
        };

        let mut panel = div().flex().flex_col().gap_3();
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .flex_wrap()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            ChoiceGroup::new(
                                "traffic-dimension",
                                vec![
                                    self.tr("按节点", "By node"),
                                    self.tr("按进程", "By process"),
                                    self.tr("按主域名", "By domain"),
                                ],
                                match self.traffic_ui.query.dimension {
                                    TrafficDimension::Node => 0,
                                    TrafficDimension::Process => 1,
                                    TrafficDimension::Domain => 2,
                                },
                            )
                            .small()
                            .on_change(cx.listener(
                                |this, index, _, cx| {
                                    this.traffic_ui.dimension_chosen = true;
                                    this.traffic_ui.query.dimension = [
                                        TrafficDimension::Node,
                                        TrafficDimension::Process,
                                        TrafficDimension::Domain,
                                    ][*index];
                                    this.refresh_statistics(true, cx);
                                },
                            )),
                        )
                        .child(
                            ChoiceGroup::new(
                                "traffic-window",
                                vec![
                                    self.tr("运行以来", "This session"),
                                    self.tr("日", "Day"),
                                    self.tr("周", "Week"),
                                    self.tr("月", "Month"),
                                ],
                                match self.traffic_ui.query.window {
                                    TrafficWindow::Session => 0,
                                    TrafficWindow::Day => 1,
                                    TrafficWindow::Week => 2,
                                    TrafficWindow::Month => 3,
                                },
                            )
                            .small()
                            .on_change(cx.listener(
                                |this, index, _, cx| {
                                    this.traffic_ui.query.window = [
                                        TrafficWindow::Session,
                                        TrafficWindow::Day,
                                        TrafficWindow::Week,
                                        TrafficWindow::Month,
                                    ][*index];
                                    this.refresh_statistics(true, cx);
                                },
                            )),
                        )
                        .child(
                            ChoiceGroup::new(
                                "traffic-display",
                                vec![self.tr("条形图", "Bar chart"), self.tr("表格", "Table")],
                                usize::from(self.traffic_ui.display == TrafficDisplayMode::Table),
                            )
                            .small()
                            .on_change(cx.listener(
                                |this, index, _, cx| {
                                    this.traffic_ui.display = if *index == 0 {
                                        TrafficDisplayMode::Bar
                                    } else {
                                        TrafficDisplayMode::Table
                                    };
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_shrink_0()
                        .ml_auto()
                        .child(
                            Button::new("traffic-attribution-help")
                                .xsmall()
                                .compact()
                                .h(px(24.))
                                .px_2()
                                .custom(help_variant)
                                .selected(help_open)
                                .toggled(help_open)
                                .icon(IconName::Info)
                                .label(self.tr("归因说明", "Attribution"))
                                .tooltip(rules_summary)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.traffic_ui.help_open = !this.traffic_ui.help_open;
                                    cx.notify();
                                })),
                        ),
                ),
        );

        let xray_active = view.has_xray || self.snapshot.process.core == Some(CoreKind::Xray);
        if help_open {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(p.row_bg())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.fg)
                            .child(self.tr("流量采样与归因规则", "Traffic sampling & attribution")),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                        "记录内核处理的代理和直连流量，关闭后保留历史。",
                        "Records proxy and direct traffic handled by the core. History is retained when collection is off.",
                    )))
                    .when(
                        view.has_singbox
                            || self.snapshot.process.core == Some(CoreKind::SingBox),
                        |d| {
                            d.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                                "分类根据连接采样汇总，短连接可能未被采样；无法确定归属的流量计入“未归因”。",
                                "Categories use connection samples, which may miss short connections. Traffic without a known category is listed as unattributed.",
                            )))
                        },
                    )
                    .when(xray_active, |d| {
                        d.child(div().text_xs().text_color(p.muted_fg).child(
                            if self.traffic_ui.query.dimension == TrafficDimension::Node {
                                self.tr(
                                    "Xray 节点流量可能包含协议开销及前置代理流量，节点相加可能与区间合计不同。",
                                    "Xray node counters may include protocol overhead and chained proxy traffic; their sum can differ from the period total.",
                                )
                            } else {
                                self.tr(
                                    "Xray 不提供进程和域名分类，其流量仅计入区间合计。",
                                    "Xray does not provide process or domain categories. Its traffic is included only in the period total.",
                                )
                            },
                        ))
                    }),
            );
        }

        if self.traffic_ui.query.window != TrafficWindow::Session {
            let today = stats::today();
            let (start, end) = self.traffic_ui.query.range(today);
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        toolbar_icon_button(
                            "traffic-previous",
                            IconName::ChevronLeft,
                            self.tr("上一周期", "Previous period"),
                            cx,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.traffic_ui.query =
                                this.traffic_ui.query.shifted(false, stats::today());
                            this.refresh_statistics(true, cx);
                        })),
                    )
                    .child(div().text_sm().child(if start == end {
                        start.to_string()
                    } else {
                        format!("{start} — {end}")
                    }))
                    .child(
                        toolbar_icon_button(
                            "traffic-next",
                            IconName::ChevronRight,
                            self.tr("下一周期", "Next period"),
                            cx,
                        )
                        .disabled(end >= today)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.traffic_ui.query =
                                this.traffic_ui.query.shifted(true, stats::today());
                            this.refresh_statistics(true, cx);
                        })),
                    )
                    .child(
                        Button::new("traffic-current")
                            .ghost()
                            .label(self.tr("当前周期", "Current period"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.traffic_ui.query.date = None;
                                this.refresh_statistics(true, cx);
                            })),
                    )
                    .child(
                        div()
                            .w(px(theme::STATS_DATE_WIDTH))
                            .child(Input::new(&self.traffic_ui.date_input)),
                    )
                    .child(
                        Button::new("traffic-jump")
                            .label(self.tr("跳转", "Go"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let date = this
                                    .traffic_ui
                                    .date_input
                                    .read(cx)
                                    .value()
                                    .trim()
                                    .parse::<chrono::NaiveDate>();
                                if let Ok(date) = date
                                    && date <= stats::today()
                                    && date >= chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
                                {
                                    this.traffic_ui.query.date = Some(date);
                                    this.traffic_ui.date_error = false;
                                    this.refresh_statistics(true, cx);
                                } else {
                                    this.traffic_ui.date_error = true;
                                    cx.notify();
                                }
                            })),
                    ),
            );
            if self.traffic_ui.date_error {
                panel = panel.child(div().text_xs().text_color(p.danger_text).child(self.tr(
                    "请输入有效日期（yyyy-MM-dd），日期不得晚于今天。",
                    "Enter a valid date (yyyy-MM-dd), no later than today.",
                )));
            }
        }
        if view.loading {
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(self.tr("正在读取统计…", "Loading statistics…")),
            );
        }
        if view.query_error.is_some() {
            panel = panel.child(div().text_xs().text_color(p.danger_text).child(self.tr(
                "无法读取统计，请检查数据目录权限后重试。",
                "Cannot read statistics. Check data directory permissions and retry.",
            )));
        }
        if view.save_error.is_some() {
            panel = panel.child(div().text_xs().text_color(p.danger_text).child(self.tr(
                "统计尚未保存，正在重试。请检查磁盘空间和数据目录权限。",
                "Statistics are not saved yet. Retrying; check disk space and data directory permissions.",
            )));
        }
        if !self.snapshot.process.phase.is_alive() && enabled {
            panel = panel.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                "内核未运行，启动后继续统计。",
                "The core is stopped. Collection resumes when it starts.",
            )));
        }
        if view.range_start.is_none() && (view.loading || view.query_error.is_some()) {
            return panel;
        }

        let mono = cx.theme().mono_font_family.clone();
        let last_sample = view.last_sample_at.and_then(|at| {
            chrono::DateTime::from_timestamp(at, 0).map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
        });
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .flex_wrap()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(p.row_bg())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_4()
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_color(p.data_down)
                                .child(div().size(px(8.)).rounded_xs().bg(p.data_down))
                                .child(self.tr("↓ 下载", "↓ Download"))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format_bytes(view.total.download)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_color(p.data_up)
                                .child(div().size(px(8.)).rounded_xs().bg(p.data_up))
                                .child(self.tr("↑ 上传", "↑ Upload"))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format_bytes(view.total.upload)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("Σ 合计", "Σ Total")),
                                )
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(p.fg)
                                        .child(format_bytes(view.total.total())),
                                ),
                        ),
                )
                .when(
                    view.collection_error || view.has_gaps || last_sample.is_some(),
                    |d| {
                        let gap_tip = self.tr(
                            "部分时段未能取得流量数据，统计可能不完整。",
                            "Traffic data was unavailable for some intervals; statistics may be incomplete.",
                        );
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .flex_wrap()
                                .ml_auto()
                                .when(view.collection_error || view.has_gaps, |w| {
                                    w.child(
                                        div()
                                            .id("traffic-incomplete-warning")
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(p.warning.opacity(theme::SEMANTIC_ALPHA))
                                            .text_xs()
                                            .text_color(p.warn_text)
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(gap_tip.clone()).build(window, cx)
                                            })
                                            .child(Icon::new(IconName::TriangleAlert).size(px(12.)))
                                            .child(self.tr("部分时段采样不完整", "Incomplete intervals")),
                                    )
                                })
                                .when_some(last_sample, |s, text| {
                                    s.child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .font_family(mono.clone())
                                            .child(format!(
                                                "{} {text}",
                                                self.tr("最后采样", "Last sample")
                                            )),
                                    )
                                }),
                        )
                    },
                ),
        );

        if xray_active && self.traffic_ui.query.dimension != TrafficDimension::Node && !help_open {
            panel = panel.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                "Xray 不提供进程和域名分类，其流量仅计入区间合计。",
                "Xray does not provide process or domain categories. Its traffic is included only in the period total.",
            )));
        }

        if view.rows.is_empty() {
            panel = panel.child(empty_hint(
                p,
                if !enabled
                    && view.last_sample_at.is_none()
                    && self.traffic_ui.query.window == TrafficWindow::Session
                {
                    self.tr(
                        "开启流量采样后可查看流量排行与历史。",
                        "Enable sampling to view traffic rankings and history.",
                    )
                } else {
                    self.tr(
                        "所选范围暂无分类数据。",
                        "No category data for this period.",
                    )
                },
            ));
        } else if self.traffic_ui.display == TrafficDisplayMode::Bar {
            panel = panel.child(self.traffic_horizontal_bars(p, cx));
        } else {
            panel = panel.child(self.traffic_table(p, cx));
        }
        panel
    }

    fn traffic_label(&self, row: &TrafficRow) -> String {
        let label = match row.entity.kind {
            TrafficEntityKind::Named => {
                if (self.hide_ips || self.snapshot.settings.privacy_mode)
                    && self.traffic_ui.query.dimension != TrafficDimension::Process
                {
                    self.tr("已隐藏", "Hidden").to_string()
                } else {
                    row.entity.label.clone()
                }
            }
            TrafficEntityKind::Direct => self.tr("本机直连", "Direct").to_string(),
            TrafficEntityKind::Unknown => self.tr("未知", "Unknown").to_string(),
            TrafficEntityKind::NoDomain => self.tr("无域名", "No domain").to_string(),
            TrafficEntityKind::Unattributed => self.tr("未归因", "Unattributed").to_string(),
            TrafficEntityKind::Summary => self.tr("合计", "Total").to_string(),
        };
        if self.traffic_ui.query.dimension == TrafficDimension::Node {
            format!("{label} · {}", row.core.display_name())
        } else {
            label
        }
    }

    fn traffic_detail(&self, row: &TrafficRow) -> String {
        format!(
            "{}\n{}: {} · {}: {} · {}: {}",
            self.traffic_label(row),
            self.tr("下载", "Download"),
            format_bytes(row.bytes.download),
            self.tr("上传", "Upload"),
            format_bytes(row.bytes.upload),
            self.tr("合计", "Total"),
            format_bytes(row.bytes.total())
        )
    }

    fn share_denominator(&self) -> u64 {
        let view = &self.snapshot.traffic_statistics;
        let rows_sum = view
            .rows
            .iter()
            .fold(0u64, |acc, r| acc.saturating_add(r.bytes.total()));
        view.total.total().max(rows_sum)
    }

    fn traffic_horizontal_bars(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let view = &self.snapshot.traffic_statistics;
        let page = self
            .traffic_ui
            .page
            .min(view.row_count.saturating_sub(1) / BAR_PAGE_SIZE);
        let start = page * BAR_PAGE_SIZE;
        let end = (start + BAR_PAGE_SIZE).min(view.rows.len());
        let rows = view.rows.get(start..end).unwrap_or_default();
        let peak = view.rows.iter().map(|r| r.bytes.total()).max().unwrap_or(0);
        let share_denom = self.share_denominator();
        let mono = cx.theme().mono_font_family.clone();
        let hover_bg = p.row_hover();

        let mut list = div().flex().flex_col().gap_2().w_full();
        for (index, row) in rows.iter().enumerate() {
            let (bar_fill, dl_share, ul_share) =
                bar_segments(row.bytes.download, row.bytes.upload, peak);
            let share_text = format_share(row.bytes.total(), share_denom);
            let label = self.traffic_label(row);
            let tip = self.traffic_detail(row);
            let focused = self.traffic_ui.focused == Some(index);

            list = list.child(
                div()
                    .id(("traffic-hbar-row", index))
                    .track_focus(&self.traffic_ui.focus_handles[index])
                    .cursor_pointer()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2p5()
                    .rounded_lg()
                    .border_1()
                    .when(focused, |d| {
                        d.border_color(p.primary).bg(p.accent.opacity(0.4))
                    })
                    .when(!focused, |d| {
                        d.border_color(p.border.opacity(0.))
                            .bg(p.row_bg())
                            .hover(move |s| s.bg(hover_bg))
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.traffic_ui.focused = Some(index);
                        cx.notify();
                    }))
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
                                    .gap_2()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.fg)
                                            .truncate()
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(p.panel_alt)
                                            .text_xs()
                                            .font_family(mono.clone())
                                            .text_color(p.muted_fg)
                                            .flex_shrink_0()
                                            .child(share_text),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .font_family(mono.clone())
                                    .child(
                                        div().text_color(p.data_down).child(format!(
                                            "↓ {}",
                                            format_bytes(row.bytes.download)
                                        )),
                                    )
                                    .child(
                                        div()
                                            .text_color(p.data_up)
                                            .child(format!("↑ {}", format_bytes(row.bytes.upload))),
                                    )
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.fg)
                                            .child(format!(
                                                "Σ {}",
                                                format_bytes(row.bytes.total())
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .h(px(theme::STATS_BAR_HEIGHT))
                            .w_full()
                            .rounded_full()
                            .bg(p.panel_alt)
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .when(bar_fill > 0., |track| {
                                let fill = bar_fill.max(MIN_NONZERO_BAR_FILL);
                                track.child(
                                    div()
                                        .h_full()
                                        .w(relative(fill))
                                        .rounded_full()
                                        .overflow_hidden()
                                        .flex()
                                        .items_center()
                                        .when(dl_share > 0., |bar| {
                                            bar.child(
                                                div()
                                                    .h_full()
                                                    .w(relative(dl_share))
                                                    .bg(p.data_down),
                                            )
                                        })
                                        .when(ul_share > 0., |bar| {
                                            bar.child(
                                                div().h_full().w(relative(ul_share)).bg(p.data_up),
                                            )
                                        }),
                                )
                            }),
                    ),
            );
        }

        div().flex().flex_col().gap_3().child(list).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_xs().text_color(p.muted_fg).child(format!(
                    "{} {}–{} / {}",
                    self.tr("当前显示", "Showing"),
                    if rows.is_empty() { 0 } else { start + 1 },
                    end,
                    view.row_count
                )))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("traffic-bar-prev")
                                .ghost()
                                .label(self.tr("上一页", "Previous"))
                                .disabled(page == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.traffic_ui.page = this.traffic_ui.page.saturating_sub(1);
                                    this.traffic_ui.focused = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("traffic-bar-next")
                                .ghost()
                                .label(self.tr("下一页", "Next"))
                                .disabled(start + BAR_PAGE_SIZE >= view.row_count)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.traffic_ui.page += 1;
                                    this.traffic_ui.focused = None;
                                    if (this.traffic_ui.page + 1) * BAR_PAGE_SIZE
                                        > this.snapshot.traffic_statistics.rows.len()
                                    {
                                        this.refresh_statistics(false, cx);
                                    }
                                    cx.notify();
                                })),
                        ),
                ),
        )
    }

    fn traffic_table(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let view = &self.snapshot.traffic_statistics;
        let share_denom = self.share_denominator();
        let mono = cx.theme().mono_font_family.clone();
        let hover_bg = p.row_hover();
        let mut table = div().flex().flex_col().gap_1().w_full().child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(p.muted_fg)
                .child(div().flex_1().child(self.tr("归因对象", "Entity")))
                .child(
                    div()
                        .w(px(theme::STATS_VALUE_WIDTH))
                        .flex_shrink_0()
                        .text_right()
                        .text_color(p.data_down)
                        .child(self.tr("↓ 下载", "↓ Download")),
                )
                .child(
                    div()
                        .w(px(theme::STATS_VALUE_WIDTH))
                        .flex_shrink_0()
                        .text_right()
                        .text_color(p.data_up)
                        .child(self.tr("↑ 上传", "↑ Upload")),
                )
                .child(
                    div()
                        .w(px(theme::STATS_VALUE_WIDTH))
                        .flex_shrink_0()
                        .text_right()
                        .child(self.tr("Σ 合计", "Σ Total")),
                )
                .child(
                    div()
                        .w(px(theme::STATS_SHARE_WIDTH))
                        .flex_shrink_0()
                        .text_right()
                        .child(self.tr("占比", "Share")),
                ),
        );
        for (index, row) in view
            .rows
            .iter()
            .take(self.traffic_ui.table_limit)
            .enumerate()
        {
            let tip = self.traffic_detail(row);
            let share_text = format_share(row.bytes.total(), share_denom);
            table = table.child(
                div()
                    .id(("traffic-table-row", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .border_b_1()
                    .border_color(p.border)
                    .hover(move |s| s.bg(hover_bg))
                    .text_sm()
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(self.traffic_label(row)),
                    )
                    .child(
                        div()
                            .w(px(theme::STATS_VALUE_WIDTH))
                            .flex_shrink_0()
                            .text_right()
                            .font_family(mono.clone())
                            .text_color(p.data_down)
                            .child(format_bytes(row.bytes.download)),
                    )
                    .child(
                        div()
                            .w(px(theme::STATS_VALUE_WIDTH))
                            .flex_shrink_0()
                            .text_right()
                            .font_family(mono.clone())
                            .text_color(p.data_up)
                            .child(format_bytes(row.bytes.upload)),
                    )
                    .child(
                        div()
                            .w(px(theme::STATS_VALUE_WIDTH))
                            .flex_shrink_0()
                            .text_right()
                            .font_family(mono.clone())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(format_bytes(row.bytes.total())),
                    )
                    .child(
                        div()
                            .w(px(theme::STATS_SHARE_WIDTH))
                            .flex_shrink_0()
                            .text_right()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(share_text),
                    ),
            );
        }
        if view.row_count > self.traffic_ui.table_limit {
            table = table.child(
                Button::new("traffic-table-more")
                    .ghost()
                    .label(self.tr("加载更多", "Load more"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.traffic_ui.table_limit += TABLE_PAGE_SIZE;
                        this.refresh_statistics(false, cx);
                    })),
            );
        }
        table
    }
}

fn bar_segments(download: u64, upload: u64, peak: u64) -> (f32, f32, f32) {
    if peak == 0 || (download == 0 && upload == 0) {
        return (0., 0., 0.);
    }
    let total = download as f64 + upload as f64;
    let fill = (total / peak as f64).clamp(0., 1.) as f32;
    let dl_share = (download as f64 / total).clamp(0., 1.) as f32;
    let ul_share = (upload as f64 / total).clamp(0., 1.) as f32;
    (fill, dl_share, ul_share)
}

fn format_share(row_total: u64, period_total: u64) -> String {
    if row_total == 0 || period_total == 0 {
        return "0.0%".to_string();
    }
    let denom = period_total.max(row_total) as f64;
    let pct = (row_total as f64 / denom) * 100.0;
    if pct < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{pct:.1}%")
    }
}

#[cfg(test)]
mod tests {
    use super::{TrafficDisplayMode, bar_segments, format_share};

    #[test]
    fn horizontal_bars_keep_zero_small_values_and_stack_proportions() {
        assert_eq!(bar_segments(0, 0, 0), (0., 0., 0.));
        let (fill, dl_share, ul_share) = bar_segments(75, 25, 100);
        assert!((fill - 1.0).abs() < 0.001);
        assert!((dl_share - 0.75).abs() < 0.001);
        assert!((ul_share - 0.25).abs() < 0.001);

        let (tiny_fill, tiny_dl, tiny_ul) = bar_segments(1, 0, 100_000);
        assert!(tiny_fill > 0.);
        assert!((tiny_dl - 1.0).abs() < 0.001);
        assert_eq!(tiny_ul, 0.);

        let (max_fill, max_dl, max_ul) = bar_segments(u64::MAX / 2, u64::MAX / 2, u64::MAX);
        assert!(max_fill.is_finite() && (max_fill - 1.0).abs() < 0.001);
        assert!((max_dl - 0.5).abs() < 0.001);
        assert!((max_ul - 0.5).abs() < 0.001);

        assert!(TrafficDisplayMode::default() == TrafficDisplayMode::Bar);
    }

    #[test]
    fn share_percentages_format_zero_tiny_and_bounded_values() {
        assert_eq!(format_share(0, 1000), "0.0%");
        assert_eq!(format_share(10, 0), "0.0%");
        assert_eq!(format_share(1, 50_000), "<0.1%");
        assert_eq!(format_share(565, 1000), "56.5%");
        assert_eq!(format_share(1200, 1000), "100.0%");
    }
}
