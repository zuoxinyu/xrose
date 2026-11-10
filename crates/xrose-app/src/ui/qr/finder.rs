//! Live desktop viewfinder. Only the area near the pointer is sampled.
use super::{
    decode::{Code, Input},
    platform::{self, DesktopInput, SelectionSource},
    sample::{
        Rect, SNAP_DISTANCE, ScanSummary, analysis_region, brackets, decode_snapshot, inside,
        scan_desktop, scan_summary, target_index,
    },
};
use crate::ui::{
    AppView,
    theme::{DIALOG_WIDE, PAGE_PAD, Palette, finder_brand_color, palette},
    tr,
};
use gpui_kit::{prelude::FluentBuilder, *};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use xrose_core::{AppService, settings::Language};

const DEFAULT_SIZE: f32 = 280.;
const MIN_SIZE: f32 = 120.;
const MAX_SIZE: f32 = 800.;
const SETTLE: Duration = Duration::from_millis(140);
const PRESS_HOLD: Duration = Duration::from_millis(120);
type Outcome = Result<Option<(String, SelectionSource)>, platform::CaptureError>;
type OnFinish = Box<dyn FnOnce(Outcome, &mut App)>;

/// One completion and zoom level shared by all display overlays.
struct Session {
    windows: RefCell<Vec<AnyWindowHandle>>,
    finish: RefCell<Option<OnFinish>>,
    size: Cell<f32>,
}

impl Session {
    fn finish(&self, outcome: Outcome, cx: &mut App) {
        let Some(callback) = self.finish.take() else {
            return;
        };
        let windows = self.windows.take();
        cx.defer(move |cx| {
            for handle in windows {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            callback(outcome, cx);
        });
    }
}

struct Finder {
    #[cfg(target_os = "linux")]
    main: AnyWindowHandle,
    input: Option<DesktopInput>,
    codes: Vec<Code>,
    summaries: Vec<ScanSummary>,
    status_bottom: Pixels,
    pill_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    last_hit: Option<usize>,
    position: [f32; 2],
    session: Rc<Session>,
    active: bool,
    pressed: bool,
    last_press: Option<Instant>,
    crosshair_scale: f32,
    rect: Rect,
    focus: FocusHandle,
    palette: Palette,
    lang: Language,
    privacy: bool,
    target: platform::CaptureTarget,
    revision: u32,
    generation: u64,
    scan_at: Option<Instant>,
    sampled_at: Option<[f32; 2]>,
    snapshot: Option<(Instant, Arc<image::RgbaImage>)>,
    in_flight: bool,
    last_frame: Instant,
    opened: Instant,
}

impl AppView {
    pub(super) fn start_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scan_pending || self.import_busy {
            return;
        }
        let main = window.window_handle();
        let owner = cx.weak_entity();
        let lang = self.lang();
        let colors = palette(cx);
        let privacy = self.hide_ips;
        #[cfg(target_os = "macos")]
        platform::main_visible(window, false);
        #[cfg(windows)]
        crate::tray::hide_main_window();
        #[cfg(not(any(windows, target_os = "macos")))]
        window.minimize_window();
        self.scan_pending = true;
        let session = Rc::new(Session {
            windows: Default::default(),
            size: Cell::new(DEFAULT_SIZE),
            finish: RefCell::new(Some(Box::new(move |outcome, cx| {
                let _ = main.update(cx, |_, window, cx| {
                    restore(window, cx);
                    let _ = owner.update(cx, |view, cx| {
                        view.scan_pending = false;
                        match outcome {
                            Ok(Some((text, source))) => {
                                let mode = match source {
                                    SelectionSource::Click => super::ImportMode::ClickedCode,
                                    SelectionSource::Keyboard => super::ImportMode::Apply,
                                };
                                view.process_import(vec![Input::Text(text)], mode, window, cx)
                            }
                            Err((zh, en)) => {
                                view.import_problem(tr(lang, zh, en).to_string(), window, cx)
                            }
                            Ok(None) => {}
                        }
                        window.refresh();
                        cx.notify();
                    });
                });
            }))),
        });
        for (id, bounds, visible_bottom, target) in platform::overlays(cx) {
            let status_bottom = (bounds.bottom() - visible_bottom).max(px(0.))
                + (bounds.size.height * 0.12).clamp(px(PAGE_PAD), px(6. * PAGE_PAD));
            let shared = session.clone();
            let options = WindowOptions {
                display_id: Some(id),
                #[cfg(target_os = "linux")]
                app_id: Some(crate::APP_ID.into()),
                window_bounds: Some(
                    if cfg!(target_os = "linux") && cx.compositor_name() == "Wayland" {
                        WindowBounds::Fullscreen(bounds)
                    } else {
                        WindowBounds::Windowed(bounds)
                    },
                ),
                kind: WindowKind::PopUp,
                focus: false,
                titlebar: None,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            };
            let opened = cx.open_window(options, move |window, cx| {
                cx.new(|cx| {
                    let input = DesktopInput::new(window, id);
                    let focus = cx.focus_handle();
                    if input.is_none() {
                        cx.observe_window_activation(window, |finder: &mut Finder, _, cx| {
                            finder.invalidate();
                            cx.notify();
                        })
                        .detach();
                    }
                    let now = Instant::now();
                    let pos = window.mouse_position();
                    let position = [pos.x.into(), pos.y.into()];
                    let session_for_displays = Rc::downgrade(&shared);
                    let displays_changed = cx.observe_displays(move |_, cx| {
                        if let Some(session) = session_for_displays.upgrade() {
                            session.finish(Ok(None), cx);
                        }
                    });
                    cx.on_release(move |finder: &mut Finder, cx| {
                        drop(displays_changed);
                        drop(finder.input.take());
                        finder.session.finish(Ok(None), cx);
                    })
                    .detach();
                    let weak = cx.weak_entity();
                    window.on_next_frame(move |window, cx| {
                        let _ = weak.update(cx, |_: &mut Finder, cx| {
                            // The first animation frame is already visible. Keep at most one capture in flight.
                            cx.spawn_in(window, async move |finder, cx| {
                                loop {
                                    cx.background_executor()
                                        .timer(Duration::from_millis(17))
                                        .await;
                                    if finder
                                        .update_in(cx, |s, window, cx| s.tick(window, cx))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            })
                            .detach();
                        });
                    });
                    Finder {
                        #[cfg(target_os = "linux")]
                        main,
                        input,
                        codes: vec![],
                        summaries: vec![],
                        status_bottom,
                        pill_bounds: Default::default(),
                        last_hit: None,
                        position,
                        session: shared,
                        active: false,
                        pressed: false,
                        last_press: None,
                        crosshair_scale: 1.,
                        rect: [
                            position[0] - DEFAULT_SIZE / 2.,
                            position[1] - DEFAULT_SIZE / 2.,
                            DEFAULT_SIZE,
                            DEFAULT_SIZE,
                        ],
                        focus,
                        palette: colors,
                        lang,
                        privacy,
                        target,
                        revision: 0,
                        generation: 0,
                        scan_at: None,
                        sampled_at: None,
                        snapshot: None,
                        in_flight: false,
                        last_frame: now,
                        opened: now,
                    }
                })
            });
            match opened {
                Ok(handle) => session.windows.borrow_mut().push(handle.into()),
                Err(_) => {
                    session.finish(
                        Err((
                            "无法打开全屏取景器，请重试",
                            "Could not open the viewfinder. Try again.",
                        )),
                        cx,
                    );
                    break;
                }
            }
        }
        if session.windows.borrow().is_empty() {
            session.finish(
                Err(("无法获取当前屏幕", "The current display is unavailable.")),
                cx,
            );
        }
        cx.notify();
    }
}

fn restore(window: &mut Window, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        platform::main_visible(window, true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::tray::show_main_window();
        window.activate_window();
        cx.activate(true);
    }
}

impl Finder {
    fn close(&self, outcome: Outcome, cx: &mut App) {
        // Request focus while the surface carrying the last input serial still exists.
        #[cfg(target_os = "linux")]
        if cx.compositor_name() == "Wayland" {
            let _ = self.main.update(cx, |_, main, _| main.activate_window());
        }
        self.session.finish(outcome, cx);
    }

    fn invalidate(&mut self) {
        self.snapshot = None;
        self.resample();
    }

    fn resample(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(input) = &self.input {
            input.set_hit(false);
        }
        self.last_hit = None;
        self.codes.clear();
        self.summaries.clear();
        self.scan_at = Some(Instant::now() + SETTLE);
    }
    fn move_to(&mut self, pos: [f32; 2]) {
        self.position = pos;
        if self
            .sampled_at
            .is_some_and(|old| (old[0] - pos[0]).hypot(old[1] - pos[1]) > SNAP_DISTANCE)
        {
            self.sampled_at = Some(pos);
            // Moving changes the search region, not the previously decoded screen coordinates.
            self.generation = self.generation.wrapping_add(1);
            self.scan_at = Some(Instant::now() + SETTLE);
        }
    }
    fn zoom(&mut self, delta: f32) {
        let old = self.session.size.get();
        let new = (old + delta * 28.).clamp(MIN_SIZE, MAX_SIZE);
        if new != old {
            self.session.size.set(new);
            self.resample();
        }
    }

    fn on_pill(&self, pos: [f32; 2]) -> bool {
        self.pill_bounds
            .get()
            .is_some_and(|b| b.contains(&point(px(pos[0]), px(pos[1]))))
    }

    fn target_at(&self, pos: [f32; 2], dims: [f32; 2]) -> Option<(usize, Rect)> {
        let pill = self.last_hit.filter(|_| self.on_pill(pos));
        let i = target_index(&self.codes, pos, dims, self.session.size.get(), pill)?;
        Some((i, brackets(&self.codes[i], dims)))
    }

    fn select(&mut self, pos: [f32; 2], dims: [f32; 2], source: SelectionSource, cx: &mut App) {
        self.last_press = Some(Instant::now());
        if let Some((i, _)) = self.target_at(pos, dims) {
            self.close(Ok(Some((self.codes[i].text.clone(), source))), cx);
        }
    }
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.finish.borrow().is_none() {
            return;
        }
        let viewport = window.viewport_size();
        let dims = [f32::from(viewport.width), f32::from(viewport.height)];
        let sample = self.input.as_mut().map(DesktopInput::sample);
        if sample.as_ref().is_some_and(|s| s.cancel) {
            self.close(Ok(None), cx);
            return;
        }
        let pos = sample.as_ref().map(|s| s.position).unwrap_or_else(|| {
            let p = window.mouse_position();
            [p.x.into(), p.y.into()]
        });
        let active = inside(pos, dims) && (sample.is_some() || window.is_window_hovered());
        if self.active != active {
            self.active = active;
            self.invalidate();
            self.sampled_at = None;
            self.pressed = false;
            if active {
                let side = self.session.size.get();
                self.rect = [pos[0] - side / 2., pos[1] - side / 2., side, side];
                // Let the new panel settle and receive cancellation before starting capture.
                if self.input.is_none() {
                    window.activate_window();
                    self.focus.focus(window, cx);
                    cx.hide_cursor_until_mouse_moves();
                }
            }
            cx.notify();
        }
        if !active {
            return;
        }
        if self.position != pos {
            self.move_to(pos);
            cx.notify();
        }
        if let Some(sample) = sample {
            if self.revision != sample.revision {
                self.revision = sample.revision;
                self.invalidate();
                cx.notify();
            }
            if self.pressed != sample.pressed {
                self.pressed = sample.pressed;
                cx.notify();
            }
            if sample.zoom_delta != 0. {
                self.zoom(sample.zoom_delta);
                cx.notify();
            }
            if let Some((pos, source)) = sample.selection.filter(|(p, _)| inside(*p, dims)) {
                self.select(pos, dims, source, cx);
                cx.notify();
                return;
            }
        }
        if !cx.reduce_motion() && self.last_frame.elapsed() >= Duration::from_millis(32) {
            cx.notify();
        }
        if self.last_hit.is_some() && self.on_pill(self.position) {
            return;
        }
        if self.in_flight || !self.scan_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        let cache_display = cfg!(windows) || platform::uses_portal();
        let cached = self
            .snapshot
            .take()
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(2));
        #[cfg(windows)]
        if cached.is_none()
            && let Some(input) = &self.input
        {
            input.exclude_from_capture(true);
        }
        self.in_flight = true;
        self.scan_at = None;
        self.sampled_at = Some(self.position);
        let region = analysis_region(self.position, dims, self.session.size.get());
        let (target, generation, privacy, lang) =
            (self.target, self.generation, self.privacy, self.lang);
        cx.spawn_in(window, async move |view, cx| {
            let timer = cx.background_executor().timer(Duration::from_secs(30));
            let scan = async {
                // WGC and Wayland already take display snapshots. Reuse one for nearby crops;
                // macOS can request a high-resolution region directly from ScreenCaptureKit.
                let snapshot = if cache_display && cached.is_none() {
                    let pixels = cx
                        .background_executor()
                        .spawn(platform::capture(target, [0., 0., 1., 1.]))
                        .await?;
                    #[cfg(windows)]
                    let _ = view.update_in(cx, |finder, _, _| {
                        if let Some(input) = &finder.input {
                            input.exclude_from_capture(false);
                        }
                    });
                    Some((Instant::now(), Arc::new(pixels)))
                } else {
                    cached
                };
                cx.background_executor()
                    .spawn(async move {
                        let codes = if let Some((_, image)) = &snapshot {
                            decode_snapshot(image, region)
                        } else {
                            scan_desktop(target, region).await?
                        };
                        let summaries = codes
                            .iter()
                            .map(|code| {
                                scan_summary(
                                    AppService::preview_shared_content(&code.text),
                                    privacy,
                                    lang,
                                )
                            })
                            .collect();
                        Ok((codes, summaries, snapshot))
                    })
                    .await
            };
            let result = tokio::select! {
                result = scan => result,
                _ = timer => Err(("截屏超时，请重试", "Screen capture timed out. Try again.")),
            };
            let _ = view.update_in(cx, |finder, _window, cx| {
                #[cfg(windows)]
                if let Some(input) = &finder.input {
                    input.exclude_from_capture(false);
                }
                finder.in_flight = false;
                if finder.generation != generation || !finder.active {
                    return;
                }
                match result {
                    Err(error) => finder.close(Err(error), cx),
                    Ok((codes, summaries, snapshot)) => {
                        finder.snapshot = snapshot;
                        finder.last_hit = None;
                        finder.codes = codes;
                        finder.summaries = summaries;
                        if finder.input.is_some() {
                            finder.scan_at = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Clockwise from top left. Each arm starts at the same color at its vertex.
fn frame_corner(index: usize, flow: f32, alpha: f32, arm: f32, stroke: f32) -> Div {
    let top = index < 2;
    let left = matches!(index, 0 | 3);
    let phase = index as f32 * 0.25 - flow;
    let span = if top == left { 0.14 } else { -0.14 };
    let gradient = |angle, offset| {
        linear_gradient(
            angle,
            linear_color_stop(finder_brand_color(phase, alpha), 0.),
            linear_color_stop(finder_brand_color(phase + offset, alpha), 1.),
        )
    };
    div()
        .absolute()
        .w(px(arm))
        .h(px(arm))
        .map(|d| if top { d.top_0() } else { d.bottom_0() })
        .map(|d| if left { d.left_0() } else { d.right_0() })
        .child(
            div()
                .absolute()
                .w_full()
                .h(px(stroke))
                .map(|d| if top { d.top_0() } else { d.bottom_0() })
                .bg(gradient(if left { 90. } else { 270. }, span)),
        )
        .child(
            div()
                .absolute()
                .w(px(stroke))
                .h(px((arm - stroke).max(0.)))
                .map(|d| {
                    if top {
                        d.top(px(stroke))
                    } else {
                        d.bottom(px(stroke))
                    }
                })
                .map(|d| if left { d.left_0() } else { d.right_0() })
                .bg(gradient(if top { 180. } else { 0. }, -span)),
        )
}

impl Render for Finder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let dims = [size.width.into(), size.height.into()];
        let side = self.session.size.get();
        let hit = self.target_at(self.position, dims);
        if let Some((i, _)) = hit {
            self.last_hit = Some(i);
        }
        let pill_bounds = self.pill_bounds.clone();
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(input) = &self.input {
            input.set_hit(self.active && hit.is_some());
        }
        let target = hit.map(|(_, rect)| rect).unwrap_or([
            self.position[0] - side / 2.,
            self.position[1] - side / 2.,
            side,
            side,
        ]);
        let now = Instant::now();
        let animate = !cx.reduce_motion();
        let ease = if animate {
            1. - (-25. * (now - self.last_frame).as_secs_f32()).exp()
        } else {
            1.
        };
        let press = self.pressed || self.last_press.is_some_and(|t| t.elapsed() < PRESS_HOLD);
        let scale = if press { 0.62 } else { 1. };
        self.crosshair_scale += (scale - self.crosshair_scale) * ease;
        self.last_frame = now;
        for (v, to) in self.rect.iter_mut().zip(target) {
            *v = if (*v - to).abs() < 0.4 {
                to
            } else {
                *v + (to - *v) * ease
            };
        }
        let age = self.opened.elapsed().as_secs_f32();
        let entrance = if animate {
            (1. - (age / 0.2).min(1.)).powi(2) * 14.
        } else {
            0.
        };
        let breath = if hit.is_some() || !animate {
            0.
        } else {
            2. * (age * std::f32::consts::PI).sin()
        };
        let offset = entrance + breath;
        let [l, t, w, h] = self.rect;
        let flow = if animate { age * 0.55 } else { 0. };
        let crosshair_color = finder_brand_color(flow, 1.);
        let (stroke, arm, alpha) = if hit.is_some() {
            let alpha = if animate {
                0.85 + 0.15 * (age * std::f32::consts::TAU).cos()
            } else {
                1.
            };
            (4., (w.min(h) / 3.).min(36.), alpha)
        } else {
            (3., 26. + breath, 1.)
        };
        let name = hit.and_then(|(i, _)| self.summaries[i].name.clone());
        let status = if let Some((i, _)) = hit {
            self.summaries[i].detail.clone()
        } else if self.in_flight || self.sampled_at.is_none() {
            tr(self.lang, "正在识别二维码", "Looking for a QR code")
        } else {
            tr(
                self.lang,
                "移动取景框对准二维码",
                "Move the frame over a QR code",
            )
        };
        let spoken = name
            .as_ref()
            .map(|n| format!("{n} · {status}").into())
            .unwrap_or_else(|| status.clone());
        let zoom_hint = if cfg!(target_os = "macos") {
            tr(self.lang, "⌘ + 滚轮缩放", "⌘ + wheel to resize")
        } else {
            tr(self.lang, "Ctrl + 滚轮缩放", "Ctrl + wheel to resize")
        };
        let action_hint = if hit.is_some() {
            tr(
                self.lang,
                "点击或按回车导入 · 右键或 Esc 退出",
                "Click or press Enter to import · Right-click or Esc to exit",
            )
        } else {
            tr(self.lang, "右键或 Esc 退出", "Right-click or Esc to exit")
        };
        let hint = SharedString::from(format!("{zoom_hint} · {action_hint}"));
        if self.active && animate && (self.rect != target || entrance > 0.) {
            window.request_animation_frame();
        }
        let surface = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(move |s, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    s.close(Ok(None), cx);
                } else if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    s.select(s.position, dims, SelectionSource::Keyboard, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|s, _, _, cx| {
                    cx.stop_propagation();
                    s.close(Ok(None), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, event: &MouseDownEvent, _, cx| {
                    if s.input.is_none() {
                        let pos = [event.position.x.into(), event.position.y.into()];
                        s.move_to(pos);
                        s.pressed = true;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |s, event: &MouseUpEvent, _, cx| {
                    if s.input.is_none() && s.pressed {
                        s.select(
                            [event.position.x.into(), event.position.y.into()],
                            dims,
                            SelectionSource::Click,
                            cx,
                        );
                    }
                    s.pressed = false;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|s, event: &MouseMoveEvent, _, cx| {
                if s.input.is_none() {
                    cx.hide_cursor_until_mouse_moves();
                    s.move_to([event.position.x.into(), event.position.y.into()]);
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(|s, event: &ScrollEvent, window, cx| {
                if s.input.is_none() {
                    if if cfg!(target_os = "macos") {
                        event.modifiers.platform
                    } else {
                        event.modifiers.control
                    } {
                        let delta = match event.delta {
                            ScrollDelta::Lines(l) => l.y,
                            ScrollDelta::Pixels(p) => f32::from(p.y) / 20.,
                        };
                        s.zoom(delta);
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    s.invalidate();
                    // Return the pointer to the underlying Wayland surface for the gesture, then sample its final position.
                    window.set_input_region(Some(&[]));
                    cx.spawn_in(window, async move |view, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(650))
                            .await;
                        let _ = view.update_in(cx, |s, win, cx| {
                            win.set_input_region(None);
                            s.invalidate();
                            cx.notify();
                        });
                    })
                    .detach();
                    cx.notify();
                }
            }));
        if !self.active {
            return surface;
        }
        surface
            .child(
                div()
                    .absolute()
                    .left(px(l - offset))
                    .top(px(t - offset))
                    .w(px(w + 2. * offset))
                    .h(px(h + 2. * offset))
                    .children((0..4).map(|i| frame_corner(i, flow, alpha, arm, stroke))),
            )
            .child({
                let [x, y] = self.position;
                let length = 24. * self.crosshair_scale;
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .child(
                        div()
                            .absolute()
                            .left(px(-length / 2.))
                            .top(px(-1.))
                            .w(px(length))
                            .h(px(2.))
                            .rounded_full()
                            .bg(crosshair_color),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(-1.))
                            .top(px(-length / 2.))
                            .w(px(2.))
                            .h(px(length))
                            .rounded_full()
                            .bg(crosshair_color),
                    )
            })
            .child(
                div()
                    .absolute()
                    .bottom(self.status_bottom)
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .on_children_prepainted(move |children, _, _| {
                        pill_bounds.set(children.first().copied())
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .max_w((size.width - px(2. * PAGE_PAD)).min(px(DIALOG_WIDE)))
                            .rounded_lg()
                            .id("scanner-status")
                            .role(Role::Status)
                            .aria_label(spoken)
                            .aria_description(hint.clone())
                            .gap_1()
                            .border_1()
                            .border_color(self.palette.border)
                            .shadow_lg()
                            .bg(self.palette.panel.opacity(0.95))
                            .text_color(self.palette.fg)
                            .px_4()
                            .py_2()
                            .text_sm()
                            .when(hit.is_some(), |d| {
                                d.child(
                                    div()
                                        .rounded_full()
                                        .px_2()
                                        .text_xs()
                                        .bg(self.palette.accent)
                                        .text_color(self.palette.primary)
                                        .child(tr(self.lang, "二维码", "QR Code")),
                                )
                            })
                            .when_some(name, |d, name| {
                                d.child(
                                    div()
                                        .max_w_full()
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(name),
                                )
                            })
                            .child(div().max_w_full().truncate().child(status))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(self.palette.muted_fg)
                                    .child(hint),
                            ),
                    ),
            )
    }
}
