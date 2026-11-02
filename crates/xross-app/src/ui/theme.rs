use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, Hsla};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Hsla,
    pub panel: Hsla,
    pub panel_alt: Hsla,
    pub fg: Hsla,
    pub muted_fg: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub primary: Hsla,
    pub primary_fg: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub is_dark: bool,
}

impl Palette {
    /// Surface of the left sidebar. macOS lets the window's vibrancy show
    /// through a clear sidebar, Windows paints a translucent tint over the
    /// acrylic backdrop, and every other platform stays opaque.
    pub fn sidebar(&self) -> Hsla {
        if !crate::window_effects::blur_backdrop() {
            return self.panel;
        }
        if cfg!(target_os = "macos") {
            return gpui_kit::hsla(0., 0., 0., 0.);
        }
        self.panel.opacity(if self.is_dark { 0.78 } else { 0.70 })
    }
}

pub fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    let is_dark = theme.mode.is_dark();
    let content_bg = if is_dark {
        gpui_kit::hsla(0., 0., 0.08, 1.0)
    } else {
        gpui_kit::hsla(0., 0., 1.0, 1.0)
    };
    Palette {
        bg: content_bg,
        panel: theme.secondary,
        panel_alt: theme.muted,
        fg: theme.foreground,
        muted_fg: theme.muted_foreground,
        border: theme.border,
        accent: theme.accent,
        primary: theme.primary,
        primary_fg: theme.primary_foreground,
        danger: theme.danger,
        success: theme.success,
        warning: theme.warning,
        info: theme.info,
        is_dark,
    }
}
