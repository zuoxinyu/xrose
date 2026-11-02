//! Fallback backend for platforms without a tray implementation: the tray is
//! unavailable, so closing the window keeps quitting normally.

use xross_core::AppService;

use super::TrayCommand;

pub struct Backend;

impl Backend {
    pub fn new(_service: &AppService) -> Option<Self> {
        None
    }

    pub fn tick(&mut self, _service: &AppService) -> Option<TrayCommand> {
        None
    }
}

pub fn available() -> bool {
    false
}

pub fn hide_main_window() {}

pub fn show_main_window() {}

pub fn main_window_minimized() -> bool {
    false
}

pub fn setup_window() {}
