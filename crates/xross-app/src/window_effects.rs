//! Window backdrop shared by window creation and the chrome styling.
//!
//! The window is asked for a blurred background (`Blurred`) exactly when the
//! platform can render one, and the sidebar is translucent only in that case so
//! it never ends up showing the raw desktop through a tint.

use std::sync::OnceLock;

use gpui_kit::component::Theme;
use gpui_kit::App;
use gpui_kit::WindowBackgroundAppearance;

static BLUR_BACKDROP: OnceLock<bool> = OnceLock::new();

/// Whether the compositor blurs what is behind the window (vibrancy on macOS,
/// acrylic on Windows).
pub fn blur_backdrop() -> bool {
    *BLUR_BACKDROP.get_or_init(xross_core::system::window_effects::blur_backdrop_available)
}

/// Background appearance to request when opening the main window.
pub fn window_background() -> WindowBackgroundAppearance {
    if blur_backdrop() {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// The root surface of every window is filled with `theme.tokens.background`
/// by GPUI Kit's root plugin, which would hide the backdrop. Clear it so the
/// blur is only visible where the UI itself is translucent.
pub fn make_surface_translucent(cx: &mut App) {
    if !blur_backdrop() {
        return;
    }
    Theme::update(cx, |theme| {
        theme.tokens.background = gpui_kit::hsla(0., 0., 0., 0.).into();
    });
}
