//! Shared traffic ranking rendered as stacked bars or a table.

use std::time::{Duration, Instant};

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::*;
use xrose_core::traffic_statistics::{
    self as stats, TrafficDimension, TrafficEntityKind, TrafficQuery, TrafficRow, TrafficWindow,
};
use xrose_core::util::format_bytes;

use super::menus::{self, MenuTarget};
use super::theme::{self, Palette};
use super::widgets::*;
use super::{AppView, Page, text_input};

const BAR_PAGE_SIZE: usize = 10;
const TABLE_PAGE_SIZE: usize = 50;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TrafficDisplayMode {
    #[default]
    Bar,
    Table,
}

pub(super) struct TrafficUi {
    pub expanded: bool,
    display: TrafficDisplayMode,
    query: TrafficQuery,
    dimension_chosen: bool,
    page: usize,
    table_limit: usize,
    date_input: Entity<InputState>,
    date_error: bool,
    last_query: Instant,
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
            expanded: false,
            display: TrafficDisplayMode::Bar,
            query: TrafficQuery::default(),
            dimension_chosen: false,
            page: 0,
            table_limit: TABLE_PAGE_SIZE,
            date_input: text_input(window, cx, "yyyy-MM-dd"),
            date_error: false,
            last_query: Instant::now(),
            focused: None,
            focus_handles,
        }
    }
}

impl AppView {
    pub(super) fn refresh_statistics(&mut self, reset_page: bool, cx: &mut Context<Self>) {
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
        self.snapshot = self.service.snapshot();
        self.traffic_ui.last_query = Instant::now();
        cx.notify();
    }
    pub(super) fn statistics_tick(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Connections
            && self.traffic_ui.expanded
            && self.traffic_ui.last_query.elapsed() >= Duration::from_secs(5)
            && !self.snapshot.traffic_statistics.loading
        {
            self.refresh_statistics(false, cx);
        }
    }

    pub(super) fn traffic_statistics_card(
        &mut self,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.traffic_ui.expanded;
        let enabled = self.snapshot.settings.traffic_statistics_enabled;
        let view = self.snapshot.traffic_statistics.clone();
        let mut panel = card(p).flex_shrink_0().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .flex_wrap()
                .child(
                    Button::new("traffic-expand")
                        .ghost()
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(self.tr("流量统计", "Traffic statistics"))
                        .accessibility_label(if expanded {
                            self.tr("收起流量统计", "Collapse traffic statistics")
                        } else {
                            self.tr("展开流量统计", "Expand traffic statistics")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.traffic_ui.expanded = !this.traffic_ui.expanded;
                            if this.traffic_ui.expanded {
                                if !this.traffic_ui.dimension_chosen {
                                    let core = this
                                        .snapshot
                                        .process
                                        .core
                                        .unwrap_or(this.snapshot.settings.core);
                                    this.traffic_ui.query.dimension =
                                        if core.capabilities().clash_connections
                                            && this.snapshot.model.work_mode
                                                == xrose_core::model::WorkMode::Direct
                                        {
                                            TrafficDimension::Process
                                        } else {
                                            TrafficDimension::Node
                                        };
                                }
                                this.refresh_statistics(false, cx);
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            more_button(
                                "traffic-actions",
                                self.tr("统计操作", "Statistics actions"),
                            )
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                MenuTarget::TrafficStatistics,
                            )),
                        )
                        .child(
                            toggle(
                                "traffic-enabled",
                                self.tr("启用统计", "Collect statistics"),
                                enabled,
                            )
                            .disabled(self.snapshot.busy.iter().any(|b| b == "traffic-setting"))
                            .on_change(cx.listener(
                                |this, checked, _, _| {
                                    this.service.set_traffic_statistics_enabled(*checked)
                                },
                            )),
                        ),
                ),
        );
        if !expanded {
            return panel;
        }
        panel = panel.child(div().text_xs().text_color(p.muted_fg).child(self.tr(
            "记录内核处理的代理和直连流量，关闭后保留历史。",
            "Records proxy and direct traffic handled by the core. History is retained when collection is off.")));
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap_3()
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
                    .on_change(cx.listener(|this, index, _, cx| {
                        this.traffic_ui.dimension_chosen = true;
                        this.traffic_ui.query.dimension = [
                            TrafficDimension::Node,
                            TrafficDimension::Process,
                            TrafficDimension::Domain,
                        ][*index];
                        this.refresh_statistics(true, cx);
                    })),
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
                    .on_change(cx.listener(|this, index, _, cx| {
                        this.traffic_ui.query.window = [
                            TrafficWindow::Session,
                            TrafficWindow::Day,
                            TrafficWindow::Week,
                            TrafficWindow::Month,
                        ][*index];
                        this.refresh_statistics(true, cx);
                    })),
                )
                .child(
                    ChoiceGroup::new(
                        "traffic-display",
                        vec![self.tr("柱状图", "Bar chart"), self.tr("表格", "Table")],
                        usize::from(self.traffic_ui.display == TrafficDisplayMode::Table),
                    )
                    .on_change(cx.listener(|this, index, _, cx| {
                        this.traffic_ui.display = if *index == 0 {
                            TrafficDisplayMode::Bar
                        } else {
                            TrafficDisplayMode::Table
                        };
                        cx.notify();
                    })),
                ),
        );
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
                        Button::new("traffic-previous")
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .tooltip(self.tr("上一周期", "Previous period"))
                            .accessibility_label(self.tr("上一周期", "Previous period"))
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
                        Button::new("traffic-next")
                            .ghost()
                            .icon(IconName::ChevronRight)
                            .disabled(end >= today)
                            .tooltip(self.tr("下一周期", "Next period"))
                            .accessibility_label(self.tr("下一周期", "Next period"))
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
            panel = panel.child(div().text_xs().text_color(p.danger_text).child(self.tr("统计尚未保存，正在重试。请检查磁盘空间和数据目录权限。", "Statistics are not saved yet. Retrying; check disk space and data directory permissions.")));
        }
        if view.collection_error || view.has_gaps {
            panel = panel.child(div().text_xs().text_color(p.warn_text).child(self.tr(
                "部分时段未能取得流量数据，统计可能不完整。",
                "Traffic data was unavailable for some intervals; statistics may be incomplete.",
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
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_4()
                .child(div().text_sm().text_color(p.data_down).child(format!(
                    "{}  {}",
                    self.tr("↓ 下载", "↓ Download"),
                    format_bytes(view.total.download)
                )))
                .child(div().text_sm().text_color(p.data_up).child(format!(
                    "{}  {}",
                    self.tr("↑ 上传", "↑ Upload"),
                    format_bytes(view.total.upload)
                )))
                .child(div().text_sm().child(format!(
                    "{}  {}",
                    self.tr("合计", "Total"),
                    format_bytes(view.total.total())
                ))),
        );
        if let Some(at) = view.last_sample_at {
            let text = chrono::DateTime::from_timestamp(at, 0)
                .map(|at| {
                    at.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(format!("{} {text}", self.tr("最后采样", "Last sample"))),
            );
        }
        if view.has_singbox {
            panel = panel.child(div().text_xs().text_color(p.muted_fg).child(self.tr("分类根据连接采样汇总，短连接可能未被采样；无法确定归属的流量计入“未归因”。", "Categories use connection samples, which may miss short connections. Traffic without a known category is listed as unattributed.")));
        }
        if view.has_xray || self.snapshot.process.core == Some(xrose_core::model::CoreKind::Xray) {
            panel = panel.child(div().text_xs().text_color(p.muted_fg).child(if self.traffic_ui.query.dimension == TrafficDimension::Node {
                self.tr("Xray 节点流量可能包含协议开销及前置代理流量，节点相加可能与区间合计不同。", "Xray node counters may include protocol overhead and chained proxy traffic; their sum can differ from the period total.")
            } else { self.tr("Xray 不提供进程和域名分类，其流量仅计入区间合计。", "Xray does not provide process or domain categories. Its traffic is included only in the period total.") }));
        }
        if view.rows.is_empty() {
            panel = panel.child(empty_hint(
                p,
                if !enabled
                    && view.last_sample_at.is_none()
                    && self.traffic_ui.query.window == TrafficWindow::Session
                {
                    self.tr(
                        "开启统计后可查看流量排行与历史。",
                        "Enable collection to view traffic rankings and history.",
                    )
                } else {
                    self.tr(
                        "所选范围暂无分类数据。",
                        "No category data for this period.",
                    )
                },
            ));
        } else if self.traffic_ui.display == TrafficDisplayMode::Bar {
            panel = panel.child(self.traffic_bars(p, cx));
        } else {
            panel = panel.child(self.traffic_table(p, cx));
        }
        panel
    }

    fn traffic_label(&self, row: &TrafficRow) -> String {
        let label = match row.entity.kind {
            TrafficEntityKind::Named => {
                if self.snapshot.settings.privacy_mode
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
    fn traffic_bars(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let view = &self.snapshot.traffic_statistics;
        let page = self
            .traffic_ui
            .page
            .min(view.row_count.saturating_sub(1) / BAR_PAGE_SIZE);
        let start = page * BAR_PAGE_SIZE;
        let end = (start + BAR_PAGE_SIZE).min(view.rows.len());
        let rows = view.rows.get(start..end).unwrap_or_default();
        let peak = rows.iter().map(|r| r.bytes.total()).max().unwrap_or(0);
        let (unit, divisor) = chart_unit(peak);
        let axis = div()
            .flex()
            .flex_col()
            .justify_between()
            .h(px(theme::STATS_CHART_HEIGHT))
            .w(px(theme::STATS_AXIS_WIDTH))
            .flex_shrink_0()
            .text_xs()
            .text_color(p.muted_fg)
            .children((0..=4).map(|i| {
                div().child(format!(
                    "{:.1} {unit}",
                    peak as f64 / divisor * (4 - i) as f64 / 4.
                ))
            }));
        let mut columns = div().flex().gap_1().flex_1().min_w_0();
        for (index, row) in rows.iter().enumerate() {
            let (down, up) = bar_heights(row.bytes.download, row.bytes.upload, peak);
            let detail = self.traffic_detail(row);
            let tip = detail.clone();
            let label = self.traffic_label(row);
            columns = columns.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .id(("traffic-bar", index))
                            .h(px(theme::STATS_CHART_HEIGHT))
                            .w_full()
                            .px_2()
                            .flex()
                            .flex_col()
                            .justify_end()
                            .border_b_1()
                            .border_color(p.border)
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .child(div().h(px(up)).bg(p.data_up).w_full().flex_shrink_0())
                            .child(div().h(px(down)).bg(p.data_down).w_full().flex_shrink_0()),
                    )
                    .child(
                        Button::new(("traffic-bar-label", index))
                            .ghost()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .label(label)
                            .tooltip(detail.clone())
                            .accessibility_label(detail)
                            .track_focus(&self.traffic_ui.focus_handles[index])
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.traffic_ui.focused = Some(index);
                                cx.notify();
                            })),
                    ),
            );
        }
        let mut chart = div().flex().flex_col().gap_3().child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(axis)
                .child(columns),
        );
        if let Some(row) = rows.get(self.traffic_ui.focused.unwrap_or(0)) {
            chart = chart.child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(self.traffic_detail(row)),
            );
        }
        chart.child(
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
        let mut table = div().flex().flex_col().gap_2().w_full().child(
            div()
                .flex()
                .gap_3()
                .text_xs()
                .text_color(p.muted_fg)
                .child(div().flex_1().child(self.tr("名称", "Name")))
                .children(
                    [
                        self.tr("上传", "Upload"),
                        self.tr("下载", "Download"),
                        self.tr("合计", "Total"),
                    ]
                    .into_iter()
                    .map(|label| {
                        div()
                            .w(px(theme::STATS_VALUE_WIDTH))
                            .flex_shrink_0()
                            .text_right()
                            .child(label)
                    }),
                ),
        );
        for (index, row) in view
            .rows
            .iter()
            .take(self.traffic_ui.table_limit)
            .enumerate()
        {
            let tip = self.traffic_detail(row);
            table = table.child(
                div()
                    .id(("traffic-table-row", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .text_sm()
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(self.traffic_label(row)),
                    )
                    .children(
                        [row.bytes.upload, row.bytes.download, row.bytes.total()]
                            .into_iter()
                            .map(|bytes| {
                                div()
                                    .w(px(theme::STATS_VALUE_WIDTH))
                                    .flex_shrink_0()
                                    .text_right()
                                    .child(format_bytes(bytes))
                            }),
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

fn bar_heights(download: u64, upload: u64, peak: u64) -> (f32, f32) {
    if peak == 0 {
        return (0., 0.);
    }
    let scale = theme::STATS_CHART_HEIGHT as f64 / peak as f64;
    (
        (download as f64 * scale) as f32,
        (upload as f64 * scale) as f32,
    )
}
fn chart_unit(peak: u64) -> (&'static str, f64) {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut index = 0;
    let mut divisor = 1.;
    while peak as f64 / divisor >= 1024. && index + 1 < units.len() {
        index += 1;
        divisor *= 1024.;
    }
    (units[index], divisor)
}

#[cfg(test)]
mod tests {
    use super::{TrafficDisplayMode, bar_heights, chart_unit, theme};
    #[test]
    fn bars_keep_zero_small_values_and_stack_proportions() {
        assert_eq!(bar_heights(0, 0, 0), (0., 0.));
        let (down, up) = bar_heights(75, 25, 100);
        assert!((down + up - theme::STATS_CHART_HEIGHT).abs() < 0.001);
        assert!((down / up - 3.).abs() < 0.001);
        assert!(bar_heights(1, 0, 100000).0 > 0.);
        assert!(
            bar_heights(u64::MAX / 2, u64::MAX / 2, u64::MAX)
                .0
                .is_finite()
        );
        assert_eq!(chart_unit(1024), ("KiB", 1024.));
        assert!(TrafficDisplayMode::default() == TrafficDisplayMode::Bar);
    }
}
