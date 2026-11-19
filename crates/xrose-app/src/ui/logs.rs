use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gpui_base::{Disableable, SelectableText};
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::*;
use xrose_core::core::logs::{LogFilter, LogPrivacy, LogSourceFilter};
use xrose_core::core::{LogEntry, LogStream};
use xrose_core::service::ToastLevel;
use xrose_core::settings::LogLevel;

use super::AppView;
use super::menus::{self, Command, MenuTarget};
use super::theme::{Palette, palette};
use super::widgets::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogAction {
    Copy,
    Export,
}

pub(super) struct LogListState {
    sharing: Option<LogAction>,
    revision: Option<u64>,
    frozen_entries: Option<Vec<LogEntry>>,
    list: ListState,
    rows: Rc<Vec<LogRow>>,
    filter: LogFilter,
    query: String,
    newest_first: bool,
}

const MAX_VISIBLE_LOGS: usize = 400;
// One line plus row padding. Wrapped rows replace this estimate as they are measured.
const LOG_ROW_HEIGHT_HINT: Pixels = px(22.);

#[derive(Clone)]
struct LogRow {
    key: SharedString,
    entry: LogEntry,
    text: std::cell::OnceCell<(SharedString, SharedString, SharedString)>,
}

fn same_entry(a: &LogEntry, b: &LogEntry) -> bool {
    a.timestamp == b.timestamp
        && a.unix_ms == b.unix_ms
        && a.level == b.level
        && a.stream == b.stream
        && a.message == b.message
}

fn filter_entries(
    entries: &[LogEntry],
    filter: &LogFilter,
    query: &str,
    limit: usize,
) -> Vec<LogEntry> {
    let query = query.trim().to_ascii_lowercase();
    let mut out: Vec<LogEntry> = entries
        .iter()
        .rev()
        .filter(|entry| filter.matches(entry))
        .filter(|entry| query.is_empty() || entry.message.to_ascii_lowercase().contains(&query))
        .take(limit)
        .cloned()
        .collect();
    out.reverse();
    out
}

impl LogListState {
    pub(super) fn new() -> Self {
        Self {
            sharing: None,
            revision: None,
            frozen_entries: None,
            list: ListState::new(0, ListAlignment::Top, px(128.))
                .with_uniform_item_height(LOG_ROW_HEIGHT_HINT),
            rows: Rc::new(Vec::new()),
            filter: LogFilter::default(),
            query: String::new(),
            newest_first: true,
        }
    }

    pub(super) fn paused(&self) -> bool {
        self.frozen_entries.is_some()
    }

    fn pause(&mut self, entries: Vec<LogEntry>) {
        self.frozen_entries = Some(entries);
    }

    fn resume(&mut self) {
        self.frozen_entries = None;
        self.revision = None;
    }

    pub(super) fn clear(&mut self) {
        if let Some(frozen) = &mut self.frozen_entries {
            frozen.clear();
        }
        self.revision = None;
        self.rows = Rc::new(Vec::new());
        self.reset_at_latest(0);
    }

    fn sync(
        &mut self,
        revision: u64,
        filter: &LogFilter,
        query: String,
        fetch_live: impl FnOnce(&str) -> Vec<LogEntry>,
    ) {
        let filter_changed = &self.filter != filter || self.query != query;
        if let Some(frozen) = self.frozen_entries.as_deref() {
            if filter_changed {
                let logs = filter_entries(frozen, filter, &query, MAX_VISIBLE_LOGS);
                self.update(logs, filter, query);
            }
            return;
        }
        if self.revision != Some(revision) || filter_changed {
            let logs = fetch_live(&query);
            self.update(logs, filter, query);
            self.revision = Some(revision);
        }
    }

    fn toggle_order(&mut self) {
        self.newest_first = !self.newest_first;
        Rc::make_mut(&mut self.rows).reverse();
        self.reset_at_latest(self.rows.len());
    }

    fn reset_at_latest(&self, count: usize) {
        self.list
            .reset_with_uniform_height(count, LOG_ROW_HEIGHT_HINT);
        self.list.set_follow_mode(if self.newest_first {
            FollowMode::Normal
        } else {
            FollowMode::Tail
        });
    }

    fn update(&mut self, logs: Vec<LogEntry>, filter: &LogFilter, query: String) {
        let filter_changed = &self.filter != filter || self.query != query;
        if !filter_changed
            && self.rows.len() == logs.len()
            && logs.iter().enumerate().all(|(i, entry)| {
                let index = if self.newest_first {
                    logs.len() - 1 - i
                } else {
                    i
                };
                same_entry(&self.rows[index].entry, entry)
            })
        {
            return;
        }

        let mut occurrences = HashMap::new();
        let mut rows: Vec<_> = logs
            .into_iter()
            .map(|entry| {
                let mut hasher = DefaultHasher::new();
                (
                    &entry.timestamp,
                    entry.unix_ms,
                    entry.level.as_str(),
                    entry.stream.label(),
                    &entry.message,
                )
                    .hash(&mut hasher);
                let hash = hasher.finish();
                let occurrence = occurrences.entry(hash).or_insert(0usize);
                let key = format!("log-{hash:x}-{occurrence}").into();
                *occurrence += 1;
                LogRow {
                    key,
                    entry,
                    text: Default::default(),
                }
            })
            .collect();
        if self.newest_first {
            rows.reverse();
        }

        if filter_changed {
            self.filter.clone_from(filter);
            self.reset_at_latest(rows.len());
        } else {
            // Keep measurements and the reading position of unchanged rows.
            let top = self.list.logical_scroll_top();
            let prefix = self
                .rows
                .iter()
                .zip(&rows)
                .take_while(|(a, b)| same_entry(&a.entry, &b.entry))
                .count();
            let suffix = self.rows[prefix..]
                .iter()
                .rev()
                .zip(rows[prefix..].iter().rev())
                .take_while(|(a, b)| same_entry(&a.entry, &b.entry))
                .count();
            let inserted = rows.len() - prefix - suffix;
            self.list.splice(prefix..self.rows.len() - suffix, inserted);
            if self.list.is_following_tail() {
                self.list.scroll_to_end();
            } else if self.newest_first && top == ListOffset::default() {
                self.list.scroll_to(ListOffset::default());
            } else if let Some(anchor) = self.rows.get(top.item_ix) {
                if let Some(item_ix) = rows.iter().position(|row| row.key == anchor.key) {
                    self.list.scroll_to(ListOffset { item_ix, ..top });
                }
            }
        }
        self.rows = Rc::new(rows);
        self.query = query;
    }
}

impl AppView {
    fn toggle_log_pause(&mut self, cx: &mut Context<Self>) {
        if self.log_list.paused() {
            self.log_list.resume();
            self.refresh_snapshot();
        } else {
            let entries = self.service.logs().recent(usize::MAX, LogLevel::Trace, "");
            self.log_list.pause(entries);
        }
        cx.notify();
    }

    pub(super) fn can_share_logs(&self, action: LogAction) -> bool {
        self.log_list.sharing.is_none()
            && (action == LogAction::Export || !self.log_list.rows.is_empty())
    }

    pub(super) fn share_logs(
        &mut self,
        action: LogAction,
        privacy: LogPrivacy,
        cx: &mut Context<Self>,
    ) {
        if !self.can_share_logs(action) {
            return;
        }
        let selected = (action == LogAction::Copy).then(|| {
            self.log_list
                .rows
                .iter()
                .map(|row| row.entry.clone())
                .collect::<Vec<_>>()
        });
        self.log_list.sharing = Some(action);
        cx.notify();
        let service = self.service.clone();
        let work = cx.background_executor().spawn(async move {
            match selected {
                Some(rows) => service.logs_for_clipboard(&rows, privacy).map(Some),
                None => service.export_logs_with_privacy(privacy).map(|_| None),
            }
        });
        cx.spawn(async move |view, cx| {
            let result = work.await;
            let _ = view.update(cx, |this, cx| {
                this.log_list.sharing = None;
                match result {
                    Ok(Some(text)) => {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        let label = match privacy {
                            LogPrivacy::Original => {
                                this.tr("已复制原始日志", "Original logs copied")
                            }
                            LogPrivacy::Redacted => {
                                this.tr("已复制脱敏日志", "Redacted logs copied")
                            }
                        };
                        this.service
                            .local_toast(ToastLevel::Success, label.to_string());
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let label = match action {
                            LogAction::Copy => this.tr("复制日志失败", "Could not copy logs"),
                            LogAction::Export => this.tr("导出日志失败", "Could not export logs"),
                        };
                        this.service
                            .toast(ToastLevel::Error, format!("{label}: {err}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn page_logs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let query = self.log_search.read(cx).value().to_string();
        let filter = &self.log_filter;
        let service = &self.service;
        // Sample arriving logs on the data poll, independently of scroll frame rate.
        self.log_list
            .sync(self.log_revision, filter, query, |query| {
                service.logs_matching(filter, query, MAX_VISIBLE_LOGS)
            });
        let rows = self.log_list.rows.clone();
        let share_tip = self.tr(
            "日志中可能包含敏感信息，请谨慎分享原始日志",
            "Logs may contain sensitive information. Take care when sharing original logs.",
        );
        let mono = cx.theme().mono_font_family.clone();

        let mut body = div().flex().flex_col().gap_4().size_full().min_h(px(0.));

        body = body.child(
            card(&p)
                .flex_shrink_0()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .flex_wrap()
                        .child(div().flex_1().min_w(px(220.)).child(section_title(
                            &p,
                            self.tr("实时日志", "Live logs"),
                            self.tr(
                                "按来源、级别和关键词筛选运行记录。日志中可能包含敏感信息，请谨慎将日志原文分享给他人。",
                                "Filter runtime logs by source, level, and keyword. Logs may contain sensitive information. Take care when sharing original logs with others.",
                            ),
                        )))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("copy-logs")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .icon(IconName::Copy)
                                        .label(self.tr("复制日志", "Copy logs"))
                                        .tooltip(share_tip.clone())
                                        .loading(self.log_list.sharing == Some(LogAction::Copy))
                                        .disabled(!self.can_share_logs(LogAction::Copy))
                                        .dropdown_menu(menus::builder(cx.entity().downgrade(), MenuTarget::Logs(LogAction::Copy))),
                                )
                                .child(
                                    Button::new("export-logs")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("导出", "Export"))
                                        .tooltip(share_tip)
                                        .loading(self.log_list.sharing == Some(LogAction::Export))
                                        .disabled(!self.can_share_logs(LogAction::Export))
                                        .dropdown_menu(menus::builder(cx.entity().downgrade(), MenuTarget::Logs(LogAction::Export))),
                                )
                                .child(
                                    Button::new("clear-logs")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .ghost()
                                        .label(self.tr("清空", "Clear"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.run_command(Command::ClearLogs, false, window, cx)
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .flex_wrap()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .child(Input::new(&self.log_search)),
                        )
                        .child(self.log_source_filter(p, cx))
                        .child(self.log_level_filter(p, cx)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(div().flex_1().min_w(px(0.)).text_xs().child(format!(
                            "{} {}",
                            rows.len(),
                            self.tr("条", "entries")
                        )))
                        .child(
                            toolbar_icon_button(
                                "open-log-dir",
                                IconName::FolderOpen,
                                self.tr("打开日志目录", "Open log folder"),
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, _| {
                                this.service.open_url(&format!(
                                    "file://{}",
                                    this.snapshot.platform.log_dir
                                ));
                            })),
                        )
                        .child({
                            let paused = self.log_list.paused();
                            icon_button_toggled(
                                toolbar_icon_button(
                                    "logs-pause-toggle",
                                    if paused {
                                        IconName::Play
                                    } else {
                                        IconName::Pause
                                    },
                                    if paused {
                                        self.tr("恢复刷新日志", "Resume log updates")
                                    } else {
                                        self.tr("暂停刷新日志", "Pause log updates")
                                    },
                                    cx,
                                ),
                                paused,
                                cx,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_log_pause(cx);
                            }))
                        })
                        .child(
                            toolbar_icon_button(
                                "logs-scroll-top",
                                IconName::ArrowUpToLine,
                                self.tr("滚动到顶部", "Scroll to top"),
                                cx,
                            )
                            .disabled(rows.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.log_list.list.scroll_to(ListOffset::default());
                                cx.notify();
                            })),
                        )
                        .child(
                            toolbar_icon_button(
                                "logs-scroll-bottom",
                                IconName::ArrowDownToLine,
                                self.tr("滚动到底部", "Scroll to bottom"),
                                cx,
                            )
                            .disabled(rows.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.log_list.newest_first {
                                    this.log_list.list.scroll_to_end();
                                } else {
                                    this.log_list.list.set_follow_mode(FollowMode::Tail);
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            Button::new("logs-order")
                                .ghost()
                                .label(if self.log_list.newest_first {
                                    self.tr("最新日志在前", "Newest first")
                                } else {
                                    self.tr("最新日志在尾", "Newest last")
                                })
                                .tooltip(self.tr("切换日志顺序", "Reverse log order"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.log_list.toggle_order();
                                    cx.notify();
                                })),
                        ),
                ),
        );

        let mut list = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .p_3()
            .w_full()
            .overflow_hidden()
            .rounded_md()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .font_family(mono);

        if rows.is_empty() {
            list = list.child(empty_hint(
                &p,
                if self.log_filter.levels.is_empty() {
                    self.tr("请选择至少一个日志级别", "Select at least one log level")
                } else {
                    self.tr("没有匹配的日志", "No matching logs")
                },
            ));
        } else {
            // A bounded viewport is required: an outer scrolling Div would lay
            // out all 400 rows, including their selectable text, on every poll.
            let viewport = gpui_kit::list(self.log_list.list.clone(), move |index, _, _| {
                log_row(&rows[index], p).into_any_element()
            })
            .flex_1()
            .w_full();
            list = list.child(scroll_frame(
                "logs-scrollbar",
                &self.log_list.list,
                viewport,
            ));
        }

        body = body.child(list);
        body
    }

    fn log_source_filter(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let options = [
            (LogSourceFilter::All, self.tr("全部", "All")),
            (LogSourceFilter::App, "App".into()),
            (LogSourceFilter::Core, "Core".into()),
        ];
        let selected = options
            .iter()
            .position(|(source, _)| *source == self.log_filter.source);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(self.tr("来源", "Source")),
            )
            .child(
                ChoiceGroup::new(
                    "log-source",
                    options.iter().map(|(_, label)| label.clone()).collect(),
                    selected,
                )
                .on_change(cx.listener(move |this, index: &usize, _, cx| {
                    this.log_filter.source = options[*index].0;
                    cx.notify();
                })),
            )
    }

    fn log_level_filter(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let levels = [
            (LogLevel::Trace, "Trace"),
            (LogLevel::Debug, "Debug"),
            (LogLevel::Info, "Info"),
            (LogLevel::Warn, "Warn"),
            (LogLevel::Error, "Error"),
        ];
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .mr_1()
                    .text_color(p.muted_fg)
                    .child(self.tr("级别（多选）", "Levels (multi-select)")),
            )
            .children(levels.into_iter().map(|(level, label)| {
                let selected = self.log_filter.levels.contains(&level);
                let view = cx.entity().downgrade();
                gpui_base::Toggle::new((ElementId::from("log-level"), label))
                    .pressed(selected)
                    .accessibility_label(label)
                    .h(px(super::theme::ICON_BUTTON_SIZE))
                    .px_2()
                    .rounded_full()
                    .border_1()
                    .border_color(if selected { p.primary } else { p.border })
                    .bg(if selected { p.accent } else { p.panel })
                    .text_color(if selected { p.primary } else { p.muted_fg })
                    .text_sm()
                    .cursor_pointer()
                    .hover(move |style| style.border_color(p.primary))
                    .focus_visible(move |style| style.border_2().border_color(p.primary))
                    .child(label)
                    .on_change(move |pressed, _, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if pressed {
                                this.log_filter.levels.insert(level);
                            } else {
                                this.log_filter.levels.remove(&level);
                            }
                            cx.notify();
                        });
                    })
            }))
    }
}

fn log_time_in_timezone(timestamp: &str, timezone: &impl chrono::TimeZone) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|time| {
            time.with_timezone(timezone)
                .time()
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| timestamp.to_owned())
}

/// Shortens the visible message while keeping the captured entry available for diagnostics.
fn displayed_log_message(entry: &LogEntry) -> &str {
    let message = entry.message.as_str();
    if entry.stream == LogStream::App {
        return message;
    }
    let has_offset = message.starts_with(['+', '-']);
    let remainder = if has_offset {
        // sing-box full timestamp, including the numeric UTC offset.
        chrono::DateTime::parse_and_remainder(message, "%z %Y-%m-%d %H:%M:%S%.f")
            .map(|(_, rest)| rest)
    } else {
        // Xray's Go logger uses slash-separated dates and optional microseconds.
        chrono::NaiveDateTime::parse_and_remainder(message, "%Y/%m/%d %H:%M:%S%.f")
            .map(|(_, rest)| rest)
    };
    let Some(body) = remainder.ok().and_then(|rest| rest.strip_prefix(' ')) else {
        return message;
    };
    let body = body.trim_start();
    if body.is_empty() {
        return message;
    }
    if let Some((token, details)) = body.split_once(' ') {
        let duplicate_level = if has_offset {
            token.eq_ignore_ascii_case(entry.level.singbox())
        } else {
            token
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .is_some_and(|level| level.eq_ignore_ascii_case(entry.level.xray()))
        };
        let details = details.trim_start();
        if duplicate_level && !details.is_empty() {
            return details;
        }
    }
    body
}

fn log_row(row: &LogRow, p: Palette) -> impl IntoElement {
    let entry = &row.entry;
    let key = &row.key;
    let color = match entry.level {
        LogLevel::Trace | LogLevel::Debug => p.muted_fg,
        LogLevel::Info => p.fg,
        LogLevel::Warn => p.warn_text,
        LogLevel::Error => p.danger_text,
    };
    let (timestamp, level, message) = row.text.get_or_init(|| {
        (
            log_time_in_timezone(&entry.timestamp, &chrono::Local).into(),
            entry.level.as_str().to_uppercase().into(),
            displayed_log_message(entry).to_owned().into(),
        )
    });
    div()
        .id(key.clone())
        .flex()
        .items_start()
        .gap_2()
        .pb_1()
        .w_full()
        .min_w(px(0.))
        .overflow_hidden()
        .text_xs()
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_color(p.muted_fg)
                .child(SelectableText::new("time", timestamp.clone())),
        )
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_color(color)
                .child(SelectableText::new("level", level.clone())),
        )
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_color(p.muted_fg)
                .child(SelectableText::new("stream", entry.stream.source_label())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_color(color)
                .whitespace_normal()
                .child(SelectableText::new("message", message.clone())),
        )
}

#[cfg(test)]
mod tests {
    use super::{LogFilter, LogListState, LogSourceFilter};
    use gpui_kit::{ListOffset, px};
    use std::rc::Rc;
    use xrose_core::core::LogEntry;
    use xrose_core::core::LogStream;
    use xrose_core::settings::LogLevel;

    #[test]
    fn log_time_converts_utc_and_explicit_offsets_to_the_viewers_timezone() {
        let east = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        for timestamp in [
            "2026-11-06T18:44:47Z",
            "2026-11-06T18:44:47.123+00:00",
            "2026-11-07T02:44:47+08:00",
            "2026-11-06T14:44:47-04:00",
        ] {
            assert_eq!(super::log_time_in_timezone(timestamp, &east), "02:44:47");
        }
    }

    #[test]
    fn log_time_handles_negative_and_half_hour_offsets() {
        let timestamp = "2026-11-07T02:10:05+08:00";
        let west = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let half_hour = chrono::FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
        assert_eq!(super::log_time_in_timezone(timestamp, &west), "13:10:05");
        assert_eq!(
            super::log_time_in_timezone(timestamp, &half_hour),
            "23:40:05"
        );
    }

    #[test]
    fn log_time_preserves_unparseable_timestamps() {
        for timestamp in ["", "unknown", "2026-11-07 02:44:47"] {
            assert_eq!(
                super::log_time_in_timezone(timestamp, &chrono::Utc),
                timestamp
            );
        }
    }

    fn entries(range: std::ops::Range<i64>) -> Vec<LogEntry> {
        range
            .map(|unix_ms| LogEntry {
                timestamp: "2026-11-03T00:00:00Z".into(),
                unix_ms,
                level: LogLevel::Info,
                stream: LogStream::Core,
                message: format!("test log {unix_ms}"),
            })
            .collect()
    }

    #[test]
    fn core_display_compacts_timestamps_and_keeps_context() {
        let mut entry = entries(0..1).pop().unwrap();
        for (stream, level, raw, expected) in [
            (
                LogStream::Stderr,
                LogLevel::Info,
                "+0800 2026-01-02 12:34:56 INFO [42 0ms] inbound: accepted",
                "[42 0ms] inbound: accepted",
            ),
            (
                LogStream::Core,
                LogLevel::Warn,
                "-0530 2026-01-02 12:34:56 WARN [42 17ms] retry",
                "[42 17ms] retry",
            ),
            (
                LogStream::Stdout,
                LogLevel::Warn,
                "2026/01/02 12:34:56.123456 [Warning] [42] retry",
                "[42] retry",
            ),
            (
                LogStream::Stdout,
                LogLevel::Info,
                "2026/01/02 12:34:56 accepted example.test",
                "accepted example.test",
            ),
            (
                LogStream::Stderr,
                LogLevel::Error,
                "+0000 2026-01-02 12:34:56 FATAL failed",
                "FATAL failed",
            ),
            (
                LogStream::Stdout,
                LogLevel::Error,
                "2026/01/02 12:34:56 error while opening file",
                "error while opening file",
            ),
        ] {
            entry.stream = stream;
            entry.level = level;
            entry.message = raw.into();
            assert_eq!(super::displayed_log_message(&entry), expected);
        }
    }

    #[test]
    fn display_preserves_app_messages_and_unrecognized_prefixes() {
        let mut entry = entries(0..1).pop().unwrap();
        for (stream, raw) in [
            (LogStream::App, "+0800 2026-01-02 12:34:56 INFO scheduled"),
            (
                LogStream::Stderr,
                "+0800 2026-02-30 12:34:56 INFO invalid date",
            ),
            (LogStream::Core, "2026/01/02 12:34:56"),
            (LogStream::Core, "2026/01/02 12:34:56suffix"),
            (LogStream::Stdout, "[42 17ms] event at 2026/01/02 12:34:56"),
        ] {
            entry.stream = stream;
            entry.message = raw.into();
            assert_eq!(super::displayed_log_message(&entry), raw);
        }
    }

    #[test]
    fn unchanged_poll_reuses_rows_and_scroll_position() {
        let mut state = LogListState::new();
        state.update(entries(0..400), &LogFilter::default(), String::new());
        let rows = state.rows.clone();
        let top = ListOffset {
            item_ix: 120,
            offset_in_item: px(7.),
        };
        state.list.scroll_to(top);
        state.update(entries(0..400), &LogFilter::default(), String::new());
        assert!(Rc::ptr_eq(&rows, &state.rows));
        assert_eq!(state.list.logical_scroll_top(), top);
    }

    #[test]
    fn appended_rows_contribute_to_scroll_extent_before_paint() {
        let mut state = LogListState::new();
        state.toggle_order();
        state.update(entries(0..10), &LogFilter::default(), String::new());
        state.update(entries(0..30), &LogFilter::default(), String::new());
        state.list.scroll_to(ListOffset::default());
        state.list.scroll_by(super::LOG_ROW_HEIGHT_HINT * 24.);
        assert_eq!(state.list.logical_scroll_top().item_ix, 24);
    }

    #[test]
    fn rolling_window_keeps_the_log_being_read() {
        let mut state = LogListState::new();
        state.update(entries(0..400), &LogFilter::default(), String::new());
        let key = state.rows[120].key.clone();
        state.list.scroll_to(ListOffset {
            item_ix: 120,
            offset_in_item: px(7.),
        });
        state.update(entries(10..410), &LogFilter::default(), String::new());
        let top = state.list.logical_scroll_top();
        assert_eq!(state.list.item_count(), 400);
        assert_eq!(state.rows[top.item_ix].key, key);
        assert_eq!(top.offset_in_item, px(7.));
    }

    #[test]
    fn newest_logs_follow_at_top_and_filter_changes_reset_scroll() {
        let mut state = LogListState::new();
        state.update(entries(0..10), &LogFilter::default(), String::new());
        state.update(entries(0..12), &LogFilter::default(), String::new());
        assert_eq!(state.rows[0].entry.unix_ms, 11);
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
        state.list.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(3.),
        });
        state.update(entries(0..12), &LogFilter::default(), "test".into());
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
        state.list.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(3.),
        });
        let mut filter = LogFilter {
            source: LogSourceFilter::Core,
            ..LogFilter::default()
        };
        state.update(entries(0..12), &filter, "test".into());
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
        state.list.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(3.),
        });
        filter.levels.clear();
        state.update(Vec::new(), &filter, "test".into());
        assert!(state.rows.is_empty());
        assert_eq!(state.list.item_count(), 0);
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
    }

    #[test]
    fn newest_last_follows_appends_until_the_reader_scrolls_away() {
        let mut state = LogListState::new();
        let filter = LogFilter::default();
        state.update(entries(0..400), &filter, String::new());
        let latest = state.rows[0].key.clone();
        state.toggle_order();
        assert_eq!(state.rows.last().unwrap().key, latest);
        state.update(entries(1..401), &filter, String::new());
        assert_eq!(state.rows.last().unwrap().entry.unix_ms, 400);
        assert!(state.list.is_following_tail());

        state.list.scroll_to(ListOffset {
            item_ix: 12,
            offset_in_item: px(3.),
        });
        let anchor = state.rows[12].key.clone();
        state.update(entries(2..402), &filter, String::new());
        let top = state.list.logical_scroll_top();
        assert_eq!(state.rows[top.item_ix].key, anchor);
        assert_eq!(top.offset_in_item, px(3.));
        assert!(!state.list.is_following_tail());

        state.update(entries(2..402), &filter, "test".into());
        assert!(state.list.is_following_tail());
        state.toggle_order();
        assert_eq!(state.rows[0].entry.unix_ms, 401);
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
    }

    #[test]
    fn duplicate_logs_have_distinct_stable_selection_ids() {
        let mut state = LogListState::new();
        let entry = entries(0..1).pop().unwrap();
        state.update(
            vec![entry.clone(), entry.clone()],
            &LogFilter::default(),
            String::new(),
        );
        assert_ne!(state.rows[0].key, state.rows[1].key);
        let keys: Vec<_> = state.rows.iter().map(|row| row.key.clone()).collect();
        let mut logs = vec![entry.clone(), entry];
        logs.extend(entries(1..2));
        state.update(logs, &LogFilter::default(), String::new());
        assert_eq!(state.rows[1].key, keys[0]);
        assert_eq!(state.rows[2].key, keys[1]);
    }

    #[test]
    fn pause_freezes_live_updates_and_filters_snapshot_until_resume() {
        let mut state = LogListState::new();
        let filter = LogFilter::default();
        state.sync(1, &filter, String::new(), |_| entries(0..5));
        assert!(!state.paused());
        assert_eq!(state.rows.len(), 5);
        assert_eq!(state.rows[0].entry.unix_ms, 4);

        state.pause(entries(0..5));
        assert!(state.paused());

        // New revisions must not overwrite the paused UI list.
        let mut fetched_while_paused = false;
        state.sync(2, &filter, String::new(), |_| {
            fetched_while_paused = true;
            entries(0..10)
        });
        assert!(!fetched_while_paused);
        assert_eq!(state.rows.len(), 5);
        assert_eq!(state.rows[0].entry.unix_ms, 4);

        // Filtering while paused operates only on the frozen snapshot.
        state.sync(2, &filter, "log 3".into(), |_| {
            panic!("paused filtering must not pull live logs");
        });
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].entry.unix_ms, 3);

        state.sync(2, &filter, String::new(), |_| {
            panic!("paused filtering must not pull live logs");
        });
        assert_eq!(state.rows.len(), 5);
        assert_eq!(state.rows[0].entry.unix_ms, 4);

        // Clearing while paused keeps the paused state empty until resumed.
        state.clear();
        assert!(state.paused());
        assert!(state.rows.is_empty());
        state.sync(3, &filter, String::new(), |_| {
            panic!("cleared paused view must stay frozen until resumed");
        });
        assert!(state.rows.is_empty());

        state.resume();
        assert!(!state.paused());
        state.sync(3, &filter, String::new(), |_| entries(0..10));
        assert_eq!(state.rows.len(), 10);
        assert_eq!(state.rows[0].entry.unix_ms, 9);
    }
}
