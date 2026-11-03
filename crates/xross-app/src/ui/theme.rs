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
    /// Mica supplies its own tint; macOS vibrancy and Acrylic blend with a
    /// theme-matched translucent tint so dark mode stays dark even over bright
    /// desktop wallpapers.
    pub fn sidebar(&self) -> Hsla {
        if !crate::window_effects::blur_backdrop() {
            return self.panel;
        }
        if crate::window_effects::uses_mica() {
            return gpui_kit::hsla(0., 0., 0., 0.);
        }
        if self.is_dark {
            gpui_kit::hsla(224., 0.20, 0.08, 0.62)
        } else {
            gpui_kit::hsla(216., 0.25, 0.97, 0.28)
        }
    }
}

pub fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    let is_dark = theme.mode.is_dark();
    if is_dark {
        Palette {
            bg: gpui_kit::hsla(0., 0., 0.08, 1.0),
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
    } else {
        Palette {
            bg: gpui_kit::hsla(216., 0.35, 0.972, 1.0),
            panel: gpui_kit::hsla(0., 0., 1.0, 1.0),
            panel_alt: gpui_kit::hsla(216., 0.28, 0.94, 1.0),
            fg: theme.foreground,
            muted_fg: theme.muted_foreground,
            border: gpui_kit::hsla(216., 0.22, 0.89, 1.0),
            accent: gpui_kit::hsla(214., 0.85, 0.945, 1.0),
            primary: theme.primary,
            primary_fg: theme.primary_foreground,
            danger: theme.danger,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            is_dark,
        }
    }
}
