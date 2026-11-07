//! Fallback backend for platforms without a tray implementation: the tray is
//! unavailable, so closing the window keeps quitting normally.

use xrose_core::AppService;

use super::TrayCommand;

pub struct Backend;

impl Backend {
    pub fn new(_service: &AppService) -> Option<Self> {
        None
    }

    pub fn tick(&mut self, _service: &AppService, _cx: &gpui_kit::App) -> Option<TrayCommand> {
        None
    }
}

pub fn available() -> bool {
    false
}

pub fn hide_main_window() {}

pub fn show_main_window() {}

pub fn setup_window() {}

pub fn register_main_window(_window: &gpui_kit::Window) {}
pub fn popup_anchor() -> Option<super::PopupAnchor> {
    None
}
pub fn pointer_on_icon() -> bool {
    false
}

pub fn hide_popup(window: &mut gpui_kit::Window) {
    window.remove_window();
}
pub fn show_popup(window: &mut gpui_kit::Window, _bounds: gpui_kit::Bounds<gpui_kit::Pixels>) {
    window.activate_window();
}
