//! Modal presentation for settings, node editing and sharing.

use std::sync::Arc;

use gpui_base::Disableable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::Node;
use xross_core::service::ToastLevel;

use super::{AppView, theme::palette, widgets::ChoiceGroup};

/// Shared motion policy for form, detail, and confirmation dialogs.
pub(super) const DIALOG_ENTRANCE: DialogEntrance = DialogEntrance::SlideDown;

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
        .child(
            div()
                .id("settings-dialog-body")
                .max_h(window.viewport_size().height * 0.68)
                .overflow_y_scroll()
                .mx_neg_1()
                .px_1()
                .child(body),
        )
}

impl AppView {
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
        self.show_import_box = !manual;
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.node_dialog(dialog, cx))
            } else {
                dialog
            }
        });
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
                            self.tr("链接导入", "Import links"),
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
        if self.show_manual_form {
            dialog
                .child(self.manual_node_fields(cx))
                .footer(self.manual_node_footer(cx))
        } else {
            let p = palette(cx);
            let empty = self.import_input.read(cx).value().trim().is_empty();
            dialog
                .child(
                    div().flex().flex_col().gap_3().min_w(px(0.))
                        .child(Input::new(&self.import_input))
                        .child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                            "支持节点分享链接、Clash YAML、SIP008 和 sing-box JSON",
                            "Supports share links, Clash YAML, SIP008 and sing-box JSON",
                        )))
                        .when_some(self.manual_error.clone(), |d, error| {
                            d.child(div().text_sm().text_color(p.danger).child(error))
                        }),
                )
                .footer(
                    div().flex().flex_wrap().justify_end().gap_2()
                        .child(Button::new("import-clipboard")
                            .label(self.tr("从剪贴板粘贴", "Paste from clipboard"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                                    this.import_input.update(cx, |input, cx| input.set_value(text, window, cx));
                                    this.manual_error = None;
                                } else {
                                    this.manual_error = Some(this.tr("剪贴板没有文本", "The clipboard contains no text").to_string());
                                }
                                cx.notify();
                            })))
                        .child(Button::new("cancel-import")
                            .label(self.tr("取消", "Cancel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.clear_node_editor(window, cx);
                            })))
                        .child(Button::new("import-link")
                            .primary().disabled(empty)
                            .label(self.tr("导入", "Import"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let content = this.import_input.read(cx).value().to_string();
                                if this.service.import_content(&content) > 0 {
                                    this.import_input.update(cx, |input, cx| input.set_value("", window, cx));
                                    window.close_dialog(cx);
                                    this.clear_node_editor(window, cx);
                                    this.sync(cx);
                                } else {
                                    this.manual_error = Some(this.tr(
                                        "未导入节点，请检查内容或确认节点是否已存在",
                                        "No nodes imported. Check the content or whether the nodes already exist.",
                                    ).to_string());
                                    cx.notify();
                                }
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
        cx.notify();
    }

    pub(crate) fn open_node_qr(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let matrix = match self.service.node_qr_matrix(id) {
            Ok(matrix) => matrix,
            Err(err) => {
                self.service.toast(ToastLevel::Error, err.to_string());
                return;
            }
        };
        let name = self
            .snapshot
            .model
            .node(id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        let link = self.service.export_links(&[id.to_string()]);
        let modules = matrix.len() + 8;
        let scale = 8;
        let side = (modules * scale) as u32;
        let mut pixels =
            image::RgbaImage::from_pixel(side, side, image::Rgba([255, 255, 255, 255]));
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
        let qr = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            pixels
        )]));
        let title = self.tr("节点二维码", "Node QR code");
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
