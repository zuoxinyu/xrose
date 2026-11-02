//! Platform notification icons and one shared GPUI Kit tray flyout.

mod dashboard;
#[cfg(any(windows, test))]
mod motion;
mod popup;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

pub use dashboard::TrayCommand;

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use unsupported as backend;
#[cfg(windows)]
use windows as backend;

use gpui_kit::{App, Bounds, Pixels, Point, Window};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug)]
pub struct PopupAnchor {
    pub position: Point<Pixels>,
    pub work_area: Option<Bounds<Pixels>>,
}

static COMMANDS: Mutex<Vec<TrayCommand>> = Mutex::new(Vec::new());
static COMMAND_READY: tokio::sync::Notify = tokio::sync::Notify::const_new();
pub fn queue_command(command: TrayCommand) {
    COMMANDS.lock().push(command);
    COMMAND_READY.notify_one();
}

/// Wake the GPUI loop as soon as a native callback queues a command.
pub async fn wait_for_command() {
    COMMAND_READY.notified().await;
}
pub fn register_main_window(window: &Window) {
    backend::register_main_window(window);
}

use xross_core::service::ToastLevel;
use xross_core::AppService;

/// The window is currently hidden in the tray. Kept here (not in a backend)
/// so `main.rs` avoids re-hiding/re-toasting on every tick.
static HIDDEN: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);

pub struct TrayManager {
    inner: Option<backend::Backend>,
    popup: popup::PopupManager,
}

impl TrayManager {
    pub fn new(service: &AppService) -> Self {
        Self {
            inner: backend::Backend::new(service),
            popup: popup::PopupManager::default(),
        }
    }

    /// Polls the platform queue and refreshes tooltip/icon. Returns a command
    /// that needs the GPUI window or app context.
    pub fn tick(&mut self, service: &AppService, cx: &mut App) -> Option<TrayCommand> {
        if let Some(command) = self.inner.as_mut().and_then(|inner| inner.tick(service)) {
            queue_command(command);
        }
        let commands = std::mem::take(&mut *COMMANDS.lock());
        let mut result = None;
        for command in commands {
            if command == TrayCommand::TogglePopup {
                self.popup.toggle(service, backend::popup_anchor(), cx);
            } else {
                self.popup.dismiss(cx);
                result = Some(command);
            }
        }
        self.popup.tick(service, cx);
        result
    }
}

/// Whether the platform tray backend initialized successfully.
pub fn available() -> bool {
    backend::available()
}

/// Hides the main window into the tray. Returns true when this call actually
/// hid it (used to toast only once).
pub fn hide_main_window() -> bool {
    if HIDDEN.swap(true, Ordering::SeqCst) {
        return false;
    }
    backend::hide_main_window();
    true
}

pub fn show_main_window() {
    HIDDEN.store(false, Ordering::SeqCst);
    backend::show_main_window();
}

pub fn window_hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

pub fn main_window_minimized() -> bool {
    backend::main_window_minimized()
}

/// Called from the 200 ms loop: turns a native minimize into "hide in tray"
/// when the user asked for it.
pub fn watch_minimize(service: &AppService) {
    if !available() || window_hidden() {
        return;
    }
    if !service.settings().minimize_to_tray {
        return;
    }
    if main_window_minimized() && hide_main_window() {
        service.toast(
            ToastLevel::Info,
            "已最小化到托盘：单击图标打开浮窗，可显示主窗口或退出",
        );
    }
}

/// Marks the quit path so `on_window_should_close` does not hide instead of
/// exiting (tray "退出" or the Cmd/Ctrl+Q shortcut).
pub fn request_quit() {
    QUITTING.store(true, Ordering::SeqCst);
}

pub fn quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

/// macOS-only AppKit window decoration; a no-op elsewhere.
pub fn setup_platform_window() {
    backend::setup_window();
}
