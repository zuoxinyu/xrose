//! StatusNotifierItem integration without a GTK event loop.
//!
//! D-Bus work lives on a worker thread. Hosts without Activate support get a
//! minimal native launcher; the dashboard always renders in GPUI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};

use gpui_kit::{Bounds, Pixels, Point, Window, WindowAppearance, WindowKind, point, px};
use ksni::blocking::TrayMethods;
use parking_lot::Mutex;
use xrose_core::AppService;

use super::dashboard::{TrayCommand, status_lines};
use crate::brand_assets::TrayGlyph;
use xrose_core::settings::Language;

static READY: AtomicBool = AtomicBool::new(false);
static WATCHER_OFFLINE: AtomicBool = AtomicBool::new(false);
static ANCHOR: Mutex<Option<(i32, i32)>> = Mutex::new(None);

struct Item {
    language: Language,
    status_line: String,
    traffic_line: Option<String>,
    pixmap: Vec<u8>,
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }
    fn title(&self) -> String {
        self.status_line.clone()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon {
            width: 32,
            height: 32,
            // StatusNotifierItem uses network-order ARGB, with straight alpha.
            data: self.pixmap.clone(),
        }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.status_line.clone(),
            description: self.traffic_line.clone().unwrap_or_default(),
            ..Default::default()
        }
    }
    fn activate(&mut self, x: i32, y: i32) {
        *ANCHOR.lock() = Some((x, y));
        super::queue_command(TrayCommand::TogglePopup);
    }
    fn secondary_activate(&mut self, x: i32, y: i32) {
        self.activate(x, y);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let lang = self.language;
        vec![
            StandardItem {
                label: crate::i18n::tr(lang, "打开托盘浮窗", "Open tray dashboard").to_string(),
                activate: Box::new(|_| super::queue_command(TrayCommand::TogglePopup)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: crate::i18n::tr(lang, "显示主窗口", "Show window").to_string(),
                activate: Box::new(|_| super::queue_command(TrayCommand::ShowWindow)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: crate::i18n::tr(lang, "退出 Xrose", "Quit Xrose").to_string(),
                activate: Box::new(|_| super::queue_command(TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn watcher_online(&self) {
        WATCHER_OFFLINE.store(false, Ordering::Relaxed);
        READY.store(true, Ordering::Relaxed);
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        WATCHER_OFFLINE.store(true, Ordering::Relaxed);
        mark_unavailable();
        true
    }
}

fn tray_pixmap(glyph: TrayGlyph) -> Vec<u8> {
    glyph
        .raster(32)
        .pixels()
        .flat_map(|p| {
            let a = u16::from(p[3]);
            let straight = |v: u8| {
                if a == 0 {
                    0
                } else {
                    ((u16::from(v) * 255 + a / 2) / a).min(255) as u8
                }
            };
            [p[3], straight(p[0]), straight(p[1]), straight(p[2])]
        })
        .collect()
}

pub struct Backend {
    updates: SyncSender<bool>,
}

impl Backend {
    pub fn new(service: &AppService) -> Option<Self> {
        let service = service.clone();
        let (updates, receiver) = sync_channel(1);
        std::thread::Builder::new()
            .name("xrose-status-notifier".into())
            .spawn(move || {
                let Ok(white_ink) = receiver.recv() else {
                    return;
                };
                let status = service.tray_status();
                let glyph = TrayGlyph::new(&status, white_ink);
                let (status_line, traffic_line) = status_lines(&status);
                let mut previous = (status, glyph);
                let item = Item {
                    language: status.language,
                    status_line,
                    traffic_line,
                    pixmap: tray_pixmap(glyph),
                };
                let handle = match item.assume_sni_available(true).spawn() {
                    Ok(handle) => handle,
                    Err(err) => {
                        eprintln!("Linux 托盘不可用：{err}");
                        return;
                    }
                };
                READY.store(!WATCHER_OFFLINE.load(Ordering::Relaxed), Ordering::Relaxed);
                while let Ok(white_ink) = receiver.recv() {
                    let status = service.tray_status();
                    let glyph = TrayGlyph::new(&status, white_ink);
                    if (status, glyph) == previous {
                        continue;
                    }
                    let image_changed = previous.1 != glyph;
                    previous = (status, glyph);
                    let (status_line, traffic_line) = status_lines(&status);
                    if handle
                        .update(|item| {
                            item.language = status.language;
                            item.status_line = status_line;
                            item.traffic_line = traffic_line;
                            if image_changed {
                                item.pixmap = tray_pixmap(glyph);
                            }
                        })
                        .is_none()
                    {
                        break;
                    }
                }
                mark_unavailable();
                let _ = handle.shutdown();
            })
            .ok()?;
        Some(Self { updates })
    }
    pub fn tick(&mut self, _service: &AppService, cx: &gpui_kit::App) -> Option<TrayCommand> {
        let white_ink = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let _ = self.updates.try_send(white_ink);
        None
    }
}

fn mark_unavailable() {
    if READY.swap(false, Ordering::Relaxed) {
        super::queue_command(TrayCommand::TrayUnavailable);
    }
}

pub fn available() -> bool {
    READY.load(Ordering::Relaxed)
}
pub fn register_main_window(_window: &Window) {}
// The application owns withdrawal and restoration of the KDE main window.
pub fn hide_main_window() {}
pub fn show_main_window() {}
pub fn setup_window() {}
pub fn wake_existing(paths: &xrose_core::paths::AppPaths) -> bool {
    xrose_core::system::SingleInstance::notify_existing(&paths.lock_file())
}
pub fn pointer_on_icon() -> bool {
    false
}
pub fn popup_anchor() -> Option<super::PopupAnchor> {
    ANCHOR
        .lock()
        .take()
        .filter(|&(x, y)| x != 0 || y != 0)
        .map(|(x, y)| super::PopupAnchor {
            position: point(px(x as f32), px(y as f32)),
            work_area: None,
        })
}

pub fn hide_popup(window: &mut gpui_kit::Window) {
    window.remove_window();
}
pub fn show_popup(
    window: &mut gpui_kit::Window,
    _bounds: gpui_kit::Bounds<gpui_kit::Pixels>,
    shown: tokio::sync::oneshot::Sender<()>,
) {
    window.activate_window();
    let _ = shown.send(());
}

/// The compositor places a zero-exclusive-zone layer inside the usable area,
/// so its near edge clears the panel even when it floats or changes thickness.
pub fn popup_layer(
    at: Option<Point<Pixels>>,
    bounds: Bounds<Pixels>,
    screen: Bounds<Pixels>,
) -> WindowKind {
    use gpui_kit::layer_shell::{Anchor, LayerShellOptions};
    let gap = px(8.);
    let zero = px(0.);
    let at = at.unwrap_or(screen.bottom_right());
    let edges = [
        (Anchor::BOTTOM, screen.bottom() - at.y),
        (Anchor::TOP, at.y - screen.top()),
        (Anchor::RIGHT, screen.right() - at.x),
        (Anchor::LEFT, at.x - screen.left()),
    ];
    let (edge, _) = edges
        .into_iter()
        .min_by_key(|(_, distance)| *distance)
        .unwrap();
    let left = bounds.left() - screen.left();
    let top = (at.y - bounds.size.height / 2. - screen.top()).clamp(
        gap,
        (screen.size.height - bounds.size.height - gap).max(gap),
    );
    let (anchor, margin) = match edge {
        Anchor::TOP => (Anchor::TOP | Anchor::LEFT, (gap, zero, zero, left)),
        Anchor::BOTTOM => (Anchor::BOTTOM | Anchor::LEFT, (zero, zero, gap, left)),
        Anchor::LEFT => (Anchor::TOP | Anchor::LEFT, (top, zero, zero, gap)),
        _ => (Anchor::TOP | Anchor::RIGHT, (top, gap, zero, zero)),
    };
    WindowKind::LayerShell(LayerShellOptions {
        namespace: "io.xrose.Xrose.Tray".into(),
        anchor,
        margin: Some(margin),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{layer_shell::Anchor, size};

    #[test]
    fn anchors_to_the_clicked_panel_in_output_local_coordinates() {
        let screen = Bounds::new(point(px(-1200.), px(-100.)), size(px(1200.), px(800.)));
        let popup = Bounds::new(point(px(-800.), px(80.)), size(px(400.), px(528.)));
        for (position, edges, margins) in [
            (
                (-600., 696.),
                Anchor::BOTTOM | Anchor::LEFT,
                (0., 0., 8., 400.),
            ),
            (
                (-600., -96.),
                Anchor::TOP | Anchor::LEFT,
                (8., 0., 0., 400.),
            ),
            (
                (-1198., 350.),
                Anchor::TOP | Anchor::LEFT,
                (186., 0., 0., 8.),
            ),
            ((-2., 350.), Anchor::TOP | Anchor::RIGHT, (186., 8., 0., 0.)),
        ] {
            let at = point(px(position.0), px(position.1));
            let WindowKind::LayerShell(options) = popup_layer(Some(at), popup, screen) else {
                panic!("expected a layer surface");
            };
            assert_eq!(options.anchor, edges);
            assert_eq!(
                options.margin,
                Some((px(margins.0), px(margins.1), px(margins.2), px(margins.3)))
            );
        }
    }
}
