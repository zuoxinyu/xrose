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
                            self.tr("日志", "Logs"),
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
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(format!(
                            "{} {} · {}：{}",
                            logs.len(),
                            self.tr("条", "entries"),
                            self.tr("日志目录", "log dir"),
                            self.snapshot.platform.log_dir
                        ))
                        .child(self.tr("最新日志在前", "Newest first")),
                ),
        );

        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
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
        for entry in logs.iter().rev() {
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
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_xs()
                    .child(div().w(px(64.)).text_color(p.muted_fg).child(timestamp))
                    .child(
                        div()
                            .w(px(48.))
                            .text_color(color)
                            .child(entry.level.as_str().to_uppercase()),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .text_color(p.muted_fg)
                            .child(entry.stream.label()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(color)
                            .child(entry.message.clone()),
                    ),
            );
        }

        body = body.child(list);
        let _ = lang;
        body
    }
}
