use gpui_kit::component::switch::Switch;
use gpui_kit::{div, px, Div, ElementId, Hsla, IntoElement, ParentElement, SharedString, Styled};

use super::theme::Palette;

pub fn card(p: &Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(p.panel)
        .border_1()
        .border_color(p.border)
}

pub fn section_title(
    p: &Palette,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(p.fg)
                .child(title.into()),
        )
        .child(
            div()
                .text_sm()
                .text_color(p.muted_fg)
                .child(subtitle.into()),
        )
}

pub fn pill(text: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> impl IntoElement {
    div()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(bg)
        .text_xs()
        .text_color(fg)
        .child(text.into())
}

pub fn dot(color: Hsla) -> impl IntoElement {
    div().size(px(8.)).flex_shrink_0().rounded_full().bg(color)
}

pub fn toggle(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool) -> Switch {
    Switch::new(id).checked(checked).label(label.into())
}

pub fn empty_hint(p: &Palette, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .w_full()
        .py_8()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(p.muted_fg)
        .child(text.into())
}

pub fn divider(p: &Palette) -> impl IntoElement {
    div().h(px(1.)).w_full().bg(p.border)
}
