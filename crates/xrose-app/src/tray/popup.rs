//! GPUI Kit flyout, independent of the native tray backend and main window.

use gpui_base::{Disableable, Scrollbar, ScrollbarMode};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::AppService;
use xrose_core::model::{DIRECT_VIRTUAL_NODE_ID, RoutePreset, WorkMode};
use xrose_core::settings::{Language, ThemeMode};
use xrose_core::util::format_speed;

use super::PopupAnchor;
use super::dashboard::{self, Dashboard, TrayAction};
use crate::i18n::tr;
use crate::ui::theme::palette;
use crate::ui::traffic_label;
use crate::ui::widgets::{danger_button_style, icon_button_style, start_ghost_button_style};

const NODE_ROW_HEIGHT: f32 = 38.;
const NODE_SCROLL_HINT_DELAY: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct PopupManager {
    window: Option<AnyWindowHandle>,
    view: Option<Entity<PopupView>>,
    last_refresh: Option<Instant>,
}

impl PopupManager {
    pub fn dismiss(&mut self, cx: &mut App) {
        if let (Some(handle), Some(view)) = (self.window, self.view.as_ref()) {
            let _ = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, _| view.dismiss(window))
            });
        }
    }

    pub fn toggle(&mut self, service: &AppService, anchor: Option<PopupAnchor>, cx: &mut App) {
        // Wayland cannot report global pointer hit-testing. Suppress the same
        // tray click's Activate when it immediately follows a focus dismissal.
        if cfg!(target_os = "linux")
            && self.view.as_ref().is_some_and(|view| {
                view.read(cx)
                    .deactivated_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
            })
        {
            if let Some(view) = self.view.as_ref() {
                view.update(cx, |view, _| view.deactivated_at = None);
            }
            return;
        }
        if self.view.as_ref().is_some_and(|view| view.read(cx).visible) {
            self.dismiss(cx);
            return;
        }
        let dashboard = dashboard::build(service);
        let display = anchor
            .and_then(|a| {
                cx.displays()
                    .into_iter()
                    .find(|display| tray_display_areas(display.as_ref()).0.contains(&a.position))
            })
            .or_else(|| cx.primary_display());
        let work_area = anchor
            .and_then(|a| a.work_area)
            .or_else(|| display.as_ref().map(|d| tray_display_areas(d.as_ref()).1))
            .unwrap_or(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1280.), px(800.)),
            ));
        let desired_height =
            408. + (dashboard.nodes.len().min(4) as f32 * NODE_ROW_HEIGHT).max(76.);
        let bounds = popup_bounds(
            anchor.map(|a| a.position),
            work_area,
            size(px(400.), px(desired_height)),
        );
        if let (Some(handle), Some(view)) = (self.window, self.view.as_ref()) {
            let reused = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.refresh_dashboard(dashboard.clone(), window, cx);
                    view.visible = true;
                    view.selected = view
                        .dashboard
                        .nodes
                        .iter()
                        .position(|node| node.active)
                        .unwrap_or(0);
                    view.scroll.scroll_to_item(view.selected);
                    view.show(window, bounds, cx);
                    window.focus(&view.focus, cx);
                    cx.notify();
                });
            });
            if reused.is_ok() {
                return;
            }
        }
        let error_service = service.clone();
        let service = service.clone();
        match open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                #[cfg(target_os = "linux")]
                display_id: display.map(|d| d.id()),
                #[cfg(target_os = "linux")]
                kind: if crate::wayland::desktop().layer_shell {
                    super::linux::popup_layer(anchor.map(|a| a.position), bounds, work_area)
                } else {
                    WindowKind::PopUp
                },
                #[cfg(not(target_os = "linux"))]
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                focus: !cfg!(any(windows, target_os = "macos")),
                show: !cfg!(any(windows, target_os = "macos")),
                app_id: Some("io.xrose.Xrose.Tray".into()),
                window_background: crate::window_effects::window_background(),
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                window.set_window_title("Xrose · Tray");
                crate::window_effects::apply(window, cx);
                cx.new(|cx| PopupView::new(service, dashboard, window, cx))
            },
        ) {
            Ok((handle, view)) => {
                let _ = handle.update(cx, |_, window, cx| {
                    #[cfg(windows)]
                    crate::window_effects::apply(window, cx);
                    view.update(cx, |view, cx| view.show(window, bounds, cx));
                });
                self.window = Some(handle);
                self.view = Some(view);
                self.last_refresh = Some(Instant::now());
            }
            Err(err) => error_service.toast(
                xrose_core::service::ToastLevel::Error,
                format!(
                    "{}{err}",
                    tr(
                        error_service.settings().language,
                        "无法打开托盘浮窗：",
                        "Failed to open tray flyout: ",
                    )
                ),
            ),
        }
    }

    pub fn tick(&mut self, service: &AppService, cx: &mut App) {
        if self
            .last_refresh
            .is_some_and(|last| last.elapsed() < Duration::from_millis(400))
        {
            return;
        }
        let Some(view) = self.view.as_ref() else {
            return;
        };
        if !view.read(cx).visible {
            return;
        }
        if let Some(handle) = self.window {
            let _ = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.refresh_dashboard(dashboard::build(service), window, cx);
                    cx.notify();
                })
            });
        }
        self.last_refresh = Some(Instant::now());
    }
}

/// The icon may lie outside the work area, for example in a secondary display's panel.
fn tray_display_areas(display: &dyn PlatformDisplay) -> (Bounds<Pixels>, Bounds<Pixels>) {
    #[cfg(target_os = "linux")]
    if crate::wayland::desktop().layer_shell {
        let screen = crate::wayland::desktop().bounds(display);
        return (screen, screen);
    }
    (display.bounds(), display.visible_bounds())
}

/// Clamp both axes to the monitor work area, including negative monitor origins.
fn popup_bounds(
    anchor: Option<Point<Pixels>>,
    work: Bounds<Pixels>,
    desired: Size<Pixels>,
) -> Bounds<Pixels> {
    let gap = px(8.);
    let width = desired.width.min((work.size.width - gap * 2.).max(px(1.)));
    let height = desired
        .height
        .min((work.size.height - gap * 2.).max(px(1.)));
    let at = anchor.unwrap_or(point(work.right() - gap, work.bottom() + gap));
    let below = at.y < work.center().y;
    let x = (at.x - width / 2.)
        .max(work.left() + gap)
        .min(work.right() - width - gap);
    let y = if below {
        at.y + gap
    } else {
        at.y - height - gap
    };
    let y = y.max(work.top() + gap).min(work.bottom() - height - gap);
    Bounds::new(point(x, y), size(width, height))
}

#[derive(Clone)]
struct RouteOption {
    preset: RoutePreset,
    label: SharedString,
}

impl SelectItem for RouteOption {
    type Value = RoutePreset;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &RoutePreset {
        &self.preset
    }
}

fn route_options(lang: Language) -> Vec<RouteOption> {
    [
        (
            RoutePreset::BypassMainland,
            "绕过中国大陆",
            "Bypass mainland",
        ),
        (RoutePreset::GfwList, "GFWList", "GFWList"),
        (RoutePreset::Global, "全局代理", "Global proxy"),
        (RoutePreset::ProxyApps, "仅代理指定应用", "Selected apps"),
        (RoutePreset::Custom, "自定义", "Custom"),
    ]
    .into_iter()
    .map(|(preset, zh, en)| RouteOption {
        preset,
        label: tr(lang, zh, en),
    })
    .collect()
}

fn mesh_phase_info(
    phase: xrose_core::mesh::MeshPhase,
    p: &crate::ui::theme::Palette,
    lang: Language,
) -> (Hsla, SharedString) {
    use xrose_core::mesh::MeshPhase::*;
    let color = match phase {
        Connected => p.success,
        Starting => p.info,
        NeedsLogin | NeedsApproval | Stopping => p.warning,
        Failed => p.danger,
        Disconnected => p.muted_fg,
    };
    (color, super::dashboard::mesh_phase_label(phase, lang))
}

fn status_toggle_button(
    id: impl Into<ElementId>,
    active: bool,
    disabled: bool,
    tip: SharedString,
    p: &crate::ui::theme::Palette,
    cx: &App,
) -> Button {
    Button::new(id)
        .xsmall()
        .flex_shrink_0()
        .custom(if active {
            danger_button_style(p, cx)
        } else {
            start_ghost_button_style(p, cx)
        })
        .icon(if active {
            IconName::Square
        } else {
            IconName::Play
        })
        .tooltip(tip.clone())
        .accessibility_label(tip)
        .disabled(disabled)
}

#[allow(clippy::too_many_arguments)]
fn render_status_column(
    p: &crate::ui::theme::Palette,
    kicker_icon: IconName,
    title: impl Into<SharedString>,
    status_color: Hsla,
    status_text: SharedString,
    toggle_button: Button,
    detail_text: String,
    detail_active: bool,
    cx: &App,
) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(Icon::new(kicker_icon).size(px(12.)).text_color(p.muted_fg))
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(p.muted_fg)
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w(px(0.))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(status_color),
                )
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_xs()
                        .text_color(p.fg)
                        .child(status_text),
                )
                .child(toggle_button),
        )
        .child(
            div()
                .min_w(px(0.))
                .truncate()
                .text_size(px(10.5))
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(if detail_active {
                    p.muted_fg
                } else {
                    p.muted_fg.opacity(0.5)
                })
                .child(detail_text),
        )
}

#[allow(clippy::too_many_arguments)]
fn mesh_status_column(
    status: &super::dashboard::DashboardMeshStatus,
    busy_mesh: bool,
    mesh_supported: bool,
    kicker_icon: IconName,
    title: &'static str,
    p: &crate::ui::theme::Palette,
    lang: Language,
    cx: &mut Context<PopupView>,
) -> Div {
    let provider = status.provider;
    let (dot_color, status_text) = if status.retrying {
        (p.warning, tr(lang, "等待重连", "Waiting to reconnect"))
    } else {
        mesh_phase_info(status.phase, p, lang)
    };
    let stoppable = status.is_stoppable();
    let stopping = status.phase == xrose_core::mesh::MeshPhase::Stopping;
    let button_active = stoppable || stopping;
    let busy = busy_mesh && !status.retrying;
    let disabled = if button_active {
        busy || stopping
    } else {
        busy || !mesh_supported || status.profile_id.is_none()
    };
    let tip = if status.retrying {
        tr(lang, "取消重连", "Cancel reconnect")
    } else if button_active {
        tr(lang, "断开组网", "Disconnect")
    } else if !mesh_supported {
        tr(lang, "当前平台不支持", "Unsupported platform")
    } else if status.profile_id.is_none() {
        tr(lang, "尚未配置组网", "No network configured")
    } else {
        tr(lang, "连接组网", "Connect")
    };
    let retrying = status.retrying;
    let profile_id = status.profile_id.clone();
    let button = status_toggle_button(
        SharedString::from(format!("tray-mesh-{}", provider.key())),
        button_active,
        disabled,
        tip,
        p,
        cx,
    )
    .on_click(cx.listener(move |this, _, window, cx| {
        if retrying {
            if let Some(id) = profile_id.clone() {
                this.action(
                    TrayAction::CancelMeshReconnect { profile_id: id },
                    window,
                    cx,
                );
            }
        } else if stoppable {
            this.action(TrayAction::DisconnectMesh(provider), window, cx);
        } else if let Some(id) = profile_id.clone() {
            this.action(TrayAction::ConnectMesh { profile_id: id }, window, cx);
        }
    }));
    let ip_text = status.ip.clone().unwrap_or_else(|| "-".to_string());
    let has_ip = status.ip.is_some();
    render_status_column(
        p,
        kicker_icon,
        title,
        dot_color,
        status_text,
        button,
        ip_text,
        has_ip,
        cx,
    )
}

#[derive(Default)]
enum NodeScrollHint {
    #[default]
    Idle,
    Waiting {
        _task: Task<()>,
    },
    Opening,
    AlignSelection,
    Start,
    Moving {
        began: Instant,
        origin: Pixels,
        distance: Pixels,
        last: Pixels,
    },
}

pub struct PopupView {
    service: AppService,
    dashboard: Dashboard,
    focus: FocusHandle,
    selected: usize,
    visible: bool,
    deactivated_at: Option<Instant>,
    scroll: ScrollHandle,
    scroll_hint: NodeScrollHint,
    route_select: Entity<SelectState<Vec<RouteOption>>>,
    _subscriptions: Vec<Subscription>,
}

impl PopupView {
    fn new(
        service: AppService,
        dashboard: Dashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() && !super::backend::pointer_on_icon() {
                this.dismiss(window);
                this.deactivated_at = Some(Instant::now());
            }
        });
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |view, _| view.dismiss(window));
            false
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.service.settings().theme == ThemeMode::System {
                crate::window_effects::sync_system_appearance(window, cx);
                crate::window_effects::apply(window, cx);
            }
            cx.notify();
        });
        let route_select = cx.new(|cx| {
            let mut state = SelectState::new(route_options(dashboard.language), None, window, cx);
            state.set_selected_value(&dashboard.route_preset, window, cx);
            state
        });
        let route_selection =
            cx.subscribe_in(&route_select, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(preset)) = event {
                    this.action(TrayAction::SetRoutePreset(*preset), window, cx);
                }
            });
        let selected = dashboard
            .nodes
            .iter()
            .position(|node| node.active)
            .unwrap_or(0);
        Self {
            service,
            dashboard,
            focus,
            selected,
            visible: true,
            deactivated_at: None,
            scroll: ScrollHandle::new(),
            scroll_hint: NodeScrollHint::Idle,
            route_select,
            _subscriptions: vec![activation, appearance, route_selection],
        }
    }

    fn refresh_dashboard(
        &mut self,
        dashboard: Dashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language_changed = dashboard.language != self.dashboard.language;
        let selection_changed =
            self.route_select.read(cx).selected_value() != Some(&dashboard.route_preset);
        if language_changed || selection_changed {
            self.route_select.update(cx, |state, cx| {
                if language_changed {
                    state.set_items(route_options(dashboard.language), window, cx);
                }
                state.set_selected_value(&dashboard.route_preset, window, cx);
            });
        }
        self.dashboard = dashboard;
    }

    fn show(&mut self, window: &mut Window, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let (shown, completion) = tokio::sync::oneshot::channel();
        // Dismissing, interacting or reopening drops this opening's pending task.
        self.scroll_hint = NodeScrollHint::Waiting {
            _task: cx.spawn(async move |view, cx| {
                if completion.await.is_ok() {
                    let _ = view.update(cx, |view, cx| {
                        view.scroll_hint = NodeScrollHint::Opening;
                        cx.notify();
                    });
                }
            }),
        };
        super::backend::show_popup(window, bounds, shown);
    }

    fn advance_scroll_hint(&mut self, window: &Window, cx: &mut Context<Self>) {
        use NodeScrollHint::*;
        if !self.visible || !window.is_visible() {
            return;
        }
        let offset = self.scroll.offset();
        self.scroll_hint = match self.scroll_hint {
            Idle | Waiting { .. } => return,
            // First lay out the real viewport, then bring the selected row into view.
            Opening => AlignSelection,
            AlignSelection => {
                self.scroll.scroll_to_item(self.selected);
                if cx.reduce_motion() || self.scroll.max_offset().y <= Pixels::ZERO {
                    Idle
                } else {
                    Waiting {
                        _task: cx.spawn(async move |view, cx| {
                            cx.background_executor().timer(NODE_SCROLL_HINT_DELAY).await;
                            let _ = view.update(cx, |view, cx| {
                                view.scroll_hint = Start;
                                cx.notify();
                            });
                        }),
                    }
                }
            }
            Start => {
                let range = self.scroll.max_offset().y;
                let row = px(NODE_ROW_HEIGHT);
                if cx.reduce_motion() || range <= Pixels::ZERO {
                    Idle
                } else {
                    let down = (range + offset.y).min(row);
                    // At the bottom, reveal the preceding row and return to the same origin.
                    let distance = if down > Pixels::ZERO {
                        -down
                    } else {
                        (-offset.y).min(row)
                    };
                    Moving {
                        began: Instant::now(),
                        origin: offset.y,
                        distance,
                        last: offset.y,
                    }
                }
            }
            Moving {
                began,
                origin,
                distance,
                last,
            } => {
                if offset.y != last {
                    Idle
                } else {
                    let elapsed = began.elapsed().as_secs_f32();
                    // A cosine round trip has zero velocity at the start, turn and finish.
                    let progress = (elapsed / 0.36).min(1.);
                    let finished = progress >= 1. || cx.reduce_motion();
                    let y = if finished {
                        origin
                    } else {
                        origin + distance * (0.5 - 0.5 * (progress * std::f32::consts::TAU).cos())
                    };
                    self.scroll.set_offset(point(offset.x, y));
                    if finished {
                        Idle
                    } else {
                        Moving {
                            began,
                            origin,
                            distance,
                            last: y,
                        }
                    }
                }
            }
        };
        if !matches!(self.scroll_hint, Idle | Waiting { .. }) {
            window.request_animation_frame();
        }
    }

    fn dismiss(&mut self, window: &mut Window) {
        if let NodeScrollHint::Moving { origin, last, .. } = self.scroll_hint {
            let offset = self.scroll.offset();
            if offset.y == last {
                self.scroll.set_offset(point(offset.x, origin));
            }
        }
        self.scroll_hint = NodeScrollHint::Idle;
        self.visible = false;
        self.deactivated_at = None;
        super::backend::hide_popup(window);
    }

    fn action(&mut self, action: TrayAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = dashboard::dispatch(action, &self.service) {
            super::queue_command(command);
            #[cfg(target_os = "linux")]
            if command.shows_main_window() {
                // Keep the requesting surface alive until the main window has asked for focus.
                return;
            }
            self.dismiss(window);
        } else {
            self.refresh_dashboard(dashboard::build(&self.service), window, cx);
            cx.notify();
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.dashboard.busy_core {
            return;
        }
        let Some(node) = self.dashboard.nodes.get(index) else {
            return;
        };
        let Some(action) = node.action.clone() else {
            return;
        };
        self.selected = index;
        window.focus(&self.focus, cx);
        self.action(action, window, cx);
    }
}

impl Render for PopupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.advance_scroll_hint(window, cx);
        let p = palette(cx);
        let d = &self.dashboard;
        let lang = d.language;
        let running = d.running;
        let node_rows = d
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let delay = node
                    .delay_ms
                    .map(|ms| format!("HTTP {ms} ms"))
                    .unwrap_or_else(|| "—".into());
                let delay_color = crate::ui::theme::latency_color_ms(&p, node.delay_ms);
                div()
                    .id(("tray-node", index))
                    .border_1()
                    .border_color(if index == self.selected {
                        p.primary.opacity(0.5)
                    } else {
                        gpui_kit::hsla(0., 0., 0., 0.)
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(NODE_ROW_HEIGHT))
                    .px_2()
                    .rounded_md()
                    .bg(if node.active {
                        p.accent.opacity(0.65)
                    } else {
                        gpui_kit::hsla(0., 0., 0., 0.)
                    })
                    .when(node.action.is_some() && !d.busy_core, |row| {
                        row.hover(|s| s.bg(p.accent)).cursor_pointer().on_click(
                            cx.listener(move |this, _, window, cx| this.select(index, window, cx)),
                        )
                    })
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_color(p.primary)
                            .child(if node.active { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_xs()
                            .child(node.label.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(delay_color)
                            .when(node.id != DIRECT_VIRTUAL_NODE_ID, |cell| cell.child(delay)),
                    )
            })
            .collect::<Vec<_>>();
        let translucent = crate::window_effects::blur_backdrop();
        #[cfg(target_os = "linux")]
        let corner_radius = rems(0.75).to_pixels(window.rem_size());
        #[cfg(target_os = "linux")]
        if cx.compositor_name() == "Wayland"
            && !matches!(window.window_decorations(), Decorations::Client { .. })
        {
            window.set_client_corner_radii(Corners::all(corner_radius));
        }
        let surface = if crate::window_effects::uses_mica() {
            gpui_kit::hsla(0., 0., 0., 0.)
        } else if cfg!(target_os = "linux") || !translucent {
            p.sidebar()
        } else {
            p.panel.opacity(if p.is_dark { 0.30 } else { 0.25 })
        };
        let card = if translucent {
            p.bg.opacity(if p.is_dark { 0.18 } else { 0.35 })
        } else {
            p.bg
        };
        div()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .bg(surface)
            // DWM and the Linux client frame already own the outer contour.
            .when(
                !cfg!(windows)
                    && !matches!(window.window_decorations(), Decorations::Client { .. }),
                |surface| {
                    #[cfg(target_os = "linux")]
                    let surface = surface.rounded(corner_radius);
                    #[cfg(not(target_os = "linux"))]
                    let surface = surface.rounded_xl();
                    surface.border_1().border_color(p.border)
                },
            )
            .text_color(p.fg)
            .text_xs()
            .capture_any_mouse_down(
                cx.listener(|this, _, _, _| this.scroll_hint = NodeScrollHint::Idle),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.scroll_hint = NodeScrollHint::Idle;
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss(window),
                    "down" if this.focus.is_focused(window) => {
                        this.selected = (this.selected + 1).min(
                            this.dashboard
                                .nodes
                                .iter()
                                .filter(|node| !node.id.starts_with("__xrose_"))
                                .count(),
                        );
                        this.scroll.scroll_to_item(this.selected);
                        cx.notify();
                    }
                    "up" if this.focus.is_focused(window) => {
                        this.selected = this.selected.saturating_sub(1);
                        this.scroll.scroll_to_item(this.selected);
                        cx.notify();
                    }
                    "enter" if this.focus.is_focused(window) => {
                        this.select(this.selected, window, cx)
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(img("app-icon-64.png").size(px(30.)).flex_shrink_0())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_base()
                            .child("Xrose"),
                    )
                    .child({
                        let any_active = d.any_service_active();
                        let power_tip = tr(
                            lang,
                            if d.busy_power {
                                "处理中…"
                            } else if any_active {
                                "停止代理与组网"
                            } else {
                                "启动代理与组网"
                            },
                            if d.busy_power {
                                "Working…"
                            } else if any_active {
                                "Stop proxy & mesh"
                            } else {
                                "Start proxy & mesh"
                            },
                        );
                        Button::new("tray-power")
                            .compact()
                            .custom(icon_button_style(&p, any_active, cx))
                            .toggled(any_active)
                            .icon(IconName::Power)
                            .tooltip(power_tip.clone())
                            .accessibility_label(power_tip)
                            .disabled(d.busy_power)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ToggleAll, window, cx)
                            }))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .bg(card)
                    .child(div().flex().gap_3().children([
                        {
                            let status_color = if running { p.success } else { p.muted_fg };
                            let status_text = tr(
                                lang,
                                if running { "运行中" } else { "已停止" },
                                if running { "Running" } else { "Stopped" },
                            );
                            let core_tip = tr(
                                lang,
                                if d.busy_core {
                                    "处理中…"
                                } else if running {
                                    "停止内核"
                                } else {
                                    "启动内核"
                                },
                                if d.busy_core {
                                    "Working…"
                                } else if running {
                                    "Stop core"
                                } else {
                                    "Start core"
                                },
                            );
                            let core_toggle = status_toggle_button(
                                "tray-core",
                                running,
                                d.busy_core,
                                core_tip,
                                &p,
                                cx,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.action(TrayAction::ToggleCore, window, cx)
                                },
                            ));
                            render_status_column(
                                &p,
                                IconName::Server,
                                d.core_name.clone(),
                                status_color,
                                status_text,
                                core_toggle,
                                d.proxy_address.clone(),
                                running,
                                cx,
                            )
                        },
                        mesh_status_column(
                            &d.easytier,
                            d.busy_mesh,
                            d.mesh_supported,
                            IconName::Network,
                            "EasyTier",
                            &p,
                            lang,
                            cx,
                        ),
                        mesh_status_column(
                            &d.tailscale,
                            d.busy_mesh,
                            d.mesh_supported,
                            IconName::Globe,
                            "Tailscale",
                            &p,
                            lang,
                            cx,
                        ),
                    ]))
                    .child(
                        div().flex().gap_3().children(
                            [
                                (
                                    IconName::ArrowDown,
                                    tr(lang, "下载", "Download"),
                                    format_speed(d.down_speed),
                                    p.data_down,
                                ),
                                (
                                    IconName::ArrowUp,
                                    tr(lang, "上传", "Upload"),
                                    format_speed(d.up_speed),
                                    p.data_up,
                                ),
                                (
                                    IconName::Sigma,
                                    tr(lang, "累计流量", "Total traffic"),
                                    d.total.clone(),
                                    p.fg,
                                ),
                            ]
                            .into_iter()
                            .map(|(icon, label, value, color)| {
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        traffic_label(icon, label)
                                            .text_size(px(11.))
                                            .text_color(p.muted_fg),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_xs()
                                            .text_color(color)
                                            .child(value),
                                    )
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .bg(card)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!(
                                        "{} · {}",
                                        tr(lang, "出站节点", "Outbound nodes"),
                                        d.group_name
                                    )),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(11.))
                                    .text_color(p.muted_fg)
                                    .child(format!("{}", d.nodes.len().saturating_sub(1))),
                            )
                            .child({
                                let test_tip =
                                    tr(lang, "全部出口 HTTP 测速", "HTTP test all exits");
                                Button::new("tray-test")
                                    .xsmall()
                                    .compact()
                                    .custom(icon_button_style(&p, false, cx).foreground(p.muted_fg))
                                    .icon(IconName::Gauge)
                                    .tooltip(test_tip.clone())
                                    .accessibility_label(test_tip)
                                    .disabled(d.busy_latency)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.action(TrayAction::TestAll, window, cx)
                                    }))
                            }),
                    )
                    .child(
                        div()
                            .id("tray-node-scroll")
                            .on_scroll(
                                cx.listener(|this, _, _, _| {
                                    this.scroll_hint = NodeScrollHint::Idle
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .children(node_rows)
                            .when((d.direct || d.mesh) && d.automatic_group, |s| {
                                s.child(
                                    Button::new("tray-resume-auto")
                                        .compact()
                                        .ghost()
                                        .disabled(d.busy_core)
                                        .label(tr(lang, "恢复自动代理", "Resume automatic proxy"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.action(
                                                TrayAction::SelectMode(WorkMode::Proxy),
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    )
                    .when(cx.reduce_motion(), |card| {
                        card.child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Always))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_xs()
                            .when(d.direct || d.mesh, |label| label.text_color(p.muted_fg))
                            .child(if d.direct {
                                tr(
                                    lang,
                                    "本机直连 · 代理分流停用",
                                    "Direct · Proxy routing off",
                                )
                            } else if d.mesh {
                                tr(lang, "组网 · 本地分流暂停", "Mesh · Local routing paused")
                            } else {
                                tr(lang, "路由模式", "Routing mode")
                            }),
                    )
                    .child(
                        div()
                            .w(px(match lang {
                                Language::Zh => 120.,
                                Language::En => 136.,
                            }))
                            .h(px(28.))
                            .flex_shrink_0()
                            .child(
                                Select::new(&self.route_select)
                                    .xsmall()
                                    .w_full()
                                    .h_full()
                                    .menu_width(px(160.))
                                    .menu_max_h(px(190.))
                                    .disabled(d.busy_core || d.direct || d.mesh)
                                    .accessibility_label(tr(lang, "路由模式", "Routing mode")),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(div().flex_1().min_w(px(0.)).text_xs().child(tr(
                        lang,
                        "系统代理",
                        "System proxy",
                    )))
                    .when_some(
                        d.system_proxy_status
                            .filter(|_| !d.busy_proxy)
                            .and_then(|status| crate::i18n::proxy_status_label(lang, status)),
                        |row, label| row.child(div().text_xs().text_color(p.muted_fg).child(label)),
                    )
                    .when(d.busy_proxy, |row| {
                        row.child(gpui_kit::component::spinner::Spinner::new().small())
                    })
                    .child(
                        Switch::new("tray-proxy")
                            .checked(d.system_proxy)
                            .disabled(d.busy_proxy || d.mesh)
                            .on_change(cx.listener(|this, checked, window, cx| {
                                this.action(TrayAction::SetSystemProxy(*checked), window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(div().flex_1().min_w(px(0.)).text_xs().child("TUN"))
                    .when(!d.tun_supported, |row| {
                        row.child(div().text_size(px(11.)).text_color(p.muted_fg).child(tr(
                            lang,
                            "内核不支持",
                            "Unsupported core",
                        )))
                    })
                    .child(
                        Switch::new("tray-tun")
                            .checked(d.tun_enabled)
                            .disabled(!d.tun_supported || d.busy_core || d.mesh)
                            .on_change(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ToggleTun, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_shrink_0()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        Button::new("tray-show")
                            .xsmall()
                            .h(px(28.))
                            .compact()
                            .ghost()
                            .label(tr(lang, "显示主窗口", "Show window"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ShowWindow, window, cx)
                            })),
                    )
                    .child(
                        Button::new("tray-quit")
                            .xsmall()
                            .h(px(28.))
                            .compact()
                            .ghost()
                            .icon(IconName::LogOut)
                            .label(tr(lang, "退出", "Quit"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::Quit, window, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::popup_bounds;
    use gpui_kit::{Bounds, point, px, size};
    #[test]
    fn placement_stays_inside_work_area_at_every_edge() {
        let work = Bounds::new(point(px(-1600.), px(-200.)), size(px(1600.), px(860.)));
        for at in [
            work.origin,
            work.bottom_right(),
            point(px(-800.), px(-200.)),
            point(px(-800.), px(660.)),
        ] {
            let b = popup_bounds(Some(at), work, size(px(400.), px(600.)));
            assert!(b.left() >= work.left() && b.right() <= work.right());
            assert!(b.top() >= work.top() && b.bottom() <= work.bottom());
        }
    }
    #[test]
    fn small_screens_shrink_the_flyout() {
        let work = Bounds::new(point(px(0.), px(0.)), size(px(320.), px(480.)));
        let b = popup_bounds(None, work, size(px(400.), px(620.)));
        assert_eq!(b.size, size(px(304.), px(464.)));
    }
}
