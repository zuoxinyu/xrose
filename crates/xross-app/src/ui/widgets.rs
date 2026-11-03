use gpui_kit::assets::IconName;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    div, px, Div, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, StatefulInteractiveElement, Styled,
};

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
        .when(!p.is_dark, |d| d.shadow_xs())
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
        .flex_shrink_0()
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

/// Container of a segmented control; items come from [`segment_item`].
pub fn segmented(p: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .bg(p.panel_alt)
}

/// One option of a segmented control; the caller attaches `on_click`.
pub fn segment_item(
    p: &Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let hover_bg = p.panel;
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .px_3()
        .py_1()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .font_weight(if selected {
            gpui_kit::FontWeight::MEDIUM
        } else {
            gpui_kit::FontWeight::NORMAL
        })
        .text_color(if selected { p.primary } else { p.muted_fg })
        .when(selected, |d| d.bg(p.panel).shadow_xs())
        .when(!selected, |d| d.hover(move |h| h.bg(hover_bg.opacity(0.6))))
        .child(label.into())
}

/// Container for a grouped segment / button group.
pub fn button_group(p: &Palette) -> Div {
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .p_0p5()
        .gap_0p5()
        .rounded_md()
        .border_1()
        .border_color(p.border)
        .bg(p.panel_alt.opacity(0.35))
}

/// One item in a `button_group`.
/// Selected state uses the app's signature cyan theme:
/// `p.primary` for text/active border, `p.accent` (cyan tint) for background.
pub fn button_group_item(
    p: &Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let hover_bg = p.panel;
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(26.))
        .px_3()
        .rounded_sm()
        .text_xs()
        .cursor_pointer()
        .font_weight(if selected {
            gpui_kit::FontWeight::MEDIUM
        } else {
            gpui_kit::FontWeight::NORMAL
        })
        .border_1()
        .border_color(if selected {
            p.primary
        } else {
            gpui_kit::hsla(0., 0., 0., 0.)
        })
        .bg(if selected {
            p.accent
        } else {
            gpui_kit::hsla(0., 0., 0., 0.)
        })
        .text_color(if selected { p.primary } else { p.muted_fg })
        .when(!selected, |d| {
            d.hover(move |h| h.bg(hover_bg).text_color(p.fg))
        })
        .child(label.into())
}

/// Danger icon button with subtle red resting state, warning red on hover, and tooltip.
pub fn danger_icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tip: impl Into<SharedString>,
    p: &Palette,
) -> Stateful<Div> {
    let del_fg = p.danger.opacity(0.72);
    let del_hover_fg = p.danger;
    let del_hover_bg = p.danger.opacity(0.14);
    let del_active_bg = p.danger.opacity(0.22);
    let tip_text: SharedString = tip.into();
    div()
        .id(id)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.))
        .h(px(28.))
        .rounded_md()
        .text_color(del_fg)
        .hover(move |s| s.bg(del_hover_bg).text_color(del_hover_fg))
        .active(move |s| s.bg(del_active_bg).text_color(del_hover_fg))
        .tooltip(move |window, cx| Tooltip::new(tip_text.clone()).build(window, cx))
        .child(Icon::new(icon).size(px(14.)))
}
