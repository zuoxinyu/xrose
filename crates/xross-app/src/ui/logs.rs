use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use gpui_base::SelectableText;
use gpui_kit::assets::IconName;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::*;
use xross_core::core::LogEntry;
use xross_core::settings::LogLevel;

use super::AppView;
use super::theme::{Palette, palette};
use super::widgets::*;

pub(super) struct LogListState {
    list: ListState,
    rows: Rc<Vec<LogRow>>,
    level: LogLevel,
    query: String,
}

struct LogRow {
    key: SharedString,
    entry: LogEntry,
}

fn same_entry(a: &LogEntry, b: &LogEntry) -> bool {
    a.timestamp == b.timestamp
        && a.unix_ms == b.unix_ms
        && a.level == b.level
        && a.stream == b.stream
        && a.message == b.message
}

impl LogListState {
    pub(super) fn new() -> Self {
        Self {
            list: ListState::new(0, ListAlignment::Top, px(128.)),
            rows: Rc::new(Vec::new()),
            level: LogLevel::Info,
            query: String::new(),
        }
    }

    fn update(&mut self, logs: Vec<LogEntry>, level: LogLevel, query: String) {
        let filter_changed = self.level != level || self.query != query;
        if !filter_changed
            && self.rows.len() == logs.len()
            && self
                .rows
                .iter()
                .rev()
                .zip(&logs)
                .all(|(row, entry)| same_entry(&row.entry, entry))
        {
            return;
        }

        let mut occurrences = HashMap::new();
        let rows: Vec<_> = logs
            .into_iter()
            .rev()
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
                LogRow { key, entry }
            })
            .collect();

        if filter_changed {
            self.list.reset(rows.len());
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
            self.list.splice(
                prefix..self.rows.len() - suffix,
                rows.len() - prefix - suffix,
            );
            if top == ListOffset::default() {
                self.list.scroll_to(ListOffset::default());
            } else if let Some(anchor) = self.rows.get(top.item_ix) {
                if let Some(item_ix) = rows.iter().position(|row| row.key == anchor.key) {
                    self.list.scroll_to(ListOffset { item_ix, ..top });
                }
            }
        }
        self.rows = Rc::new(rows);
        self.level = level;
        self.query = query;
    }
}

impl AppView {
    pub(crate) fn page_logs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let query = self.log_search.read(cx).value().to_string();
        let logs = self.service.logs_filtered(self.log_level, &query, 400);
        self.log_list.update(logs, self.log_level, query);
        let rows = self.log_list.rows.clone();
        let mono = cx.theme().mono_font_family.clone();

        let mut body = div().flex().flex_col().gap_4().size_full().min_h(px(0.));

        body = body.child(
            card(&p)
                .flex_shrink_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .flex_wrap()
                        .child(section_title(
                            &p,
                            self.tr("实时日志", "Live logs"),
                            self.tr(
                                "实时收集 core 的 stdout/stderr，并按大小滚动写入日志文件",
                                "Collects core stdout/stderr and rotates a log file by size",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    Button::new("copy-logs")
                                        .compact()
                                        .icon(IconName::Copy)
                                        .label(self.tr("复制日志", "Copy logs"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let query =
                                                this.log_search.read(cx).value().to_string();
                                            let logs = this.service.logs_filtered(
                                                this.log_level,
                                                &query,
                                                400,
                                            );
                                            let text = logs
                                                .iter()
                                                .rev()
                                                .map(|entry| {
                                                    format!(
                                                        "{} {} {} {}",
                                                        entry.timestamp,
                                                        entry.level.as_str().to_uppercase(),
                                                        entry.stream.label(),
                                                        entry.message
                                                    )
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        })),
                                )
                                .child(
                                    Button::new("export-logs")
                                        .compact()
                                        .label(self.tr("导出", "Export"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            let _ = this.service.export_logs();
                                        })),
                                )
                                .child(
                                    Button::new("clear-logs")
                                        .compact()
                                        .ghost()
                                        .label(self.tr("清空", "Clear"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.service.clear_logs();
                                            this.sync(cx);
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
                                .min_w(px(220.))
                                .child(Input::new(&self.log_search)),
                        )
                        .children(
                            [
                                (LogLevel::Trace, "Trace"),
                                (LogLevel::Debug, "Debug"),
                                (LogLevel::Info, "Info"),
                                (LogLevel::Warn, "Warn"),
                                (LogLevel::Error, "Error"),
                            ]
                            .into_iter()
                            .map(|(level, label)| {
                                let selected = self.log_level == level;
                                div()
                                    .id(SharedString::from(format!("loglevel-{label}")))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .border_1()
                                    .border_color(if selected { p.primary } else { p.border })
                                    .bg(if selected { p.accent } else { p.bg })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.log_level = level;
                                        cx.notify();
                                    }))
                                    .child(label)
                            }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(div().flex_1().min_w(px(0.)).truncate().child(format!(
                            "{} {} · {}：{}",
                            rows.len(),
                            self.tr("条", "entries"),
                            self.tr("日志目录", "log dir"),
                            self.snapshot.platform.log_dir
                        )))
                        .child(
                            div()
                                .flex_shrink_0()
                                .child(self.tr("最新日志在前", "Newest first")),
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
                self.tr("没有匹配的日志", "No matching logs"),
            ));
        } else {
            // A bounded viewport is required: an outer scrolling Div would lay
            // out all 400 rows, including their selectable text, on every poll.
            list = list.child(
                gpui_kit::list(self.log_list.list.clone(), move |index, _, _| {
                    log_row(&rows[index], p).into_any_element()
                })
                .flex_1()
                .w_full(),
            );
        }

        body = body.child(list);
        body
    }
}

fn log_row(row: &LogRow, p: Palette) -> impl IntoElement {
    let entry = &row.entry;
    let key = &row.key;
    let color = match entry.level {
        LogLevel::Trace | LogLevel::Debug => p.muted_fg,
        LogLevel::Info => p.fg,
        LogLevel::Warn => p.warning,
        LogLevel::Error => p.danger,
    };
    let timestamp = entry
        .timestamp
        .split('T')
        .nth(1)
        .map(|t| {
            t.trim_end_matches('Z')
                .split('.')
                .next()
                .unwrap_or(t)
                .to_string()
        })
        .unwrap_or_else(|| entry.timestamp.clone());
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
                .child(SelectableText::new(
                    SharedString::from(format!("{key}-time")),
                    timestamp,
                )),
        )
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_color(color)
                .child(SelectableText::new(
                    SharedString::from(format!("{key}-level")),
                    entry.level.as_str().to_uppercase(),
                )),
        )
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_color(p.muted_fg)
                .child(SelectableText::new(
                    SharedString::from(format!("{key}-stream")),
                    entry.stream.label(),
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_color(color)
                .whitespace_normal()
                .child(SelectableText::new(
                    SharedString::from(format!("{key}-message")),
                    entry.message.clone(),
                )),
        )
}

#[cfg(test)]
mod tests {
    use super::LogListState;
    use gpui_kit::{ListOffset, px};
    use std::rc::Rc;
    use xross_core::core::LogEntry;
    use xross_core::core::LogStream;
    use xross_core::settings::LogLevel;

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
    fn unchanged_poll_reuses_rows_and_scroll_position() {
        let mut state = LogListState::new();
        state.update(entries(0..400), LogLevel::Info, String::new());
        let rows = state.rows.clone();
        let top = ListOffset {
            item_ix: 120,
            offset_in_item: px(7.),
        };
        state.list.scroll_to(top);
        state.update(entries(0..400), LogLevel::Info, String::new());
        assert!(Rc::ptr_eq(&rows, &state.rows));
        assert_eq!(state.list.logical_scroll_top(), top);
    }

    #[test]
    fn rolling_window_keeps_the_log_being_read() {
        let mut state = LogListState::new();
        state.update(entries(0..400), LogLevel::Info, String::new());
        let key = state.rows[120].key.clone();
        state.list.scroll_to(ListOffset {
            item_ix: 120,
            offset_in_item: px(7.),
        });
        state.update(entries(10..410), LogLevel::Info, String::new());
        let top = state.list.logical_scroll_top();
        assert_eq!(state.list.item_count(), 400);
        assert_eq!(state.rows[top.item_ix].key, key);
        assert_eq!(top.offset_in_item, px(7.));
    }

    #[test]
    fn newest_logs_follow_at_top_and_filter_changes_reset_scroll() {
        let mut state = LogListState::new();
        state.update(entries(0..10), LogLevel::Info, String::new());
        state.update(entries(0..12), LogLevel::Info, String::new());
        assert_eq!(state.rows[0].entry.unix_ms, 11);
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
        state.list.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(3.),
        });
        state.update(entries(0..12), LogLevel::Info, "test".into());
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
        state.list.scroll_to(ListOffset {
            item_ix: 5,
            offset_in_item: px(3.),
        });
        state.update(Vec::new(), LogLevel::Error, "test".into());
        assert!(state.rows.is_empty());
        assert_eq!(state.list.item_count(), 0);
        assert_eq!(state.list.logical_scroll_top(), ListOffset::default());
    }

    #[test]
    fn duplicate_logs_have_distinct_stable_selection_ids() {
        let mut state = LogListState::new();
        let entry = entries(0..1).pop().unwrap();
        state.update(
            vec![entry.clone(), entry.clone()],
            LogLevel::Info,
            String::new(),
        );
        assert_ne!(state.rows[0].key, state.rows[1].key);
        let keys: Vec<_> = state.rows.iter().map(|row| row.key.clone()).collect();
        let mut logs = vec![entry.clone(), entry];
        logs.extend(entries(1..2));
        state.update(logs, LogLevel::Info, String::new());
        assert_eq!(state.rows[1].key, keys[0]);
        assert_eq!(state.rows[2].key, keys[1]);
    }
}
