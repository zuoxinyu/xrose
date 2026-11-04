use super::theme::*;
use gpui_kit::App;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::{
    Div, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    div, px,
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
        .flex()
        .flex_col()
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

/// The core start action keeps the interaction accent.
pub fn start_button(cx: &App) -> ButtonCustomVariant {
    let p = super::theme::palette(cx);
    ButtonCustomVariant::new(cx)
        .color(p.primary)
        .foreground(p.primary_fg)
        .hover(p.primary)
        .active(p.primary)
        .shadow(false)
}

pub fn more_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    let label = label.into();
    Button::new(id)
        .compact()
        .ghost()
        .icon(IconName::Ellipsis)
        .w(px(ICON_BUTTON_SIZE))
        .h(px(ICON_BUTTON_SIZE))
        .accessibility_label(label.clone())
        .tooltip(label)
}

pub fn empty_state(
    p: &Palette,
    icon: IconName,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .min_h(px(EMPTY_HEIGHT))
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            Icon::new(icon)
                .size(px(CONTROL_HEIGHT))
                .text_color(p.muted_fg),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(p.fg)
                .child(title.into()),
        )
        .child(
            div()
                .text_sm()
                .text_color(p.muted_fg)
                .child(description.into()),
        )
}

/// Destructive tools stay neutral until hover and use the standard focus behavior.
pub fn danger_icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tip: impl Into<SharedString>,
    cx: &App,
) -> Button {
    let label = tip.into();
    Button::new(id)
        .compact()
        .custom(danger_tool_style(cx))
        .icon(icon)
        .tooltip(label.clone())
        .accessibility_label(label)
        .w(px(ICON_BUTTON_SIZE))
        .h(px(ICON_BUTTON_SIZE))
}

pub fn danger_tool_style(cx: &App) -> ButtonCustomVariant {
    let p = super::theme::palette(cx);
    ButtonCustomVariant::new(cx)
        .foreground(p.muted_fg)
        .hover(p.danger.opacity(SEMANTIC_ALPHA))
        .active(p.danger.opacity(SEMANTIC_ALPHA))
}

/// Controlled radio group with roving focus and arrow-key selection.
#[derive(gpui_kit::IntoElement)]
pub struct ChoiceGroup {
    id: ElementId,
    labels: Vec<SharedString>,
    selected: usize,
    cards: bool,
    disabled: bool,
    handler: Option<std::rc::Rc<dyn Fn(&usize, &mut gpui_kit::Window, &mut App)>>,
}
impl ChoiceGroup {
    pub fn new(id: impl Into<ElementId>, labels: Vec<SharedString>, selected: usize) -> Self {
        Self {
            id: id.into(),
            labels,
            selected,
            cards: false,
            disabled: false,
            handler: None,
        }
    }
    pub fn cards(mut self) -> Self {
        self.cards = true;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn on_change(
        mut self,
        handler: impl Fn(&usize, &mut gpui_kit::Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(std::rc::Rc::new(handler));
        self
    }
}
impl gpui_kit::RenderOnce for ChoiceGroup {
    fn render(self, window: &mut gpui_kit::Window, cx: &mut App) -> impl IntoElement {
        let p = super::theme::palette(cx);
        let count = self.labels.len();
        let handles: Vec<_> = (0..count)
            .map(|index| {
                window
                    .use_keyed_state((self.id.clone(), index.to_string()), cx, |_, cx| {
                        cx.focus_handle()
                    })
                    .read(cx)
                    .clone()
            })
            .collect();
        let key_handles = handles.clone();
        let key_handler = self.handler.clone();
        let selected = self.selected;
        let cards = self.cards;
        let disabled = self.disabled;
        let mut group = gpui_base::RadioGroup::new(self.id.clone())
            .axis(gpui_kit::Axis::Horizontal)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .bg(p.bg)
            .opacity(if disabled { DISABLED_OPACITY } else { 1. })
            .on_key_down(move |event, window, cx| {
                if disabled || count == 0 {
                    return;
                }
                let next = match event.keystroke.key.as_str() {
                    "left" | "up" => Some((selected + count - 1) % count.max(1)),
                    "right" | "down" => Some((selected + 1) % count.max(1)),
                    "home" => Some(0),
                    "end" => count.checked_sub(1),
                    _ => None,
                };
                if let Some(next) = next.filter(|n| *n < count) {
                    key_handles[next].focus(window, cx);
                    if let Some(handler) = &key_handler {
                        handler(&next, window, cx);
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                }
            });
        for (index, label) in self.labels.into_iter().enumerate() {
            let checked = index == selected;
            let focused = handles[index].is_focused(window);
            let handler = self.handler.clone();
            group = group.child(
                gpui_base::Radio::new((self.id.clone(), index.to_string()))
                    .checked(checked)
                    .disabled(disabled)
                    .tab_stop(checked && !disabled)
                    .track_focus(&handles[index])
                    .accessibility_label(label.clone())
                    .on_change(move |_, _, window, cx| {
                        if disabled {
                            return;
                        }
                        if let Some(handler) = &handler {
                            handler(&index, window, cx);
                        }
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(if cards {
                        CONTROL_HEIGHT
                    } else {
                        ICON_BUTTON_SIZE
                    }))
                    .px_3()
                    .cursor_pointer()
                    // Subtract the track's 1px border and 2px padding from its 6px radius.
                    .rounded_sm()
                    .border_1()
                    .border_color(if checked || focused {
                        p.primary
                    } else {
                        p.border.opacity(0.)
                    })
                    .bg(if checked { p.accent } else { p.bg })
                    .text_sm()
                    .text_color(if checked { p.primary } else { p.muted_fg })
                    .hover(move |s| {
                        if disabled {
                            s
                        } else {
                            s.bg(if checked { p.accent } else { p.panel_alt })
                        }
                    })
                    .child(label),
            );
        }
        group
    }
}
