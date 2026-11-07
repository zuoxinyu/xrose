//! Live desktop viewfinder. Only the area near the pointer is sampled.
use super::{
    decode::{self, Code, Input},
    platform::{self, DesktopInput},
};
use crate::ui::{
    AppView,
    theme::{DIALOG_WIDE, PAGE_PAD, Palette, palette},
    tr,
};
use gpui_kit::{prelude::FluentBuilder, *};
use std::time::{Duration, Instant};
use xrose_core::{AppService, service::SharePreview, settings::Language};

const SIZE: f32 = 300.;
const SNAP_DISTANCE: f32 = 82.;
const PADDING: f32 = 5.;
const SETTLE: Duration = Duration::from_millis(140);
const CLICK_CURSOR_TIME: Duration = Duration::from_millis(180);
type Rect = [f32; 4];

struct ScanSummary {
    name: Option<SharedString>,
    detail: SharedString,
}

pub(super) fn nearest(codes: &[Code], pos: [f32; 2], size: [f32; 2]) -> Option<(usize, Rect)> {
    codes
        .iter()
        .enumerate()
        .filter_map(|(i, code)| {
            let [l, t, r, b] = code.rect;
            let (x, y, w, h) = (
                l * size[0],
                t * size[1],
                (r - l) * size[0],
                (b - t) * size[1],
            );
            let d = (x + w / 2. - pos[0]).hypot(y + h / 2. - pos[1]);
            let inside = (x..=x + w).contains(&pos[0]) && (y..=y + h).contains(&pos[1]);
            (inside || d < SNAP_DISTANCE).then_some((
                d,
                i,
                [x - PADDING, y - PADDING, w + 2. * PADDING, h + 2. * PADDING],
            ))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i, rect)| (i, rect))
}

/// Normalized origin and size of a slightly larger region than the visible brackets.
fn analysis_region(position: [f32; 2], dims: [f32; 2]) -> Rect {
    let side = SIZE + SNAP_DISTANCE * 2.;
    let w = side.min(dims[0]);
    let h = side.min(dims[1]);
    [
        (position[0] - w / 2.).clamp(0., dims[0] - w) / dims[0],
        (position[1] - h / 2.).clamp(0., dims[1] - h) / dims[1],
        w / dims[0],
        h / dims[1],
    ]
}

struct Finder {
    input: Option<DesktopInput>,
    codes: Vec<Code>,
    summaries: Vec<ScanSummary>,
    status_bottom: Pixels,
    position: [f32; 2],
    click_cursor: Option<(Instant, [f32; 2])>,
    rect: Rect,
    focus: FocusHandle,
    palette: Palette,
    lang: Language,
    privacy: bool,
    display: DisplayId,
    center: (i32, i32),
    revision: u32,
    generation: u64,
    scan_at: Option<Instant>,
    sampled_at: Option<[f32; 2]>,
    in_flight: bool,
    last_frame: Instant,
    opened: Instant,
    selection: Option<String>,
    error: Option<platform::CaptureError>,
}

impl AppView {
    pub(super) fn start_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scan_pending || self.import_busy {
            return;
        }
        let Some(display) = window.display(cx) else {
            return self.import_problem(
                self.tr("无法获取当前屏幕", "The current display is unavailable.")
                    .to_string(),
                window,
                cx,
            );
        };
        let id = display.id();
        let bounds = display.bounds();
        let work_area = display.visible_bounds();
        let status_bottom = (bounds.bottom() - work_area.bottom()).max(px(0.))
            + (work_area.size.height * 0.12).clamp(px(PAGE_PAD), px(6. * PAGE_PAD));
        let center = (
            f32::from(bounds.center().x) as i32,
            f32::from(bounds.center().y) as i32,
        );
        let main = window.window_handle();
        let owner = cx.weak_entity();
        let lang = self.lang();
        let colors = palette(cx);
        let privacy = self.hide_ips;
        self.scan_pending = true;
        let options = WindowOptions {
            display_id: Some(id),
            window_bounds: Some(if cfg!(target_os = "linux") {
                WindowBounds::Fullscreen(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }),
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
        #[cfg(target_os = "macos")]
        platform::main_visible(window, false);
        #[cfg(windows)]
        crate::tray::hide_main_window();
        #[cfg(not(any(windows, target_os = "macos")))]
        window.minimize_window();
        let opened = cx.open_window(options, move |window, cx| {
            cx.new(|cx| {
                let input = DesktopInput::new(window, id);
                let focus = cx.focus_handle();
                if input.is_none() {
                    window.activate_window();
                    focus.focus(window, cx);
                    cx.hide_cursor_until_mouse_moves();
                    cx.observe_window_activation(window, |finder: &mut Finder, _, cx| {
                        finder.invalidate();
                        cx.notify();
                    })
                    .detach();
                }
                let now = Instant::now();
                let pos = window.mouse_position();
                let position = [pos.x.into(), pos.y.into()];
                cx.on_release(move |finder: &mut Finder, cx| {
                    drop(finder.input.take());
                    let selection = finder.selection.take();
                    let error = finder.error.take();
                    cx.defer(move |cx| {
                        let _ = main.update(cx, |_, window, cx| {
                            restore(window, cx);
                            let _ = owner.update(cx, |view, cx| {
                                view.scan_pending = false;
                                if let Some((zh, en)) = error {
                                    view.import_problem(tr(lang, zh, en).to_string(), window, cx);
                                } else if let Some(text) = selection {
                                    view.process_import(vec![Input::Text(text)], false, window, cx);
                                }
                                window.refresh();
                                cx.notify();
                            });
                        });
                    });
                })
                .detach();
                let weak = cx.weak_entity();
                window.on_next_frame(move |window, cx| {
                    let _ = weak.update(cx, |s: &mut Finder, cx| {
                        // The first animation frame is already visible. Keep at most one capture in flight.
                        s.scan_at = Some(Instant::now() + Duration::from_millis(60));
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
                    input,
                    codes: vec![],
                    summaries: vec![],
                    status_bottom,
                    position,
                    click_cursor: None,
                    rect: [position[0] - SIZE / 2., position[1] - SIZE / 2., SIZE, SIZE],
                    focus,
                    palette: colors,
                    lang,
                    privacy,
                    display: id,
                    center,
                    revision: 0,
                    generation: 0,
                    scan_at: None,
                    sampled_at: None,
                    in_flight: false,
                    last_frame: now,
                    opened: now,
                    selection: None,
                    error: None,
                }
            })
        });
        if opened.is_err() {
            restore(window, cx);
            self.scan_pending = false;
            self.import_problem(
                self.tr(
                    "无法打开全屏取景器，请重试",
                    "Could not open the viewfinder. Try again.",
                )
                .to_string(),
                window,
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
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
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
            self.invalidate();
        }
    }
    fn select(&mut self, pos: [f32; 2], window: &mut Window) {
        self.click_cursor = Some((Instant::now() + CLICK_CURSOR_TIME, pos));
        if let Some(input) = &mut self.input {
            input.set_cursor_visible(true);
        }
        let size = window.viewport_size();
        if let Some((i, _)) = nearest(&self.codes, pos, [size.width.into(), size.height.into()]) {
            self.selection = Some(self.codes[i].text.clone());
            window.remove_window();
        }
    }
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The stationary brackets pulse slowly. Use the existing input timer
        // for that animation, and reserve display-rate frames for tracking.
        if !cx.reduce_motion() && self.last_frame.elapsed() >= Duration::from_millis(32) {
            cx.notify();
        }
        if let Some(input) = &mut self.input {
            let sample = input.sample();
            if sample.cancel {
                window.remove_window();
                return;
            }
            if self.revision != sample.revision {
                self.revision = sample.revision;
                self.invalidate();
                cx.notify();
            }
            if self.position != sample.position {
                self.move_to(sample.position);
                cx.notify();
            }
            if let Some(click) = sample.clicked {
                self.select(click, window);
                cx.notify();
            }
        }
        if self
            .click_cursor
            .is_some_and(|(until, pos)| Instant::now() >= until || pos != self.position)
        {
            self.click_cursor = None;
            if let Some(input) = &mut self.input {
                input.set_cursor_visible(false);
            } else {
                cx.hide_cursor_until_mouse_moves();
            }
            cx.notify();
        }
        if self.in_flight || !self.scan_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        let size = window.viewport_size();
        let dims = [f32::from(size.width), f32::from(size.height)];
        if dims[0] < 1. || dims[1] < 1. {
            return;
        }
        self.in_flight = true;
        self.scan_at = None;
        self.sampled_at = Some(self.position);
        let region = analysis_region(self.position, dims);
        let (id, center, generation, privacy, lang) = (
            self.display,
            self.center,
            self.generation,
            self.privacy,
            self.lang,
        );
        cx.spawn_in(window, async move |view, cx| {
            let worker = cx
                .background_executor()
                .spawn(async move { scan_desktop(id, center, region, privacy, lang) });
            let result = tokio::select! {
                result = worker => result,
                _ = cx.background_executor().timer(Duration::from_secs(30)) =>
                    Err(("截屏超时，请重试", "Screen capture timed out. Try again.")),
            };
            let _ = view.update_in(cx, |finder, window, cx| {
                finder.in_flight = false;
                match result {
                    Err(error) => {
                        finder.error = Some(error);
                        window.remove_window();
                    }
                    Ok((codes, summaries)) if finder.generation == generation => {
                        finder.codes = codes;
                        finder.summaries = summaries;
                        if finder.input.is_some() {
                            finder.scan_at = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn scan_desktop(
    id: DisplayId,
    center: (i32, i32),
    region: Rect,
    privacy: bool,
    lang: Language,
) -> Result<(Vec<Code>, Vec<ScanSummary>), platform::CaptureError> {
    let local = platform::capture(id, center, region)?;
    let mut codes = decode::screen_codes(&local);
    for code in &mut codes {
        for (i, v) in code.rect.iter_mut().enumerate() {
            *v = region[i % 2] + *v * region[2 + i % 2];
        }
    }
    // A large QR can extend beyond all four brackets. Its whole-screen
    // thumbnail is sufficient to decode it without scanning a native 5K frame.
    if codes.is_empty() && region != [0., 0., 1., 1.] && !platform::uses_portal() {
        let overview = platform::capture(id, center, [0., 0., 1., 1.])?;
        codes = decode::screen_codes(&overview);
    }
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
    Ok((codes, summaries))
}

fn scan_summary(preview: SharePreview, privacy: bool, lang: Language) -> ScanSummary {
    let (name, detail) = match preview {
        SharePreview::Node {
            mut name,
            protocol,
            endpoint,
            count,
        } => {
            let endpoint = if privacy {
                let masked = super::super::nodes::format_endpoint(&endpoint, true);
                // Unnamed links often use the server address as part of their default name.
                if let Some((host, _)) = endpoint.rsplit_once(':') {
                    let host = host.trim_matches(['[', ']']);
                    if !host.is_empty() {
                        name = name.replace(host, "••••••");
                    }
                }
                masked
            } else {
                endpoint
            };
            let mut label = format!("{protocol} · {endpoint}");
            if count > 1 {
                label.push_str(&format!(" · {count} {}", tr(lang, "个节点", "nodes")));
            }
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            (
                (!name.is_empty()).then(|| name.into()),
                SharedString::from(label),
            )
        }
        SharePreview::Subscription { host } => (
            None,
            format!(
                "{} · {}",
                tr(lang, "节点订阅", "Subscription"),
                if privacy { "••••••" } else { &host }
            )
            .into(),
        ),
        SharePreview::Unsupported => (
            None,
            tr(lang, "无法识别为节点或订阅", "Not a node or subscription"),
        ),
    };
    ScanSummary { name, detail }
}

impl Render for Finder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let dims = [size.width.into(), size.height.into()];
        let hit = nearest(&self.codes, self.position, dims);
        let target = hit.map(|(_, rect)| rect).unwrap_or([
            self.position[0] - SIZE / 2.,
            self.position[1] - SIZE / 2.,
            SIZE,
            SIZE,
        ]);
        let now = Instant::now();
        let animate = !cx.reduce_motion();
        let ease = if animate {
            1. - (-25. * (now - self.last_frame).as_secs_f32()).exp()
        } else {
            1.
        };
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
        let (color, stroke, arm) = if hit.is_some() {
            let alpha = if animate {
                0.75 + 0.25 * (age * std::f32::consts::TAU).cos()
            } else {
                1.
            };
            (
                self.palette.success.opacity(alpha),
                px(5.),
                (w.min(h) / 3.).min(34.),
            )
        } else {
            (self.palette.primary, px(2.), 24. + breath)
        };
        let name = hit.and_then(|(i, _)| self.summaries[i].name.clone());
        let status = if let Some((i, _)) = hit {
            self.summaries[i].detail.clone()
        } else if self.sampled_at.is_none() {
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
        let hint = if hit.is_some() {
            tr(
                self.lang,
                "点击导入 · 右键或 Esc 退出",
                "Click to import · Right-click or Esc to exit",
            )
        } else {
            tr(self.lang, "右键或 Esc 退出", "Right-click or Esc to exit")
        };
        if animate && (self.rect != target || entrance > 0.) {
            window.request_animation_frame();
        }
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    window.remove_window();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, window, cx| {
                    cx.stop_propagation();
                    window.remove_window();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|s, event: &MouseDownEvent, window, cx| {
                    if s.input.is_none() {
                        let pos = [event.position.x.into(), event.position.y.into()];
                        s.move_to(pos);
                        s.select(pos, window);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|s, event: &MouseMoveEvent, _, cx| {
                if s.input.is_none() {
                    cx.hide_cursor_until_mouse_moves();
                    s.move_to([event.position.x.into(), event.position.y.into()]);
                    cx.notify();
                }
            }))
            .on_scroll(cx.listener(|s, _, window, cx| {
                if s.input.is_none() {
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
            }))
            .child(
                div()
                    .absolute()
                    .left(px(l - offset))
                    .top(px(t - offset))
                    .w(px(w + 2. * offset))
                    .h(px(h + 2. * offset))
                    .children((0..4).map(|i| {
                        div()
                            .absolute()
                            .w(px(arm))
                            .h(px(arm))
                            .border_color(color)
                            .when(i < 2, |d| d.top_0().border_t(stroke))
                            .when(i >= 2, |d| d.bottom_0().border_b(stroke))
                            .when(i % 2 == 0, |d| d.left_0().border_l(stroke))
                            .when(i % 2 == 1, |d| d.right_0().border_r(stroke))
                    })),
            )
            .child(
                div()
                    .absolute()
                    .bottom(self.status_bottom)
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
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
                            .bg(self.palette.panel)
                            .text_color(self.palette.fg)
                            .px_4()
                            .py_2()
                            .text_sm()
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
            .when(self.input.is_none(), |view| {
                view.when_some(self.click_cursor, |view, (_, [x, y])| {
                    view.child(
                        svg()
                            .path("icons/mouse-pointer.svg")
                            .absolute()
                            .left(px(x))
                            .top(px(y))
                            .size_5()
                            .text_color(self.palette.fg),
                    )
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Language, SharePreview, scan_summary};

    #[test]
    fn node_summary_respects_endpoint_privacy_in_default_names() {
        let preview = |name: &str| SharePreview::Node {
            name: name.into(),
            protocol: "SOCKS",
            endpoint: "node.example:1080".into(),
            count: 1,
        };
        let label = scan_summary(preview("测试\n节点"), false, Language::Zh);
        assert_eq!(label.name.as_deref(), Some("测试 节点"));
        assert_eq!(label.detail.as_ref(), "SOCKS · node.example:1080");

        let label = scan_summary(preview("SOCKS node.example"), true, Language::En);
        assert_eq!(label.name.as_deref(), Some("SOCKS ••••••"));
        assert_eq!(label.detail.as_ref(), "SOCKS · ••••••:1080");
    }
}
