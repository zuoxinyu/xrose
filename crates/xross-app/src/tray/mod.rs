//! System tray integration and "minimize to tray" plumbing.
//!
//! - macOS keeps the AppKit status item + native menu added in the previous
//!   iteration (`macos`).
//! - Windows mirrors that dashboard with `Shell_NotifyIcon` and a native popup
//!   menu (`windows`): status, live traffic, node list with latency, and the
//!   same actions.
//! - Other platforms fall back to a no-op backend: the tray stays unavailable
//!   and closing the window quits, exactly as before.
//!
//! [`TrayManager::tick`] drains actions queued by the platform menu callbacks
//! on the GPUI main thread, so the service is never called from an OS callback
//! context. Window-level commands ([`TrayCommand`]) bubble up to `main.rs`.

mod dashboard;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;

pub use dashboard::TrayCommand;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(any(target_os = "macos", windows)))]
use unsupported as backend;
#[cfg(windows)]
use windows as backend;

use std::sync::atomic::{AtomicBool, Ordering};

use xross_core::service::ToastLevel;
use xross_core::AppService;

/// The window is currently hidden in the tray. Kept here (not in a backend)
/// so `main.rs` avoids re-hiding/re-toasting on every tick.
static HIDDEN: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);

pub struct TrayManager {
    inner: Option<backend::Backend>,
}

impl TrayManager {
    pub fn new(service: &AppService) -> Self {
        Self {
            inner: backend::Backend::new(service),
        }
    }

    /// Polls the platform queue and refreshes tooltip/icon. Returns a command
    /// that needs the GPUI window or app context.
    pub fn tick(&mut self, service: &AppService) -> Option<TrayCommand> {
        self.inner.as_mut().and_then(|inner| inner.tick(service))
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
            "已最小化到托盘：单击托盘图标恢复，托盘菜单可退出",
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
