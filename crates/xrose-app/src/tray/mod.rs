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

use xrose_core::AppService;

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
        if let Some(command) = self
            .inner
            .as_mut()
            .and_then(|inner| inner.tick(service, cx))
        {
            queue_command(command);
        }
        let commands = std::mem::take(&mut *COMMANDS.lock());
        if commands.contains(&TrayCommand::Quit) {
            self.popup.dismiss(cx);
            return Some(TrayCommand::Quit);
        }
        let mut result = None;
        for command in commands {
            #[cfg(target_os = "linux")]
            if command.shows_main_window() {
                result = Some(command);
                continue;
            } else if command == TrayCommand::TrayUnavailable {
                result.get_or_insert(command);
                continue;
            }
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

    #[cfg(target_os = "linux")]
    pub fn dismiss_popup(&mut self, cx: &mut App) {
        self.popup.dismiss(cx);
    }
}

/// Whether the platform tray backend initialized successfully.
pub fn available() -> bool {
    backend::available()
}

pub fn hide_main_window() {
    backend::hide_main_window();
}

pub fn show_main_window() {
    backend::show_main_window();
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
