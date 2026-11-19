//! Direct imports on the nodes and subscriptions pages.
mod decode;
mod finder;
#[cfg(target_os = "linux")]
mod linux;
mod platform;
#[cfg(target_os = "linux")]
mod portal;
mod sample;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportTarget {
    /// Fill the open "Add node → Links and QR codes" textarea for review.
    NodeImportDialog,
    /// Fill the open "Add subscription" form (URL + auto domain name) for confirmation.
    SubscriptionDialog,
    /// Direct page paste/drop or global QR scan: subscription URLs open the confirmation
    /// dialog instead of saving and fetching unverified third-party links.
    Direct,
    /// Explicit submission of the "Add node → Links and QR codes" textarea.
    SubmitEditor,
}

#[derive(Debug, PartialEq, Eq)]
enum SubscriptionScanOutcome {
    Url { url: String, default_name: String },
    NodeLink,
    Invalid,
}

/// Inspects decoded payloads targeted at the subscription form.
fn classify_subscription_payloads(texts: &[String]) -> SubscriptionScanOutcome {
    let mut saw_node = false;
    for text in texts {
        let inspected = xrose_core::AppService::inspect_shared_content(text);
        if let Some((url, default_name)) = inspected.subscription_urls.into_iter().next() {
            return SubscriptionScanOutcome::Url { url, default_name };
        }
        if matches!(
            xrose_core::AppService::preview_shared_content(text),
            SharePreview::Node { .. }
        ) {
            saw_node = true;
        }
    }
    if saw_node {
        SubscriptionScanOutcome::NodeLink
    } else {
        SubscriptionScanOutcome::Invalid
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DirectImportPlan {
    /// A single subscription URL: always route to the subscription confirmation dialog.
    ConfirmSubscription { url: String, default_name: String },
    /// Multiple subscription URLs or mixed nodes + subscription URLs: review in the import box.
    ReviewMixed(String),
    /// Pure node links/configs or unrecognized text: no third-party URL fetch involved.
    ImportDirect,
}

/// Decides whether direct import / global QR scan needs interactive confirmation before saving.
fn plan_direct_import(texts: &[String]) -> DirectImportPlan {
    let mut subscription_urls = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();
    let mut has_other = false;
    for text in texts {
        let inspected = xrose_core::AppService::inspect_shared_content(text);
        for (url, default_name) in inspected.subscription_urls {
            if seen_urls.insert(url.clone()) {
                subscription_urls.push((url, default_name));
            }
        }
        has_other |= inspected.has_other_content;
    }
    if subscription_urls.is_empty() {
        DirectImportPlan::ImportDirect
    } else if subscription_urls.len() == 1 && !has_other {
        let (url, default_name) = subscription_urls.into_iter().next().unwrap();
        DirectImportPlan::ConfirmSubscription { url, default_name }
    } else {
        DirectImportPlan::ReviewMixed(texts.join("\n"))
    }
}

enum BackgroundImport {
    SubscriptionDialog {
        outcome: SubscriptionScanOutcome,
        errors: Vec<String>,
    },
    NodeImportDialog {
        texts: Vec<String>,
        report: SharedImport,
    },
    ConfirmSubscription {
        url: String,
        default_name: String,
        errors: Vec<String>,
    },
    ReviewMixed {
        content: String,
        errors: Vec<String>,
    },
    Applied {
        report: SharedImport,
    },
}

impl AppView {
    pub(super) fn accepts_import(&self, window: &mut Window, cx: &mut App) -> bool {
        matches!(self.page, Page::Nodes | Page::Subscriptions) && !window.has_active_dialog(cx)
    }

    pub(super) fn init_import(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        // Dialogs can own keyboard focus outside the AppView's dispatch path.
        let main = window.window_handle();
        let view = cx.weak_entity();
        let shortcut = KeybindingKeystroke::from_keystroke(
            Keystroke::parse_with_os("secondary-shift-s", cx.platform_info().os)
                .expect("valid scan shortcut"),
        );
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() == main && event.keystroke.should_match(&shortcut) {
                window.prevent_default();
                cx.stop_propagation();
                let _ = view.update(cx, |view, cx| view.start_finder(window, cx));
            }
        });
        cx.on_release(move |_, _| drop(keys)).detach();
        #[cfg(target_os = "macos")]
        {
            let mut rx = platform::install(window);
            cx.spawn_in(window, async move |view, cx| {
                while let Some(input) = rx.recv().await {
                    let _ = view.update_in(cx, |this, window, cx| {
                        if this.accepts_import(window, cx) {
                            this.process_import(
                                vec![input],
                                ImportTarget::Direct,
                                false,
                                false,
                                window,
                                cx,
                            );
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
            .tooltip(if cfg!(target_os = "macos") {
                "⇧⌘S"
            } else {
                "Ctrl+Shift+S"
            })
            .loading(self.scan_pending || self.import_busy)
            .on_click(cx.listener(|this, _, window, cx| this.start_finder(window, cx)))
    }

    fn handle_scanned_qr(
        &mut self,
        text: String,
        source: platform::SelectionSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let play_sound = source == platform::SelectionSource::Click;
        let target = if self.show_subscription_form && window.has_active_dialog(cx) {
            ImportTarget::SubscriptionDialog
        } else if self.show_import_box && window.has_active_dialog(cx) {
            ImportTarget::NodeImportDialog
        } else {
            ImportTarget::Direct
        };
        self.process_import(
            vec![Input::Text(text)],
            target,
            play_sound,
            true,
            window,
            cx,
        );
    }

    pub(super) fn paste_import(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.accepts_import(window, cx) || window.has_focused_input(cx) {
            return false;
        }
        self.read_import_clipboard(ImportTarget::Direct, window, cx);
        true
    }

    pub(super) fn paste_into_import(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.read_import_clipboard(ImportTarget::NodeImportDialog, window, cx);
        true
    }

    pub(super) fn paste_into_subscription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_import_clipboard(ImportTarget::SubscriptionDialog, window, cx);
    }

    /// Intercepts clipboard paste inside the subscription dialog only for QR images/files
    /// or when neither text field is focused, preserving normal text paste in both inputs.
    pub(super) fn paste_image_into_subscription(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.sub_name.focus_handle(cx).is_focused(window) {
            return false;
        }
        #[cfg(target_os = "macos")]
        match platform::paste_image() {
            Ok(Some(image)) => {
                self.process_import(
                    vec![image],
                    ImportTarget::SubscriptionDialog,
                    false,
                    false,
                    window,
                    cx,
                );
                return true;
            }
            Err(issue) => {
                let (zh, en) = issue.message();
                self.import_problem(self.tr(zh, en).to_string(), window, cx);
                return true;
            }
            Ok(None) => {}
        }
        if self.sub_url.focus_handle(cx).is_focused(window) {
            if let Some(item) = cx.read_from_clipboard() {
                let has_binary = item.entries.iter().any(|entry| {
                    matches!(
                        entry,
                        ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
                    )
                });
                if has_binary {
                    self.process_import(
                        inputs_from_clipboard(item),
                        ImportTarget::SubscriptionDialog,
                        false,
                        false,
                        window,
                        cx,
                    );
                    return true;
                }
            }
            return false;
        }
        self.paste_into_subscription(window, cx);
        true
    }

    fn read_import_clipboard(
        &mut self,
        target: ImportTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(target_os = "macos")]
        match platform::paste_image() {
            Ok(Some(image)) => {
                return self.process_import(vec![image], target, false, false, window, cx);
            }
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
                let stale = match target {
                    ImportTarget::NodeImportDialog => {
                        !this.show_import_box || this.import_editor_epoch != epoch
                    }
                    ImportTarget::SubscriptionDialog => {
                        !this.show_subscription_form || this.import_editor_epoch != epoch
                    }
                    ImportTarget::Direct | ImportTarget::SubmitEditor => false,
                };
                if stale {
                    return;
                }
                match result {
                    Ok(Some(item)) => this.process_import(
                        inputs_from_clipboard(item),
                        target,
                        false,
                        false,
                        window,
                        cx,
                    ),
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
            self.process_import(
                inputs_from_clipboard(item.clone()),
                ImportTarget::Direct,
                false,
                false,
                window,
                cx,
            );
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
                ImportTarget::Direct,
                false,
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
        self.process_import(
            vec![Input::Text(text)],
            ImportTarget::SubmitEditor,
            false,
            false,
            window,
            cx,
        );
    }

    fn import_problem(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_import_box || self.show_subscription_form {
            self.manual_error = Some(message);
            self.sub_error_url = None;
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
        target: ImportTarget,
        play_sound: bool,
        from_qr: bool,
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
        if play_sound && (problem.is_some() || self.import_busy) {
            platform::play_import_sound(false, window, cx);
        }
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
        self.sub_error_url = None;
        self.import_busy = true;
        window.refresh();
        cx.notify();
        let epoch = self.import_editor_epoch;
        let editor = self.show_import_box || self.show_subscription_form;
        let service = self.service.clone();
        cx.spawn_in(window, async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let mut texts = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    let mut errors = Vec::new();
                    let mut unreadable = false;
                    for input in inputs {
                        let image = matches!(input, Input::Image(_));
                        match decode::extract(input) {
                            Ok(payloads) => {
                                for text in payloads {
                                    if !seen.insert(text.clone()) {
                                        continue;
                                    }
                                    if target == ImportTarget::NodeImportDialog
                                        && (image || from_qr)
                                        && matches!(
                                            xrose_core::AppService::preview_shared_content(&text),
                                            SharePreview::Unsupported
                                        )
                                    {
                                        unreadable = true;
                                        continue;
                                    }
                                    texts.push(text);
                                }
                            }
                            Err(issue) => {
                                let (zh, en) = issue.message();
                                errors.push(tr(lang, zh, en).to_string());
                            }
                        }
                    }
                    match target {
                        ImportTarget::SubscriptionDialog => BackgroundImport::SubscriptionDialog {
                            outcome: classify_subscription_payloads(&texts),
                            errors,
                        },
                        ImportTarget::NodeImportDialog => {
                            let mut report = SharedImport::default();
                            report.unreadable = unreadable;
                            report.errors = errors;
                            BackgroundImport::NodeImportDialog { texts, report }
                        }
                        ImportTarget::Direct => match plan_direct_import(&texts) {
                            DirectImportPlan::ConfirmSubscription { url, default_name } => {
                                BackgroundImport::ConfirmSubscription {
                                    url,
                                    default_name,
                                    errors,
                                }
                            }
                            DirectImportPlan::ReviewMixed(content) => {
                                BackgroundImport::ReviewMixed { content, errors }
                            }
                            DirectImportPlan::ImportDirect => {
                                let mut report = SharedImport::default();
                                report.errors = errors;
                                for text in texts {
                                    let part = service.import_shared_content(&text);
                                    report.nodes += part.nodes;
                                    report.subscriptions += part.subscriptions;
                                    report.duplicates += part.duplicates;
                                    report.unreadable |= part.unreadable;
                                    report.errors.extend(part.errors);
                                }
                                BackgroundImport::Applied { report }
                            }
                        },
                        ImportTarget::SubmitEditor => {
                            let mut report = SharedImport::default();
                            report.errors = errors;
                            for text in texts {
                                let part = service.import_shared_content(&text);
                                report.nodes += part.nodes;
                                report.subscriptions += part.subscriptions;
                                report.duplicates += part.duplicates;
                                report.unreadable |= part.unreadable;
                                report.errors.extend(part.errors);
                            }
                            BackgroundImport::Applied { report }
                        }
                    }
                })
                .await;
            let _ = view.update_in(cx, |this, window, cx| {
                this.import_busy = false;
                let same_editor = editor
                    && (this.show_import_box || this.show_subscription_form)
                    && this.import_editor_epoch == epoch;
                match outcome {
                    BackgroundImport::SubscriptionDialog { outcome, errors } => {
                        if !this.show_subscription_form || this.import_editor_epoch != epoch {
                            window.refresh();
                            cx.notify();
                            return;
                        }
                        match outcome {
                            SubscriptionScanOutcome::Url { url, default_name } => {
                                let duplicate =
                                    this.snapshot.subscriptions.iter().any(|sub| sub.url == url);
                                let err = duplicate.then(|| {
                                    this.tr(
                                        "此订阅地址已添加",
                                        "This subscription URL is already added.",
                                    )
                                    .to_string()
                                });
                                if play_sound {
                                    platform::play_import_sound(!duplicate, window, cx);
                                }
                                this.open_subscription_dialog_prefilled(
                                    &url,
                                    &default_name,
                                    err,
                                    window,
                                    cx,
                                );
                            }
                            SubscriptionScanOutcome::NodeLink => {
                                if play_sound {
                                    platform::play_import_sound(false, window, cx);
                                }
                                this.manual_error = Some(
                                    this.tr(
                                        "识别到节点链接，请前往「节点 → 添加节点」导入",
                                        "Detected a node link. Import it from Nodes → Add node.",
                                    )
                                    .to_string(),
                                );
                                this.sub_error_url = None;
                            }
                            SubscriptionScanOutcome::Invalid => {
                                if play_sound {
                                    platform::play_import_sound(false, window, cx);
                                }
                                let message = if !errors.is_empty() {
                                    errors.join(" · ")
                                } else {
                                    this.tr(
                                        "识别到的内容不是有效的订阅地址，请检查格式",
                                        "Unrecognized subscription URL. Check the format.",
                                    )
                                    .to_string()
                                };
                                this.manual_error = Some(message);
                                this.sub_error_url = None;
                            }
                        }
                        window.refresh();
                        cx.notify();
                    }
                    BackgroundImport::NodeImportDialog { texts, report } => {
                        if !this.show_import_box || this.import_editor_epoch != epoch {
                            window.refresh();
                            cx.notify();
                            return;
                        }
                        if !texts.is_empty() {
                            let joined = texts.join("\n");
                            this.import_input.update(cx, |input, cx| {
                                if from_qr {
                                    let existing = input.value().to_string();
                                    if existing.trim().is_empty() {
                                        input.set_value(joined, window, cx);
                                    } else {
                                        let sep = if existing.ends_with('\n') { "" } else { "\n" };
                                        input.set_value(
                                            format!("{existing}{sep}{joined}"),
                                            window,
                                            cx,
                                        );
                                    }
                                } else {
                                    input.replace(joined, window, cx);
                                }
                            });
                            this.import_input.focus_handle(cx).focus(window, cx);
                        }
                        let mut feedback = Vec::new();
                        if report.unreadable {
                            feedback.push(
                                this.tr(
                                    "内容无法识别为节点链接或订阅，请检查格式",
                                    "Unrecognized node or subscription. Check the format.",
                                )
                                .to_string(),
                            );
                        }
                        feedback.extend(report.errors);
                        let failed = !feedback.is_empty() || texts.is_empty();
                        if play_sound {
                            platform::play_import_sound(!failed, window, cx);
                        }
                        if !feedback.is_empty() {
                            this.manual_error = Some(feedback.join(" · "));
                        }
                        window.refresh();
                        cx.notify();
                    }
                    BackgroundImport::ConfirmSubscription {
                        url,
                        default_name,
                        errors,
                    } => {
                        let duplicate =
                            this.snapshot.subscriptions.iter().any(|sub| sub.url == url);
                        let mut issues = Vec::new();
                        if duplicate {
                            issues.push(
                                this.tr(
                                    "此订阅地址已添加",
                                    "This subscription URL is already added.",
                                )
                                .to_string(),
                            );
                        }
                        issues.extend(errors);
                        let err = (!issues.is_empty()).then(|| issues.join(" · "));
                        if play_sound {
                            platform::play_import_sound(err.is_none(), window, cx);
                        }
                        this.open_subscription_dialog_prefilled(
                            &url,
                            &default_name,
                            err,
                            window,
                            cx,
                        );
                    }
                    BackgroundImport::ReviewMixed { content, errors } => {
                        if play_sound {
                            platform::play_import_sound(errors.is_empty(), window, cx);
                        }
                        this.open_node_import_prefilled(content, window, cx);
                        if !errors.is_empty() {
                            this.manual_error = Some(errors.join(" · "));
                            window.refresh();
                            cx.notify();
                        }
                    }
                    BackgroundImport::Applied { report } => {
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
                        if play_sound {
                            platform::play_import_sound(added > 0 && !failed, window, cx);
                        }
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
                        if same_editor
                            && target == ImportTarget::SubmitEditor
                            && added > 0
                            && !failed
                        {
                            this.import_input
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            window.close_dialog(cx);
                            this.clear_node_editor(window, cx);
                        }
                        this.sync(cx);
                        window.refresh();
                    }
                }
            });
        })
        .detach();
    }
}

#[cfg(target_os = "macos")]
pub(super) fn enable_drop(enabled: bool) {
    platform::enable_drop(enabled);
}

#[cfg(test)]
mod tests {
    use super::{
        DirectImportPlan, SubscriptionScanOutcome, classify_subscription_payloads,
        plan_direct_import,
    };

    #[test]
    fn direct_import_routes_single_subscription_url_to_confirmation_dialog() {
        assert_eq!(
            plan_direct_import(&["https://wikipedia.com".to_string()]),
            DirectImportPlan::ConfirmSubscription {
                url: "https://wikipedia.com".to_string(),
                default_name: "wikipedia.com".to_string(),
            }
        );
        assert_eq!(
            plan_direct_import(&["https://sub.example.test/api?token=abc".to_string()]),
            DirectImportPlan::ConfirmSubscription {
                url: "https://sub.example.test/api?token=abc".to_string(),
                default_name: "sub.example.test".to_string(),
            }
        );
    }

    #[test]
    fn direct_import_routes_mixed_or_multi_url_to_review_and_keeps_nodes_direct() {
        assert_eq!(
            plan_direct_import(&["socks://node.example:1080#Demo".to_string()]),
            DirectImportPlan::ImportDirect
        );
        assert_eq!(
            plan_direct_import(&["http://proxy.example:8080?xrose=proxy#HTTP".to_string()]),
            DirectImportPlan::ImportDirect
        );
        assert!(matches!(
            plan_direct_import(&[
                "https://a.example/sub".to_string(),
                "https://b.example/sub".to_string(),
            ]),
            DirectImportPlan::ReviewMixed(_)
        ));
        assert!(matches!(
            plan_direct_import(&[
                "https://a.example/sub\nsocks://node.example:1080#Demo".to_string(),
            ]),
            DirectImportPlan::ReviewMixed(_)
        ));
    }

    #[test]
    fn subscription_dialog_classifies_url_node_and_invalid_payloads() {
        assert_eq!(
            classify_subscription_payloads(&["https://feed.example/list".to_string()]),
            SubscriptionScanOutcome::Url {
                url: "https://feed.example/list".to_string(),
                default_name: "feed.example".to_string(),
            }
        );
        assert_eq!(
            classify_subscription_payloads(&["socks://node.example:1080#Demo".to_string()]),
            SubscriptionScanOutcome::NodeLink
        );
        assert_eq!(
            classify_subscription_payloads(&["weixin://dl/business".to_string()]),
            SubscriptionScanOutcome::Invalid
        );
    }
}
