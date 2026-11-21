//! Modal presentation for settings, node editing and sharing.

use std::sync::Arc;

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::Node;
use xrose_core::service::ToastLevel;

use super::{
    AppView, Page,
    theme::{self, DIALOG_WIDE, DIALOG_WIDTH, Palette, palette},
    widgets::{ChoiceGroup, FieldWidth, dialog_field, dialog_field_with_desc},
};

/// Shared motion policy for form, detail, and confirmation dialogs.
pub(super) const DIALOG_ENTRANCE: DialogEntrance = DialogEntrance::SlideDown;

pub(super) fn dialog_scroll_body(
    id: &'static str,
    max_height: Pixels,
    body: impl IntoElement,
) -> Div {
    // Pull 12px into the dialog's horizontal padding so the overlay scrollbar sits in
    // the outer gutter while form controls stay aligned with the title and footer.
    div().mx_neg_3().child(
        div()
            .px_3()
            .py_1()
            .max_h(max_height)
            .child(body)
            .overflow_y_scrollbar()
            .id(id),
    )
}

/// Shared modal shell styling across confirmation, settings, and editor dialogs.
pub(super) fn dialog_shell(dialog: Dialog, title: SharedString, width: f32, p: &Palette) -> Dialog {
    dialog
        .centered()
        .w(px(width))
        .bg(p.panel)
        .text_color(p.fg)
        .border_color(p.border)
        .entrance(DIALOG_ENTRANCE)
        .on_ok(|_, _, _| false)
        .title(title)
}

pub(super) fn dialog_toolbar(p: &Palette, content: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .pb_3()
        .mb_1()
        .border_b_1()
        .border_color(p.border)
        .child(content)
}

pub(super) fn dialog_footer(p: &Palette, error: Option<String>, actions: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .mt_neg_1()
        .pt_3()
        .border_t_1()
        .border_color(p.border)
        .when_some(error, |d, message| {
            d.child(super::widgets::form_feedback(message, false, p))
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .w_full()
                .child(actions),
        )
}

/// Places settings in a scrollable surface so the close control stays reachable.
pub(super) fn settings_dialog(
    dialog: Dialog,
    title: SharedString,
    body: impl IntoElement,
    p: &Palette,
    window: &Window,
) -> Dialog {
    dialog_shell(dialog, title, DIALOG_WIDE, p).child(dialog_scroll_body(
        "settings-dialog-body",
        window.viewport_size().height * 0.68,
        body,
    ))
}

impl AppView {
    pub(super) fn open_subscription_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sub_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.sub_url
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.sub_name_auto = None;
        self.sub_error_url = None;
        self.manual_error = None;
        self.editing_node = None;
        self.show_manual_form = false;
        self.show_chain_form = false;
        self.show_import_box = false;
        self.show_subscription_form = true;
        self.import_editor_epoch = self.import_editor_epoch.wrapping_add(1);
        let epoch = self.import_editor_epoch;
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| {
                    this.subscription_dialog(dialog, epoch, window, cx)
                })
            } else {
                dialog
            }
        });
        self.sub_url.focus_handle(cx).focus(window, cx);
        window.refresh();
        cx.notify();
    }

    pub(super) fn open_subscription_dialog_prefilled(
        &mut self,
        url: &str,
        default_name: &str,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let already_open = self.show_subscription_form && window.has_active_dialog(cx);
        if already_open {
            self.apply_subscription_prefill(url, default_name, error, window, cx);
            self.sub_url.focus_handle(cx).focus(window, cx);
            window.refresh();
            cx.notify();
            return;
        }
        self.sub_name_auto = None;
        self.sub_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.apply_subscription_prefill(url, default_name, error, window, cx);
        self.editing_node = None;
        self.show_manual_form = false;
        self.show_chain_form = false;
        self.show_import_box = false;
        self.show_subscription_form = true;
        self.import_editor_epoch = self.import_editor_epoch.wrapping_add(1);
        let epoch = self.import_editor_epoch;
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| {
                    this.subscription_dialog(dialog, epoch, window, cx)
                })
            } else {
                dialog
            }
        });
        self.sub_url.focus_handle(cx).focus(window, cx);
        window.refresh();
        cx.notify();
    }

    fn apply_subscription_prefill(
        &mut self,
        url: &str,
        default_name: &str,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_name = self.sub_name.read(cx).value().trim().to_string();
        if should_autofill_subscription_name(&current_name, self.sub_name_auto.as_deref()) {
            self.sub_name
                .update(cx, |input, cx| input.set_value(default_name, window, cx));
            self.sub_name_auto = Some(default_name.to_string());
        }
        self.sub_url
            .update(cx, |input, cx| input.set_value(url, window, cx));
        self.sub_error_url = error.as_ref().map(|_| url.to_string());
        self.manual_error = error;
    }

    pub(super) fn submit_subscription_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import_busy {
            return;
        }
        let name = self.sub_name.read(cx).value().to_string();
        let url = self.sub_url.read(cx).value().to_string();
        match self.service.add_subscription(name, url.clone()) {
            Ok(_) => {
                self.sub_name
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.sub_url
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.sub_name_auto = None;
                self.sub_error_url = None;
                window.close_dialog(cx);
                self.clear_node_editor(window, cx);
                if self.page != Page::Subscriptions {
                    self.navigate(Page::Subscriptions, cx);
                }
                self.sync(cx);
            }
            Err(err) => {
                self.sub_error_url = Some(url);
                self.manual_error = Some(err.to_string());
                window.refresh();
                cx.notify();
            }
        }
    }

    fn subscription_dialog(
        &mut self,
        dialog: Dialog,
        epoch: u64,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let p = palette(cx);
        let view = cx.entity().downgrade();
        let url_empty = self.sub_url.read(cx).value().trim().is_empty();
        dialog_shell(
            dialog,
            self.tr("添加订阅", "Add subscription"),
            DIALOG_WIDE,
            &p,
        )
        .overlay_closable(false)
        .on_close(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| {
                if this.import_editor_epoch == epoch {
                    this.clear_node_editor(window, cx);
                }
            });
        })
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pb_1()
                .capture_action(
                    cx.listener(|this, _: &gpui_base::input::Paste, window, cx| {
                        if this.paste_image_into_subscription(window, cx) {
                            cx.stop_propagation();
                        }
                    }),
                )
                .child(dialog_field(
                    &p,
                    self.tr("名称（可选）", "Name (optional)"),
                    &self.sub_name,
                    FieldWidth::Medium,
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("订阅地址", "Subscription URL"),
                    self.tr(
                        "支持粘贴订阅链接或二维码图片，也可扫描屏幕二维码。",
                        "Paste a subscription URL or QR image, or scan a screen QR code.",
                    ),
                    &self.sub_url,
                    FieldWidth::Full,
                )),
        )
        .footer(dialog_footer(
            &p,
            self.manual_error.clone(),
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(self.scan_button(cx))
                .child(
                    Button::new("sub-import-clipboard")
                        .compact()
                        .h(px(theme::CONTROL_HEIGHT))
                        .icon(IconName::ClipboardPaste)
                        .label(self.tr("从剪贴板粘贴", "Paste from clipboard"))
                        .tooltip(if cfg!(target_os = "macos") {
                            "⌘V"
                        } else {
                            "Ctrl+V"
                        })
                        .loading(self.import_busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.paste_into_subscription(window, cx);
                        })),
                )
                .child(
                    Button::new("cancel-sub-add")
                        .label(self.tr("取消", "Cancel"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.close_dialog(cx);
                            this.clear_node_editor(window, cx);
                        })),
                )
                .child(
                    Button::new("save-subscription")
                        .primary()
                        .disabled(url_empty || self.import_busy)
                        .label(self.tr("添加订阅", "Add subscription"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_subscription_form(window, cx);
                        })),
                ),
        ))
    }

    pub(super) fn open_node_import_prefilled(
        &mut self,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_node_dialog(None, false, window, cx);
        self.import_input
            .update(cx, |input, cx| input.set_value(content, window, cx));
        self.import_input.focus_handle(cx).focus(window, cx);
        window.refresh();
        cx.notify();
    }

    pub(crate) fn open_node_dialog(
        &mut self,
        node: Option<Node>,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manual_form.reset(window, cx);
        self.chain_form.reset(window, cx);
        self.editing_node = node.as_ref().map(|node| node.id.clone());
        let is_chain = node.as_ref().is_some_and(Node::is_chain);
        if let Some(node) = node {
            if is_chain {
                self.chain_form.load(&node, window, cx);
            } else {
                self.manual_form.load(&node, window, cx);
            }
        }
        self.manual_error = None;
        self.show_chain_form = is_chain;
        self.show_manual_form = !is_chain && manual;
        self.show_subscription_form = false;
        self.show_import_box = !is_chain && !manual;
        self.import_editor_epoch = self.import_editor_epoch.wrapping_add(1);
        let epoch = self.import_editor_epoch;
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.node_dialog(dialog, epoch, window, cx))
            } else {
                dialog
            }
        });
        if self.show_import_box {
            self.import_input.focus_handle(cx).focus(window, cx);
        } else if self.show_manual_form {
            self.manual_form.server.focus_handle(cx).focus(window, cx);
        } else if self.show_chain_form {
            self.chain_form.name.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn open_chain_dialog(
        &mut self,
        entry_node_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manual_form.reset(window, cx);
        self.chain_form.reset(window, cx);
        if let Some(id) = entry_node_id {
            self.chain_form.prefill_entry(id, window, cx);
        }
        self.editing_node = None;
        self.manual_error = None;
        self.show_chain_form = true;
        self.show_manual_form = false;
        self.show_subscription_form = false;
        self.show_import_box = false;
        self.import_editor_epoch = self.import_editor_epoch.wrapping_add(1);
        let epoch = self.import_editor_epoch;
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.node_dialog(dialog, epoch, window, cx))
            } else {
                dialog
            }
        });
        self.chain_form.name.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn node_dialog(
        &mut self,
        dialog: Dialog,
        epoch: u64,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let editing = self.editing_node.is_some();
        let p = palette(cx);
        let view = cx.entity().downgrade();
        let title = if editing {
            if self.show_chain_form {
                self.tr("编辑代理链", "Edit proxy chain")
            } else {
                self.tr("编辑节点", "Edit node")
            }
        } else {
            self.tr("添加节点", "Add node")
        };
        let mut dialog = dialog_shell(dialog, title, DIALOG_WIDE, &p)
            .overlay_closable(false)
            .on_close(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.import_editor_epoch == epoch {
                        this.clear_node_editor(window, cx);
                    }
                });
            });
        if !editing {
            let selected_mode = if self.show_chain_form {
                2
            } else if self.show_manual_form {
                1
            } else {
                0
            };
            dialog = dialog.child(dialog_toolbar(
                &p,
                ChoiceGroup::new(
                    "add-node-mode",
                    vec![
                        self.tr("链接与二维码", "Links and QR codes"),
                        self.tr("手动填写", "Enter manually"),
                        self.tr("代理链", "Proxy chain"),
                    ],
                    selected_mode,
                )
                .on_change(cx.listener(|this, index: &usize, window, cx| {
                    this.show_import_box = *index == 0;
                    this.show_manual_form = *index == 1;
                    this.show_chain_form = *index == 2;
                    this.manual_error = None;
                    if this.show_import_box {
                        this.import_input.focus_handle(cx).focus(window, cx);
                    } else if this.show_manual_form {
                        this.manual_form.server.focus_handle(cx).focus(window, cx);
                    } else if this.show_chain_form {
                        this.chain_form.name.focus_handle(cx).focus(window, cx);
                    }
                    cx.notify();
                })),
            ));
        }
        if self.show_chain_form {
            dialog
                .child(dialog_scroll_body(
                    "chain-editor-scroll",
                    px(520.).min(window.viewport_size().height * 0.58),
                    self.chain_editor_fields(cx),
                ))
                .footer(self.chain_editor_footer(cx))
        } else if self.show_manual_form {
            dialog
                .child(dialog_scroll_body(
                    "manual-node-scroll",
                    px(520.).min(window.viewport_size().height * 0.58),
                    self.manual_node_fields(cx),
                ))
                .footer(self.manual_node_footer(cx))
        } else {
            let empty = self.import_input.read(cx).value().trim().is_empty();
            dialog
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .min_w(px(0.))
                        .pb_1()
                        .capture_action(cx.listener(
                            |this, _: &gpui_base::input::Paste, window, cx| {
                                if this.paste_into_import(window, cx) {
                                    cx.stop_propagation();
                                }
                            },
                        ))
                        .child(Textarea::new(&self.import_input))
                        .child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                            "支持 v2rayN、Shadowrocket 分享链接，以及 Clash YAML、SIP008 和 sing-box JSON。",
                            "Supports v2rayN and Shadowrocket share links, Clash YAML, SIP008 and sing-box JSON.",
                        ))),
                )
                .footer(dialog_footer(
                    &p,
                    self.manual_error.clone(),
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(self.scan_button(cx))
                        .child(
                            Button::new("import-clipboard")
                                .icon(IconName::ClipboardPaste)
                                .label(self.tr("从剪贴板粘贴", "Paste from clipboard"))
                                .tooltip(if cfg!(target_os = "macos") {
                                    "⌘V"
                                } else {
                                    "Ctrl+V"
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.paste_into_import(window, cx);
                                })),
                        )
                        .child(
                            Button::new("cancel-import")
                                .label(self.tr("取消", "Cancel"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    window.close_dialog(cx);
                                    this.clear_node_editor(window, cx);
                                })),
                        )
                        .child(
                            Button::new("import-link")
                                .primary()
                                .disabled(empty || self.import_busy)
                                .loading(self.import_busy)
                                .label(self.tr("导入", "Import"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let content = this.import_input.read(cx).value().to_string();
                                    this.submit_editor_content(content, window, cx);
                                })),
                        ),
                ))
        }
    }

    pub(crate) fn clear_node_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.manual_form.reset(window, cx);
        self.chain_form.reset(window, cx);
        self.manual_error = None;
        self.sub_error_url = None;
        self.sub_name_auto = None;
        self.editing_node = None;
        self.show_manual_form = false;
        self.show_chain_form = false;
        self.show_import_box = false;
        self.show_subscription_form = false;
        cx.notify();
    }

    pub(super) fn open_share_qr(
        &self,
        title: SharedString,
        name: &str,
        link: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (pixels, modules) = match share_qr_image(&link) {
            Ok(qr) => qr,
            Err(_) => {
                self.service.toast(
                    ToastLevel::Error,
                    self.tr(
                        "链接过长，无法生成二维码，请使用「复制链接」",
                        "The link is too long for a QR code. Use Copy link instead.",
                    )
                    .to_string(),
                );
                return;
            }
        };
        let name = SharedString::from(name.to_owned());
        let qr = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            pixels
        )]));
        let copy = self.tr("复制链接", "Copy link");
        let close = self.tr("关闭", "Close");
        window.open_dialog(cx, move |dialog, window, cx| {
            let p = palette(cx);
            let available = (window.viewport_size().width - px(96.)).min(px(360.));
            let module_size = (f32::from(available) / modules as f32).floor().max(1.);
            let size = px(module_size * modules as f32);
            dialog_shell(dialog, title.clone(), DIALOG_WIDTH, &p)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w_full()
                                .min_w(px(0.))
                                .text_sm()
                                .line_clamp(2)
                                .child(name.clone()),
                        )
                        .child(img(qr.clone()).w(size).h(size)),
                )
                .footer(dialog_footer(
                    &p,
                    None,
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("qr-copy").label(copy.clone()).on_click({
                            let link = link.clone();
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()))
                            }
                        }))
                        .child(
                            Button::new("close-qr")
                                .label(close.clone())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                ))
        });
    }
}

/// Returns true when the subscription name input can be safely auto-filled with a scanned domain.
fn should_autofill_subscription_name(current_trimmed: &str, last_auto: Option<&str>) -> bool {
    current_trimmed.is_empty() || last_auto == Some(current_trimmed)
}

fn share_qr_image(link: &str) -> xrose_core::Result<(image::RgbaImage, usize)> {
    let matrix = xrose_core::subscription::qr_matrix(link)?;
    let modules = matrix.len() + 8;
    let scale = 8;
    let side = (modules * scale) as u32;
    let mut pixels = image::RgbaImage::from_pixel(side, side, image::Rgba([255, 255, 255, 255]));
    for (y, row) in matrix.iter().enumerate() {
        for (x, dark) in row.iter().enumerate() {
            if *dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        pixels.put_pixel(
                            ((x + 4) * scale + dx) as u32,
                            ((y + 4) * scale + dy) as u32,
                            image::Rgba([0, 0, 0, 255]),
                        );
                    }
                }
            }
        }
    }
    Ok((pixels, modules))
}

#[cfg(test)]
mod tests {
    use super::{share_qr_image, should_autofill_subscription_name};

    #[test]
    fn autofill_subscription_name_updates_only_empty_or_previous_auto_name() {
        assert!(should_autofill_subscription_name("", None));
        assert!(should_autofill_subscription_name(
            "first.example",
            Some("first.example")
        ));
        assert!(!should_autofill_subscription_name(
            "My Custom Feed",
            Some("first.example")
        ));
        assert!(!should_autofill_subscription_name("My Custom Feed", None));
    }

    #[test]
    fn subscription_qr_scans_as_the_exact_copyable_url() {
        let link = "https://feed.example/list?name=A%2FB&format=full";
        let (pixels, _) = share_qr_image(link).unwrap();
        let gray = image::DynamicImage::ImageRgba8(pixels).into_luma8();
        let mut scanner = quircs::Quirc::default();
        let code = scanner
            .identify(gray.width() as usize, gray.height() as usize, &gray)
            .next()
            .unwrap()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(code.payload, link.as_bytes());
        assert!(share_qr_image(&"a".repeat(4096)).is_err());
    }
}
