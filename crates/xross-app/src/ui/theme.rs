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
}

pub fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    Palette {
        bg: theme.background,
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
    }
}
