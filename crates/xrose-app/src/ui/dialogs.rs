//! Modal presentation for settings, node editing and sharing.

use std::sync::Arc;

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::WindowExt;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::Node;
use xrose_core::service::ToastLevel;

use super::{theme::palette, widgets::ChoiceGroup, AppView};

/// Shared motion policy for form, detail, and confirmation dialogs.
pub(super) const DIALOG_ENTRANCE: DialogEntrance = DialogEntrance::SlideDown;

pub(super) fn dialog_scroll_body(
    id: &'static str,
    max_height: Pixels,
    body: impl IntoElement,
) -> Div {
    // Extend the viewport into the dialog padding to leave room for outer focus rings.
    div().mx_neg_1().child(
        div()
            .px_1()
            .pb_1()
            .max_h(max_height)
            .child(body)
            .overflow_y_scrollbar()
            .id(id),
    )
}

/// Places settings in a scrollable surface so the close control stays reachable.
pub(super) fn settings_dialog(
    dialog: Dialog,
    title: SharedString,
    body: impl IntoElement,
    p: &super::theme::Palette,
    window: &Window,
) -> Dialog {
    dialog
        .centered()
        .w(px(super::theme::DIALOG_WIDE))
        .bg(p.panel)
        .text_color(p.fg)
        .border_color(p.border)
        .entrance(DIALOG_ENTRANCE)
        .on_ok(|_, _, _| false)
        .title(title)
        .child(dialog_scroll_body(
            "settings-dialog-body",
            window.viewport_size().height * 0.68,
            body,
        ))
}

impl AppView {
    pub(super) fn open_subscription_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_node_dialog(None, true, window, cx);
        self.show_subscription_form = true;
        self.sub_url.focus_handle(cx).focus(window, cx);
        window.refresh();
    }

    pub(crate) fn open_node_dialog(
        &mut self,
        node: Option<Node>,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manual_form.reset(window, cx);
        self.editing_node = node.as_ref().map(|node| node.id.clone());
        if let Some(node) = node {
            self.manual_form.load(&node, window, cx);
        }
        self.manual_error = None;
        self.show_manual_form = manual;
        self.show_subscription_form = false;
        self.show_import_box = !manual;
        self.import_editor_epoch = self.import_editor_epoch.wrapping_add(1);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.node_dialog(dialog, cx))
            } else {
                dialog
            }
        });
        if self.show_import_box {
            self.import_input.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn node_dialog(&mut self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let editing = self.editing_node.is_some();
        let p = palette(cx);
        let view = cx.entity().downgrade();
        let mut dialog = dialog
            .centered()
            .w(px(super::theme::DIALOG_WIDE))
            .bg(p.panel)
            .text_color(p.fg)
            .border_color(p.border)
            .entrance(DIALOG_ENTRANCE)
            .overlay_closable(false)
            .on_ok(|_, _, _| false)
            .title(if editing {
                self.tr("编辑节点", "Edit node")
            } else if self.show_subscription_form {
                self.tr("添加订阅", "Add subscription")
            } else {
                self.tr("添加节点", "Add node")
            })
            .on_close(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.clear_node_editor(window, cx);
                });
            });
        if !editing {
            dialog = dialog.child(
                div().flex().pb_3().child(
                    ChoiceGroup::new(
                        "add-node-mode",
                        vec![
                            self.tr("链接与二维码", "Links and QR codes"),
                            self.tr("手动填写", "Enter manually"),
                        ],
                        usize::from(self.show_manual_form),
                    )
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        this.show_import_box = *index == 0;
                        this.show_manual_form = *index == 1;
                        this.manual_error = None;
                        cx.notify();
                    })),
                ),
            );
        }
        if self.show_manual_form && self.show_subscription_form {
            dialog
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .pb_1()
                        .child(Input::new(&self.sub_name))
                        .child(Input::new(&self.sub_url))
                        .when_some(self.manual_error.clone(), |d, message| {
                            d.child(super::widgets::form_feedback(message, false, &p))
                        }),
                )
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(self.scan_button(cx))
                        .child(
                            Button::new("save-subscription")
                                .primary()
                                .label(self.tr("添加", "Add"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let name = this.sub_name.read(cx).value().to_string();
                                    let url = this.sub_url.read(cx).value().to_string();
                                    match this.service.add_subscription(name, url) {
                                        Ok(_) => {
                                            this.sub_url.update(cx, |input, cx| {
                                                input.set_value("", window, cx)
                                            });
                                            window.close_dialog(cx);
                                            this.clear_node_editor(window, cx);
                                            this.sync(cx);
                                        }
                                        Err(err) => {
                                            this.manual_error = Some(err.to_string());
                                            window.refresh();
                                            cx.notify();
                                        }
                                    }
                                })),
                        ),
                )
        } else if self.show_manual_form {
            dialog
                .child(self.manual_node_fields(cx))
                .footer(self.manual_node_footer(cx))
        } else {
            let p = palette(cx);
            let empty = self.import_input.read(cx).value().trim().is_empty();
            dialog
                .child(
                    div().flex().flex_col().gap_3().min_w(px(0.)).pb_1()
                        .capture_action(cx.listener(|this, _: &gpui_base::input::Paste, window, cx| {
                            if this.paste_into_import(window,cx) { cx.stop_propagation(); }
                        }))
                        .child(Textarea::new(&self.import_input))
                        .child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                            "支持 v2rayN、Shadowrocket 分享链接，以及 Clash YAML、SIP008 和 sing-box JSON。",
                            "Supports v2rayN and Shadowrocket share links, Clash YAML, SIP008 and sing-box JSON.",
                        )))
                        .when_some(self.manual_error.clone(), |d, error| {
                            d.child(super::widgets::form_feedback(error,false,&p))
                        }),
                )
                .footer(
                    div().flex().flex_wrap().justify_end().gap_2()
                        .child(self.scan_button(cx))
                        .child(Button::new("import-clipboard")
                            .icon(IconName::ClipboardPaste)
                            .label(self.tr("从剪贴板粘贴", "Paste from clipboard"))
                            .tooltip(if cfg!(target_os = "macos") { "⌘V" } else { "Ctrl+V" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.paste_into_import(window,cx);
                            })))
                        .child(Button::new("cancel-import")
                            .label(self.tr("取消", "Cancel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.clear_node_editor(window, cx);
                            })))
                        .child(Button::new("import-link")
                            .primary().disabled(empty || self.import_busy).loading(self.import_busy)
                            .label(self.tr("导入", "Import"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let content = this.import_input.read(cx).value().to_string();
                                this.submit_editor_content(content,window,cx);
                            }))),
                )
        }
    }

    pub(crate) fn clear_node_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.manual_form.reset(window, cx);
        self.manual_error = None;
        self.editing_node = None;
        self.show_manual_form = false;
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
            dialog
                .centered()
                .w(px(440.))
                .bg(p.panel)
                .text_color(p.fg)
                .border_color(p.border)
                .entrance(DIALOG_ENTRANCE)
                .title(title.clone())
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
                .footer(
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
                )
        });
    }
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
    use super::share_qr_image;

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
