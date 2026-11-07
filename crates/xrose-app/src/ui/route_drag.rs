//! Stable-ID routing drag previews and insertion targets.
use super::{
    AppView, Page,
    theme::{self, palette},
    widgets::card,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::*;
use xrose_core::model::Placement;
use xrose_core::service::ToastLevel;

#[derive(Clone)]
pub(super) struct RouteDrag {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    pub groups: bool,
    pub epoch: u64,
}
impl Render for RouteDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        card(&palette(cx))
            .max_w(px(theme::FORM_FIELD_WIDTH))
            .text_sm()
            .child(self.name.clone())
    }
}

pub(super) fn handle(drag: RouteDrag, view: WeakEntity<AppView>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("route-grip-{}", drag.id)))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(theme::ICON_BUTTON_SIZE))
        .cursor_grab()
        .opacity(theme::DISABLED_OPACITY)
        .hover(|s| s.opacity(1.))
        .on_click(|_, _, cx| cx.stop_propagation())
        .on_drag(drag, move |drag, _, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.active_route_drag = Some(drag.clone());
                cx.notify();
            });
            cx.new(|_| drag.clone())
        })
        .child(Icon::new(IconName::GripVertical).size(px(theme::ICON_SIZE)))
}

fn placement_at(bounds: Bounds<Pixels>, pointer: Point<Pixels>, groups: bool) -> Placement {
    let before = if groups {
        pointer.x < bounds.center().x
    } else {
        pointer.y < bounds.center().y
    };
    if before {
        Placement::Before
    } else {
        Placement::After
    }
}

pub(super) fn target(
    row: Stateful<Div>,
    id: String,
    group: Option<String>,
    groups: bool,
    view: &AppView,
    cx: &mut Context<AppView>,
) -> Stateful<Div> {
    let color = palette(cx).primary;
    let compatible = cx.has_active_drag()
        && view.active_route_drag.as_ref().is_some_and(|drag| {
            drag.id != id
                && drag.groups == groups
                && drag.group == group
                && drag.epoch == view.route_drag_epoch
        });
    let measured = std::rc::Rc::new(std::cell::Cell::new(Bounds::<Pixels>::default()));
    let painted = measured.clone();
    row.relative()
        .child(
            canvas(
                move |bounds, _, _| painted.set(bounds),
                move |bounds, _, window, _| {
                    let pointer = window.mouse_position();
                    if !compatible || !bounds.contains(&pointer) {
                        return;
                    }
                    let placement = placement_at(bounds, pointer, groups);
                    let mut line = bounds;
                    if groups {
                        line.size.width = px(theme::INSERTION_LINE);
                        if placement == Placement::After {
                            line.origin.x = bounds.right() - line.size.width;
                        }
                    } else {
                        line.size.height = px(theme::INSERTION_LINE);
                        if placement == Placement::After {
                            line.origin.y = bounds.bottom() - line.size.height;
                        }
                    }
                    window.paint_quad(fill(line, color));
                },
            )
            .absolute()
            .inset_0(),
        )
        .on_drop(cx.listener(move |this, drag: &RouteDrag, window, cx| {
            this.active_route_drag = None;
            let bounds = measured.get();
            let pointer = window.mouse_position();
            if this.page != Page::Routing
                || drag.epoch != this.route_drag_epoch
                || drag.groups != groups
                || drag.group != group
                || (!groups && this.rule_group_id != group)
                || !bounds.contains(&pointer)
                || this.snapshot.busy.iter().any(|key| key == "core")
            {
                return;
            }
            // Measure the final pointer position, including quick drops with no intervening move event.
            let placement = placement_at(bounds, pointer, groups);
            let result = if groups {
                this.service.move_rule_group_to(&drag.id, &id, placement)
            } else {
                this.service
                    .move_rule_to(&drag.id, group.as_deref(), &id, placement)
            };
            if let Err(error) = result {
                this.service.toast(
                    ToastLevel::Error,
                    format!(
                        "{}: {error}",
                        this.tr("保存排序失败，请重试", "Could not save order; please retry")
                    ),
                );
            }
            this.sync(cx);
        }))
}

/// Continue scrolling while the pointer stays near an edge, even when stationary.
pub(super) fn scroll_next_frame(view: WeakEntity<AppView>, window: &mut Window, _cx: &mut App) {
    window.on_next_frame(move |window, cx| {
        let again = view
            .update(cx, |this, cx| {
                if !cx.has_active_drag() || this.page != Page::Routing {
                    this.route_scroll_running = false;
                    return false;
                }
                let bounds = this.content_scroll.bounds();
                let pointer = window.mouse_position();
                let delta = if !bounds.contains(&pointer) {
                    0.
                } else if pointer.y < bounds.top() + px(theme::DRAG_EDGE) {
                    theme::DRAG_SCROLL_STEP
                } else if pointer.y > bounds.bottom() - px(theme::DRAG_EDGE) {
                    -theme::DRAG_SCROLL_STEP
                } else {
                    0.
                };
                this.route_scroll_running = delta != 0.;
                if delta != 0. {
                    let mut offset = this.content_scroll.offset();
                    offset.y += px(delta);
                    this.content_scroll.set_offset(offset);
                    cx.notify();
                }
                this.route_scroll_running
            })
            .unwrap_or(false);
        if again {
            scroll_next_frame(view, window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::placement_at;
    use gpui_kit::{Bounds, point, px, size};
    use xrose_core::model::Placement;
    #[test]
    fn drop_uses_each_targets_axis_and_half() {
        let bounds = Bounds::new(point(px(20.), px(40.)), size(px(100.), px(60.)));
        assert_eq!(
            placement_at(bounds, point(px(30.), px(80.)), true),
            Placement::Before
        );
        assert_eq!(
            placement_at(bounds, point(px(90.), px(50.)), true),
            Placement::After
        );
        assert_eq!(
            placement_at(bounds, point(px(90.), px(50.)), false),
            Placement::Before
        );
        assert_eq!(
            placement_at(bounds, point(px(30.), px(80.)), false),
            Placement::After
        );
        assert_eq!(
            placement_at(bounds, bounds.center(), false),
            Placement::After
        );
    }
}
