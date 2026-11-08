//! Direct imports on the nodes and subscriptions pages.
mod decode;
mod finder;
#[cfg(target_os = "linux")]
mod linux;
mod platform;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(windows)]
mod windows;

use super::{AppView, Page, theme, tr};
use decode::{Input, Issue};
use gpui_kit::{
    component::{WindowExt, button::Button},
    *,
};
use xrose_core::service::{SharePreview, SharedImport, ToastLevel};

// A file or image may also advertise its name or URL as plain text. Import
// only the richest representation, not the fallback text as another subscription.
fn inputs_from_clipboard(item: ClipboardItem) -> Vec<Input> {
    let mut files = Vec::new();
    let mut images = Vec::new();
    let mut texts = Vec::new();
    for entry in item.entries {
        match entry {
            ClipboardEntry::ExternalPaths(paths) => {
                files.extend(paths.0.into_iter().map(Input::File))
            }
            ClipboardEntry::Image(image) => images.push(Input::Image(image.bytes)),
            ClipboardEntry::String(s) => texts.push(Input::Text(s.text)),
        }
    }
    if !files.is_empty() {
        files
    } else if !images.is_empty() {
        images
    } else {
        texts
    }
}

impl AppView {
    pub(super) fn accepts_import(&self, window: &mut Window, cx: &mut App) -> bool {
        matches!(self.page, Page::Nodes | Page::Subscriptions) && !window.has_active_dialog(cx)
    }

    pub(super) fn init_import(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        #[cfg(target_os = "macos")]
        {
            let mut rx = platform::install(window);
            cx.spawn_in(window, async move |view, cx| {
                while let Some(input) = rx.recv().await {
                    let _ = view.update_in(cx, |this, window, cx| {
                        if this.accepts_import(window, cx) {
                            this.process_import(vec![input], false, window, cx);
                        }
                    });
                }
            })
            .detach();
        }
    }

    pub(super) fn scan_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("scan-screen-qr")
            .compact()
            .h(px(theme::CONTROL_HEIGHT))
            .icon(gpui_kit::assets::IconName::ScanLine)
            .label(self.tr("扫描屏幕二维码", "Scan screen QR code"))
            .loading(self.scan_pending || self.import_busy)
            .on_click(cx.listener(|this, _, window, cx| this.start_finder(window, cx)))
    }

    pub(super) fn paste_import(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.accepts_import(window, cx) || window.has_focused_input(cx) {
            return false;
        }
        self.read_import_clipboard(false, window, cx);
        true
    }

    pub(super) fn paste_into_import(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.read_import_clipboard(true, window, cx);
        true
    }

    fn read_import_clipboard(&mut self, review: bool, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        match platform::paste_image() {
            Ok(Some(image)) => return self.process_import(vec![image], review, window, cx),
            Err(issue) => {
                let (zh, en) = issue.message();
                return self.import_problem(self.tr(zh, en).to_string(), window, cx);
            }
            Ok(None) => {}
        }
        let read = cx.read_from_clipboard_async();
        let epoch = self.import_editor_epoch;
        cx.spawn_in(window, async move |view, cx| {
            let result = read.await;
            let _ = view.update_in(cx, |this, window, cx| {
                if review && (!this.show_import_box || this.import_editor_epoch != epoch) {
                    return;
                }
                match result {
                    Ok(Some(item)) => {
                        this.process_import(inputs_from_clipboard(item), review, window, cx)
                    }
                    Ok(None) => this.import_problem(
                        this.tr(
                            "剪贴板为空或没有可导入的内容",
                            "Clipboard is empty or contains no importable content.",
                        )
                        .to_string(),
                        window,
                        cx,
                    ),
                    Err(_) => this.import_problem(
                        this.tr(
                            "无法读取剪贴板，请重新复制后再试",
                            "Could not read the clipboard. Copy the content and try again.",
                        )
                        .to_string(),
                        window,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    pub(super) fn drop_content(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.accepts_import(window, cx) {
            self.process_import(inputs_from_clipboard(item.clone()), false, window, cx);
        }
    }

    pub(super) fn drop_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.accepts_import(window, cx) {
            self.process_import(
                paths.paths().iter().cloned().map(Input::File).collect(),
                false,
                window,
                cx,
            );
        }
    }

    pub(super) fn submit_editor_content(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.process_import(vec![Input::Text(text)], false, window, cx);
    }

    fn import_problem(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_import_box || self.show_subscription_form {
            self.manual_error = Some(message);
        } else {
            self.service.toast(ToastLevel::Error, message);
            self.sync(cx);
        }
        window.refresh();
        cx.notify();
    }

    fn process_import(
        &mut self,
        inputs: Vec<Input>,
        review: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lang = self.lang();
        let problem = if inputs.is_empty() {
            Some(Issue::Unsupported)
        } else if inputs.len() > 64 {
            Some(Issue::TooLarge)
        } else {
            None
        };
        if let Some(issue) = problem {
            let (zh, en) = issue.message();
            return self.import_problem(self.tr(zh, en).to_string(), window, cx);
        }
        if self.import_busy {
            return self.import_problem(
                self.tr(
                    "正在导入，请稍后再试",
                    "Import in progress. Try again shortly.",
                )
                .to_string(),
                window,
                cx,
            );
        }
        self.manual_error = None;
        self.import_busy = true;
        window.refresh();
        cx.notify();
        let epoch = self.import_editor_epoch;
        let editor = self.show_import_box || self.show_subscription_form;
        let service = self.service.clone();
        cx.spawn_in(window, async move |view, cx| {
            let (texts, report) = cx
                .background_executor()
                .spawn(async move {
                    let mut texts = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    let mut report = SharedImport::default();
                    for input in inputs {
                        let image = matches!(input, Input::Image(_));
                        match decode::extract(input) {
                            Ok(payloads) => {
                                for text in payloads {
                                    if !seen.insert(text.clone()) {
                                        continue;
                                    }
                                    if review {
                                        if image
                                            && matches!(
                                                xrose_core::AppService::preview_shared_content(
                                                    &text
                                                ),
                                                SharePreview::Unsupported
                                            )
                                        {
                                            report.unreadable = true;
                                        } else {
                                            texts.push(text);
                                        }
                                    } else {
                                        let part = service.import_shared_content(&text);
                                        report.nodes += part.nodes;
                                        report.subscriptions += part.subscriptions;
                                        report.duplicates += part.duplicates;
                                        report.unreadable |= part.unreadable;
                                        report.errors.extend(part.errors);
                                    }
                                }
                            }
                            Err(issue) => {
                                let (zh, en) = issue.message();
                                report.errors.push(tr(lang, zh, en).to_string());
                            }
                        }
                    }
                    (texts, report)
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.import_busy = false;
                let same_editor = editor
                    && (this.show_import_box || this.show_subscription_form)
                    && this.import_editor_epoch == epoch;
                if review && !same_editor {
                    window.refresh();
                    cx.notify();
                    return;
                }
                if review && !texts.is_empty() {
                    this.import_input
                        .update(cx, |input, cx| input.replace(texts.join("\n"), window, cx));
                }
                let mut feedback = Vec::new();
                let added = report.nodes + report.subscriptions;
                if added > 0 {
                    feedback.push(format!(
                        "{} {} · {} {}",
                        this.tr("新增节点", "Nodes added"),
                        report.nodes,
                        this.tr("新增订阅", "Subscriptions added"),
                        report.subscriptions
                    ));
                }
                if report.duplicates > 0 {
                    feedback.push(format!(
                        "{} {}",
                        this.tr("已存在，跳过", "Already present, skipped"),
                        report.duplicates
                    ));
                }
                if report.unreadable {
                    feedback.push(
                        this.tr(
                            "内容无法识别为节点链接或订阅，请检查格式",
                            "Unrecognized node or subscription. Check the format.",
                        )
                        .to_string(),
                    );
                }
                let failed = report.unreadable
                    || !report.errors.is_empty()
                    || (report.duplicates > 0 && added == 0);
                feedback.extend(report.errors);
                if !feedback.is_empty() {
                    let message = feedback.join(" · ");
                    if same_editor && failed {
                        this.manual_error = Some(message);
                    } else {
                        let level = if !failed {
                            ToastLevel::Success
                        } else if added > 0 || report.duplicates > 0 {
                            ToastLevel::Warning
                        } else {
                            ToastLevel::Error
                        };
                        this.service.toast(level, message);
                    }
                }
                if same_editor && !review && added > 0 && !failed {
                    this.import_input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    window.close_dialog(cx);
                    this.clear_node_editor(window, cx);
                }
                this.sync(cx);
                window.refresh();
            });
        })
        .detach();
    }
}

#[cfg(target_os = "macos")]
pub(super) fn enable_drop(enabled: bool) {
    platform::enable_drop(enabled);
}
