use super::theme::*;
use gpui_base::{Scrollbar, ScrollbarHandle, ScrollbarMode, Selectable};
use gpui_kit::App;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    Div, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, Role,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement, Styled, div, px,
};

use super::theme::Palette;

type IndexChangeHandler = std::rc::Rc<dyn Fn(&usize, &mut gpui_kit::Window, &mut App)>;

/// Overlay the thumb on the bounded viewport, outside its scrolling content.
pub(super) fn scroll_frame(
    id: &'static str,
    handle: &(impl ScrollbarHandle + Clone),
    viewport: impl IntoElement,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(viewport)
        .child(
            Scrollbar::vertical(handle)
                .id(id)
                .mode(ScrollbarMode::Scrolling)
                .viewport_from_layout(),
        )
}

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

pub(super) fn prefixed_dropdown_content(
    prefix: SharedString,
    value: impl Into<SharedString>,
    p: &Palette,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_xs().text_color(p.muted_fg).child(prefix))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg)
                .max_w(px(160.))
                .truncate()
                .child(value.into()),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(px(14.))
                .text_color(p.muted_fg)
                .flex_shrink_0(),
        )
}

pub fn dot(color: Hsla) -> impl IntoElement {
    div().size(px(8.)).flex_shrink_0().rounded_full().bg(color)
}

pub fn radio_indicator(checked: bool, border: Hsla, dot: Hsla) -> Div {
    div()
        .size(px(20.))
        .flex_shrink_0()
        .rounded_full()
        .border_1()
        .border_color(border)
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |d| {
            d.child(div().size(px(10.)).rounded_full().bg(dot))
        })
}

/// Position inside the row's 1px border, clear of its rounded corners.
pub(super) fn selection_bar(color: Hsla) -> Div {
    div()
        .absolute()
        .left(px(1.))
        .top(px(7.))
        .bottom(px(7.))
        .w(px(3.))
        .rounded_full()
        .bg(color)
}

pub(super) fn radio_row_surface(
    id: impl Into<ElementId>,
    checked: bool,
    enabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    let accent = p.accent;
    div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .gap_3()
        .p_3()
        .rounded_md()
        .bg(if checked { p.accent } else { p.bg })
        .border_1()
        .border_color(p.border.opacity(0.))
        .when(checked, |row| row.child(selection_bar(p.primary)))
        .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(accent)))
}

pub(super) fn row_radio(
    id: impl Into<ElementId>,
    label: SharedString,
    checked: bool,
    enabled: bool,
    p: &Palette,
) -> gpui_base::Radio {
    let primary = p.primary;
    let accent = p.accent;
    gpui_base::Radio::new(id)
        .checked(checked)
        .disabled(!enabled)
        .size(px(ICON_BUTTON_SIZE))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(p.border.opacity(0.))
        .focus_visible(move |s| s.border_color(primary))
        .when(!enabled, |d| d.opacity(DISABLED_OPACITY))
        .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(accent)))
        .accessibility_label(label)
        .child(radio_indicator(
            checked,
            if checked { primary } else { p.muted_fg },
            primary,
        ))
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

/// Icon-only button colors: neutral gray hover/press when inactive, and `--accent-soft` + `--primary` when active.
pub fn icon_button_colors(p: &Palette, active: bool) -> (Hsla, Hsla, Hsla) {
    if active {
        (p.primary, p.neutral_hover(), p.accent)
    } else {
        (p.fg, p.neutral_hover(), p.neutral_active())
    }
}

/// Custom variant for icon-only buttons so enabled toggles use cyan and hover stays gray.
pub fn icon_button_style(p: &Palette, active: bool, cx: &App) -> ButtonCustomVariant {
    let (fg, hover, active_bg) = icon_button_colors(p, active);
    ButtonCustomVariant::new(cx)
        .foreground(fg)
        .hover(hover)
        .active(active_bg)
}

pub fn more_button(id: impl Into<ElementId>, label: impl Into<SharedString>, cx: &App) -> Button {
    icon_button(id, IconName::Ellipsis, label, cx)
}

pub fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    cx: &App,
) -> Button {
    let label = label.into();
    let p = palette(cx);
    Button::new(id)
        .compact()
        .custom(icon_button_style(&p, false, cx))
        .icon(icon)
        .w(px(ICON_BUTTON_SIZE))
        .h(px(ICON_BUTTON_SIZE))
        .accessibility_label(label.clone())
        .tooltip(label)
}

/// Standard 32×32 toolbar icon button with neutral gray hover.
pub fn toolbar_icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    cx: &App,
) -> Button {
    let label = label.into();
    let p = palette(cx);
    Button::new(id)
        .custom(icon_button_style(&p, false, cx))
        .icon(icon)
        .accessibility_label(label.clone())
        .tooltip(label)
}

/// Applies cyan enabled state (`--accent-soft` fill + `--primary` icon) or gray hover state to an icon button.
pub fn icon_button_toggled(button: Button, active: bool, cx: &App) -> Button {
    let p = palette(cx);
    button
        .custom(icon_button_style(&p, active, cx))
        .selected(active)
        .toggled(active)
}

/// Inline outcomes remain visible in forms even when system notifications are enabled.
pub(super) fn form_feedback(message: String, success: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id("form-feedback")
        .role(if success { Role::Status } else { Role::Alert })
        .aria_label(message.clone())
        .text_sm()
        .text_color(if success { p.ok_text } else { p.danger_text })
        .child(message)
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
    let p = palette(cx);
    Button::new(id)
        .compact()
        .custom(danger_button_style(&p, cx).foreground(p.muted_fg))
        .icon(icon)
        .tooltip(label.clone())
        .accessibility_label(label)
        .w(px(ICON_BUTTON_SIZE))
        .h(px(ICON_BUTTON_SIZE))
}

/// Option item inside a sliding [`SegmentedTabs`] track.
pub(super) struct SegmentedTab {
    pub(super) id: ElementId,
    pub(super) icon: Option<IconName>,
    pub(super) label: SharedString,
    pub(super) badge: SharedString,
}

impl SegmentedTab {
    pub(super) fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        badge: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: None,
            label: label.into(),
            badge: badge.into(),
        }
    }

    pub(super) fn icon(mut self, icon: Option<IconName>) -> Self {
        self.icon = icon;
        self
    }
}

/// Bordered segmented tab track with a sliding cyan capsule indicator, shared by
/// Mesh card tabs and Connections primary tabs.
#[derive(gpui_kit::IntoElement)]
pub(super) struct SegmentedTabs {
    id: ElementId,
    tabs: Vec<SegmentedTab>,
    selected: Option<usize>,
    handler: Option<IndexChangeHandler>,
}

/// Generic bordered track hosting heterogeneous tab items (such as draggable
/// routing groups) while retaining the shared 150 ms sliding cyan indicator.
#[derive(gpui_kit::IntoElement)]
pub(super) struct SlidingTabTrack {
    id: ElementId,
    selected_item: Option<usize>,
    items: Vec<gpui_kit::AnyElement>,
}

impl SlidingTabTrack {
    pub(super) fn new(id: impl Into<ElementId>, selected_item: Option<usize>) -> Self {
        Self {
            id: id.into(),
            selected_item,
            items: Vec::new(),
        }
    }

    pub(super) fn children(
        mut self,
        items: impl IntoIterator<Item = gpui_kit::AnyElement>,
    ) -> Self {
        self.items.extend(items);
        self
    }
}

fn sliding_track_shell(
    id: &ElementId,
    selected_child: Option<usize>,
    p: &Palette,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) -> gpui_kit::Stateful<Div> {
    let layout = group_layout_handle(id, window, cx);
    div()
        .id(id.clone())
        .role(Role::TabList)
        .relative()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .p_0p5()
        .rounded_md()
        .bg(p.bg)
        .border_1()
        .border_color(p.border)
        .track_scroll(&layout)
        .child(sliding_indicator_canvas_at_child(
            id,
            selected_child,
            &layout,
            p,
            window,
            cx,
        ))
}

fn sliding_track_selected_child(selected_item: Option<usize>, item_count: usize) -> Option<usize> {
    selected_item
        .filter(|&index| index < item_count)
        .map(|index| index + 1)
}

impl gpui_kit::RenderOnce for SlidingTabTrack {
    fn render(self, window: &mut gpui_kit::Window, cx: &mut App) -> impl IntoElement {
        let p = super::theme::palette(cx);
        let selected_child = sliding_track_selected_child(self.selected_item, self.items.len());
        sliding_track_shell(&self.id, selected_child, &p, window, cx).children(self.items)
    }
}

impl SegmentedTabs {
    pub(super) fn new(
        id: impl Into<ElementId>,
        tabs: Vec<SegmentedTab>,
        selected: Option<usize>,
    ) -> Self {
        Self {
            id: id.into(),
            tabs,
            selected,
            handler: None,
        }
    }

    pub(super) fn on_change(
        mut self,
        handler: impl Fn(&usize, &mut gpui_kit::Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(std::rc::Rc::new(handler));
        self
    }
}

impl gpui_kit::RenderOnce for SegmentedTabs {
    fn render(self, window: &mut gpui_kit::Window, cx: &mut App) -> impl IntoElement {
        let p = super::theme::palette(cx);
        let count = self.tabs.len();
        let handles = group_focus_handles(&self.id, count, window, cx);
        let key_handles = handles.clone();
        let key_handler = self.handler.clone();
        let selected = self.selected.filter(|&index| index < count);
        let mono = cx.theme().mono_font_family.clone();
        let transparent = cx.theme().transparent;
        let mut track = sliding_track_shell(&self.id, selected.map(|i| i + 1), &p, window, cx)
            .on_key_down(move |event, window, cx| {
                if let Some(next) = roving_key_target(event.keystroke.key.as_str(), selected, count)
                {
                    key_handles[next].focus(window, cx);
                    if let Some(handler) = &key_handler {
                        handler(&next, window, cx);
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                }
            });
        for (index, tab) in self.tabs.into_iter().enumerate() {
            let checked = selected == Some(index);
            let handler = self.handler.clone();
            let variant = ButtonCustomVariant::new(cx)
                .color(transparent)
                .foreground(if checked { p.primary } else { p.muted_fg })
                .hover(if checked {
                    transparent
                } else {
                    p.neutral_hover()
                })
                .active(if checked {
                    transparent
                } else {
                    p.neutral_active()
                });
            let badge_bg = if checked {
                if p.is_dark {
                    p.primary.opacity(0.22)
                } else {
                    p.panel.opacity(0.92)
                }
            } else {
                p.panel_alt
            };
            let badge_fg = if checked { p.primary } else { p.muted_fg };
            track = track.child(
                Button::new(tab.id)
                    .role(Role::Tab)
                    .compact()
                    .h(px(26.))
                    .rounded(gpui_kit::component::button::ButtonRounded::Size(px(3.)))
                    .custom(variant)
                    .selected(checked)
                    .tab_stop(index == selected.unwrap_or(0))
                    .track_focus(&handles[index])
                    .border_1()
                    .border_color(transparent)
                    .when_some(tab.icon, |b, icon| b.icon(icon))
                    .when(checked, |b| b.font_weight(FontWeight::MEDIUM))
                    .label(tab.label)
                    .child(
                        div()
                            .px_1p5()
                            .rounded_full()
                            .text_xs()
                            .font_family(mono.clone())
                            .bg(badge_bg)
                            .text_color(badge_fg)
                            .child(tab.badge),
                    )
                    .on_click(move |_, window, cx| {
                        if let Some(handler) = &handler {
                            handler(&index, window, cx);
                        }
                    }),
            );
        }
        track
    }
}

pub(super) fn danger_button_style(p: &Palette, cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .foreground(p.danger_text)
        .hover(p.danger.opacity(SEMANTIC_ALPHA))
        .active(p.danger.opacity(SEMANTIC_ALPHA))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChoiceStyle {
    Default,
    Small,
    Cards,
}

/// Controlled radio group with roving focus and arrow-key selection.
#[derive(gpui_kit::IntoElement)]
pub struct ChoiceGroup {
    id: ElementId,
    labels: Vec<SharedString>,
    tooltips: Vec<SharedString>,
    selected: Option<usize>,
    style: ChoiceStyle,
    full_width: bool,
    wrap: bool,
    scroll: Option<(ScrollHandle, Pixels)>,
    disabled: bool,
    handler: Option<IndexChangeHandler>,
}
impl ChoiceGroup {
    pub fn new(
        id: impl Into<ElementId>,
        labels: Vec<SharedString>,
        selected: impl Into<Option<usize>>,
    ) -> Self {
        Self {
            id: id.into(),
            labels,
            tooltips: Vec::new(),
            selected: selected.into(),
            style: ChoiceStyle::Default,
            full_width: false,
            wrap: true,
            scroll: None,
            disabled: false,
            handler: None,
        }
    }
    pub fn cards(mut self) -> Self {
        self.style = ChoiceStyle::Cards;
        self
    }
    pub fn small(mut self) -> Self {
        self.style = ChoiceStyle::Small;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn tooltips(mut self, tooltips: Vec<SharedString>) -> Self {
        self.tooltips = tooltips;
        self
    }
    pub fn no_wrap(mut self) -> Self {
        self.wrap = false;
        self
    }
    pub fn scrollable(mut self, handle: &ScrollHandle, max_height: Pixels) -> Self {
        self.scroll = Some((handle.clone(), max_height));
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
// The first child is the indicator canvas. Measure items relative to it so
// moving the window or scrolling a parent does not start a new transition.
fn sliding_indicator_item_bounds(
    layout: &ScrollHandle,
    child_index: Option<usize>,
) -> Option<gpui_kit::Bounds<Pixels>> {
    let origin = layout.bounds_for_item(0)?.origin;
    let mut bounds = layout.bounds_for_item(child_index?)?;
    bounds.origin -= origin;
    Some(bounds)
}

fn sliding_indicator_canvas_at_child(
    id: &ElementId,
    child_index: Option<usize>,
    layout: &ScrollHandle,
    p: &Palette,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) -> gpui_kit::Canvas<()> {
    let measured = sliding_indicator_item_bounds(layout, child_index);
    let animated = measured.map(|target| {
        gpui_base::motion::transition(
            ElementId::from((id.clone(), "choice-indicator")),
            target,
            gpui_base::motion::Transition::new(std::time::Duration::from_millis(150)),
            window,
            cx,
        )
    });
    let layout = layout.clone();
    let accent = p.accent;
    let primary = p.primary;
    gpui_kit::canvas(
        |_, _, _| (),
        move |canvas, _, window, _| {
            if let Some(actual) = sliding_indicator_item_bounds(&layout, child_index) {
                let mut indicator = if Some(actual) == measured {
                    animated.unwrap_or(actual)
                } else {
                    // Layout was first measured or changed during this frame.
                    window.request_animation_frame();
                    actual
                };
                indicator.origin += canvas.origin;
                window.paint_quad(gpui_kit::quad(
                    indicator,
                    px(3.),
                    accent,
                    px(1.),
                    primary,
                    Default::default(),
                ));
            }
        },
    )
    .absolute()
    .inset_0()
}

fn sliding_indicator_canvas(
    id: &ElementId,
    selected: Option<usize>,
    layout: &ScrollHandle,
    p: &Palette,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) -> gpui_kit::Canvas<()> {
    sliding_indicator_canvas_at_child(id, selected.map(|index| index + 1), layout, p, window, cx)
}

fn group_focus_handles(
    id: &ElementId,
    count: usize,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) -> Vec<gpui_kit::FocusHandle> {
    (0..count)
        .map(|index| {
            window
                .use_keyed_state((id.clone(), index.to_string()), cx, |_, cx| {
                    cx.focus_handle()
                })
                .read(cx)
                .clone()
        })
        .collect()
}

fn group_layout_handle(
    id: &ElementId,
    window: &mut gpui_kit::Window,
    cx: &mut App,
) -> ScrollHandle {
    window
        .use_keyed_state((id.clone(), "choice-layout"), cx, |_, _| {
            ScrollHandle::new()
        })
        .read(cx)
        .clone()
}

fn roving_key_target(key: &str, selected: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let next = match key {
        "left" | "up" => Some(selected.map_or(count - 1, |i| (i + count - 1) % count)),
        "right" | "down" => Some(selected.map_or(0, |i| (i + 1) % count)),
        "home" => Some(0),
        "end" => count.checked_sub(1),
        _ => None,
    };
    next.filter(|index| *index < count)
}

impl gpui_kit::RenderOnce for ChoiceGroup {
    fn render(self, window: &mut gpui_kit::Window, cx: &mut App) -> impl IntoElement {
        let p = super::theme::palette(cx);
        let count = self.labels.len();
        let handles = group_focus_handles(&self.id, count, window, cx);
        let key_handles = handles.clone();
        let key_handler = self.handler.clone();
        let selected = self.selected.filter(|&index| index < count);
        let style = self.style;
        let full_width = self.full_width;
        let wrap = self.wrap;
        let disabled = self.disabled;
        let layout = match &self.scroll {
            Some((handle, _)) => handle.clone(),
            None => group_layout_handle(&self.id, window, cx),
        };
        let mut group = gpui_base::RadioGroup::new(self.id.clone())
            .axis(gpui_kit::Axis::Horizontal)
            .relative()
            .flex()
            .when(wrap, |group| group.flex_wrap())
            .when(full_width, |group| group.w_full())
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(p.border)
            .bg(p.bg)
            .opacity(if disabled { DISABLED_OPACITY } else { 1. })
            .track_scroll(&layout)
            .when_some(self.scroll.as_ref(), |group, (_, height)| {
                group.max_h(*height).overflow_y_scroll()
            })
            .child(sliding_indicator_canvas(
                &self.id, selected, &layout, &p, window, cx,
            ))
            .on_key_down(move |event, window, cx| {
                if disabled {
                    return;
                }
                if let Some(next) = roving_key_target(event.keystroke.key.as_str(), selected, count)
                {
                    key_handles[next].focus(window, cx);
                    if let Some((scroll, _)) = &self.scroll {
                        scroll.scroll_to_item(next + 1);
                    }
                    if let Some(handler) = &key_handler {
                        handler(&next, window, cx);
                    }
                    cx.stop_propagation();
                    window.prevent_default();
                }
            });
        for (index, label) in self.labels.into_iter().enumerate() {
            let checked = selected == Some(index);
            let handler = self.handler.clone();
            let tooltip = self
                .tooltips
                .get(index)
                .cloned()
                .filter(|tip| !tip.is_empty())
                .or_else(|| (style == ChoiceStyle::Cards).then(|| label.clone()));
            group = group.child(
                gpui_base::Radio::new((self.id.clone(), index.to_string()))
                    .checked(checked)
                    .disabled(disabled)
                    .tab_stop(index == selected.unwrap_or(0) && !disabled)
                    .track_focus(&handles[index])
                    .accessibility_label(label.clone())
                    .when(style == ChoiceStyle::Cards, |option| option.max_w_full())
                    .when_some(tooltip, |option, tooltip| {
                        option.tooltip(move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        })
                    })
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
                    .h(px(match style {
                        ChoiceStyle::Small => 24.,
                        ChoiceStyle::Default => ICON_BUTTON_SIZE,
                        ChoiceStyle::Cards => CONTROL_HEIGHT,
                    }))
                    .map(|option| match style {
                        ChoiceStyle::Small => option.px_2().text_xs(),
                        _ => option.px_3().text_sm(),
                    })
                    .cursor_pointer()
                    // Subtract the track's 1px border and 2px padding from its 6px radius.
                    .rounded_sm()
                    .border_1()
                    .border_color(p.border.opacity(0.))
                    .text_color(if checked { p.primary } else { p.muted_fg })
                    .when(!disabled && !checked, |option| {
                        option
                            .hover(move |s| s.bg(p.panel_alt))
                            .focus_visible(move |s| s.border_color(p.primary))
                    })
                    .child(div().min_w_0().truncate().child(label)),
            );
        }
        if wrap {
            // Keep a block wrapper around wrapped groups so flex rows measure all lines.
            div()
                .max_w_full()
                .when(full_width, |wrapper| wrapper.w_full())
                .child(group)
                .into_any_element()
        } else {
            group.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette(is_dark: bool) -> Palette {
        let (primary, primary_fg) = interaction(is_dark);
        Palette {
            bg: hsl(210., 0.4, if is_dark { 0.09 } else { 0.975 }),
            panel: hsl(0., 0., if is_dark { 0.15 } else { 1.0 }),
            panel_alt: hsl(210., 0.3, if is_dark { 0.15 } else { 0.94 }),
            fg: hsl(0., 0., if is_dark { 0.98 } else { 0.04 }),
            muted_fg: hsl(0., 0., if is_dark { 0.65 } else { 0.42 }),
            border: hsl(0., 0., 0.5),
            accent: interaction_tint(is_dark),
            primary,
            primary_fg,
            danger: hsl(0., 0.8, 0.5),
            success: hsl(140., 0.7, 0.45),
            warning: hsl(40., 0.9, 0.5),
            info: hsl(190., 0.8, 0.5),
            data_down: primary,
            data_up: primary,
            ok_text: hsl(140., 0.7, 0.4),
            warn_text: hsl(40., 0.9, 0.4),
            danger_text: hsl(0., 0.8, 0.5),
            is_dark,
        }
    }

    #[test]
    fn icon_button_colors_use_cyan_when_active_and_gray_on_hover() {
        for is_dark in [false, true] {
            let p = test_palette(is_dark);

            let (fg_active, hover_active, bg_active) = icon_button_colors(&p, true);
            assert_eq!(fg_active, p.primary);
            assert_eq!(bg_active, p.accent);
            assert_eq!(hover_active, p.neutral_hover());

            let (fg_idle, hover_idle, bg_idle) = icon_button_colors(&p, false);
            assert_eq!(fg_idle, p.fg);
            assert_eq!(hover_idle, p.neutral_hover());
            assert_eq!(bg_idle, p.neutral_active());
            assert_ne!(hover_idle, p.accent);
        }
    }

    #[test]
    fn roving_key_target_wraps_and_handles_collapsed_selection() {
        assert_eq!(roving_key_target("left", Some(0), 3), Some(2));
        assert_eq!(roving_key_target("right", Some(2), 3), Some(0));
        assert_eq!(roving_key_target("down", Some(1), 3), Some(2));
        assert_eq!(roving_key_target("up", Some(1), 3), Some(0));
        assert_eq!(roving_key_target("home", Some(2), 3), Some(0));
        assert_eq!(roving_key_target("end", Some(0), 3), Some(2));

        // When collapsed (no active selection), arrows start at the corresponding edge.
        assert_eq!(roving_key_target("right", None, 2), Some(0));
        assert_eq!(roving_key_target("left", None, 2), Some(1));
        assert_eq!(roving_key_target("tab", Some(0), 2), None);
        assert_eq!(roving_key_target("right", None, 0), None);
    }

    #[test]
    fn sliding_track_selected_child_offsets_canvas_and_guards_bounds() {
        assert_eq!(sliding_track_selected_child(None, 4), None);
        assert_eq!(sliding_track_selected_child(Some(0), 0), None);
        assert_eq!(sliding_track_selected_child(Some(4), 4), None);
        assert_eq!(sliding_track_selected_child(Some(0), 5), Some(1));
        assert_eq!(sliding_track_selected_child(Some(4), 5), Some(5));
    }
}
