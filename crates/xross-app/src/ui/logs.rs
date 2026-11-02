use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui_base::SelectableText;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use xross_core::settings::LogLevel;

use super::theme::palette;
use super::widgets::*;
use super::AppView;

impl AppView {
    pub(crate) fn page_logs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let query = self.log_search.read(cx).value().to_string();
        let logs = self.service.logs_filtered(self.log_level, &query, 400);
        let mono = cx.theme().mono_font_family.clone();

        let mut body = div().flex().flex_col().gap_4().w_full();

        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
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
                            logs.len(),
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
            .gap_1()
            .p_3()
            .w_full()
            .overflow_hidden()
            .rounded_md()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .font_family(mono);

        if logs.is_empty() {
            list = list.child(empty_hint(
                &p,
                self.tr("没有匹配的日志", "No matching logs"),
            ));
        }
        let mut occurrences = HashMap::new();
        for entry in logs.iter().rev() {
            let mut hasher = DefaultHasher::new();
            (
                &entry.timestamp,
                entry.level.as_str(),
                entry.stream.label(),
                &entry.message,
            )
                .hash(&mut hasher);
            let hash = hasher.finish();
            let occurrence = occurrences.entry(hash).or_insert(0usize);
            let key = format!("log-{hash:x}-{occurrence}");
            *occurrence += 1;
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
            list = list.child(
                div()
                    .id(SharedString::from(key.clone()))
                    .flex()
                    .items_start()
                    .gap_2()
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
                    .child(div().w(px(48.)).flex_shrink_0().text_color(color).child(
                        SelectableText::new(
                            SharedString::from(format!("{key}-level")),
                            entry.level.as_str().to_uppercase(),
                        ),
                    ))
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
                    ),
            );
        }

        body = body.child(list);
        let _ = lang;
        body
    }
}
