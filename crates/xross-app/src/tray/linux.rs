//! StatusNotifierItem integration without a GTK event loop.
//!
//! D-Bus work lives on a worker thread. Hosts without Activate support get a
//! minimal native launcher; the dashboard always renders in GPUI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};

use gpui_kit::{point, px, Window, WindowAppearance};
use ksni::blocking::TrayMethods;
use parking_lot::Mutex;
use xross_core::AppService;

use super::dashboard::{Dashboard, TrayCommand};

static READY: AtomicBool = AtomicBool::new(false);
static WATCHER_OFFLINE: AtomicBool = AtomicBool::new(false);
static ANCHOR: Mutex<Option<(i32, i32)>> = Mutex::new(None);

struct Item {
    dashboard: Dashboard,
    alpha: Vec<u8>,
    ink: u8,
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "io.xross.Xross".into()
    }
    fn title(&self) -> String {
        self.dashboard.status_line.clone()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![ksni::Icon {
            width: 32,
            height: 32,
            // StatusNotifierItem uses network-order ARGB, with straight alpha.
            data: self
                .alpha
                .iter()
                .flat_map(|&a| [a, self.ink, self.ink, self.ink])
                .collect(),
        }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.dashboard.status_line.clone(),
            description: self.dashboard.traffic_line.clone().unwrap_or_default(),
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
        let lang = self.dashboard.language;
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
                label: crate::i18n::tr(lang, "退出 Xross", "Quit Xross").to_string(),
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
        READY.store(false, Ordering::Relaxed);
        true
    }
}

pub struct Backend {
    updates: SyncSender<(Dashboard, u8)>,
}

impl Backend {
    pub fn new(_service: &AppService) -> Option<Self> {
        let (updates, receiver) = sync_channel(1);
        std::thread::Builder::new()
            .name("xross-status-notifier".into())
            .spawn(move || {
                let Ok(image) = image::load_from_memory(crate::brand_assets::TRAY_TEMPLATE) else {
                    return;
                };
                let image = image
                    .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
                    .to_rgba8();
                let (dashboard, ink) = match receiver.recv() {
                    Ok(update) => update,
                    Err(_) => return,
                };
                let item = Item {
                    dashboard,
                    alpha: image.pixels().map(|p| p[3]).collect(),
                    ink,
                };
                let handle = match item.assume_sni_available(true).spawn() {
                    Ok(handle) => handle,
                    Err(err) => {
                        eprintln!("Linux 托盘不可用：{err}");
                        return;
                    }
                };
                READY.store(!WATCHER_OFFLINE.load(Ordering::Relaxed), Ordering::Relaxed);
                while let Ok((dashboard, ink)) = receiver.recv() {
                    if handle
                        .update(|item| {
                            item.dashboard = dashboard;
                            item.ink = ink;
                        })
                        .is_none()
                    {
                        break;
                    }
                }
                READY.store(false, Ordering::Relaxed);
                let _ = handle.shutdown();
            })
            .ok()?;
        Some(Self { updates })
    }
    pub fn tick(&mut self, service: &AppService, cx: &gpui_kit::App) -> Option<TrayCommand> {
        let dark = matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let ink = if dark { 255 } else { 0 };
        let _ = self
            .updates
            .try_send((super::dashboard::build(service), ink));
        None
    }
}

pub fn available() -> bool {
    READY.load(Ordering::Relaxed)
}
pub fn register_main_window(_window: &Window) {}
// GPUI exposes minimize/activate on Linux, but no portable hide/unhide for a
// Wayland toplevel. The main loop uses those operations for this backend.
pub fn hide_main_window() {}
pub fn show_main_window() {}
pub fn main_window_minimized() -> bool {
    false
}
pub fn setup_window() {}
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
pub fn show_popup(window: &mut gpui_kit::Window, _bounds: gpui_kit::Bounds<gpui_kit::Pixels>) {
    window.activate_window();
}
