use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, Hsla, rgb};
use xrose_core::storage::LatencyEntry;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Hsla,
    pub panel: Hsla,
    pub panel_alt: Hsla,
    pub fg: Hsla,
    pub muted_fg: Hsla,
    pub border: Hsla,
    /// Soft tint of the interaction color (selection, active nav item).
    pub accent: Hsla,
    /// The single interaction color: primary buttons, toggles, radios, bars.
    pub primary: Hsla,
    pub primary_fg: Hsla,
    pub danger: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    /// Data series colors, independent of the interaction color.
    pub data_down: Hsla,
    pub data_up: Hsla,
    /// Text-grade status colors (WCAG AA against the surfaces they sit on).
    pub ok_text: Hsla,
    pub warn_text: Hsla,
    pub danger_text: Hsla,
    pub is_dark: bool,
}

/// HSL from degrees (GPUI's `hsla` expects the hue as a 0..1 fraction).
pub fn hsl(h: f32, s: f32, l: f32) -> Hsla {
    gpui_kit::hsla(h / 360., s, l, 1.0)
}

/// Sample the three sRGB stops of the Xrose mark for the viewfinder.
pub fn finder_brand_color(phase: f32, opacity: f32) -> Hsla {
    let position = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
    let (from, to, t) = if position < 0.48 {
        (0x00bdf5, 0x187af7, position / 0.48)
    } else {
        (0x187af7, 0x963bf2, (position - 0.48) / 0.52)
    };
    Hsla::from(rgb(from).blend(rgb(to).alpha(t))).opacity(opacity)
}

/// Interaction color shared by the theme override and the palette.
pub fn interaction(dark: bool) -> (Hsla, Hsla) {
    if dark {
        (hsl(188., 0.86, 0.53), hsl(192., 0.80, 0.10))
    } else {
        (hsl(192., 0.81, 0.31), hsl(0., 0.0, 1.0))
    }
}

/// Soft tint of the interaction color.
pub fn interaction_tint(dark: bool) -> Hsla {
    if dark {
        hsl(192., 0.50, 0.17)
    } else {
        hsl(190., 0.75, 0.93)
    }
}

// Wayland blur filters the pixels behind the surface without adding a system tint.
#[cfg(target_os = "linux")]
const WAYLAND_SURFACE_ALPHA: f32 = 0.87;

impl Palette {
    /// Solid backdrop color used when window material is set to Solid or blur is unavailable.
    pub(crate) fn solid_backdrop(&self) -> Hsla {
        if self.is_dark {
            self.panel
        } else {
            rgb(0xc5e1ec).into()
        }
    }

    /// Mica and macOS vibrancy supply their own tint through a clear sidebar.
    /// Acrylic and vibrancy over a bright wallpaper get a theme-matched tint so
    /// dark mode stays dark.
    pub fn sidebar(&self) -> Hsla {
        if !crate::window_effects::blur_backdrop() {
            return self.solid_backdrop();
        }
        if crate::window_effects::uses_mica() {
            return gpui_kit::hsla(0., 0., 0., 0.);
        }
        #[cfg(target_os = "linux")]
        return self.panel.opacity(WAYLAND_SURFACE_ALPHA);
        #[cfg(not(target_os = "linux"))]
        if self.is_dark {
            hsl(220., 0.18, 0.08).opacity(0.62)
        } else {
            hsl(210., 0.30, 0.98).opacity(0.28)
        }
    }

    /// Neutral gray hover background for icon buttons in both light and dark themes.
    pub fn neutral_hover(&self) -> Hsla {
        if self.is_dark {
            hsl(0., 0., 0.32).opacity(0.65)
        } else {
            hsl(210., 0.16, 0.91).opacity(0.85)
        }
    }

    /// Slightly stronger neutral gray fill when pressing an inactive icon button or holding its menu open.
    pub fn neutral_active(&self) -> Hsla {
        if self.is_dark {
            hsl(0., 0., 0.36).opacity(0.78)
        } else {
            hsl(210., 0.16, 0.87).opacity(0.92)
        }
    }
}

pub fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    let is_dark = theme.mode.is_dark();
    let (primary, primary_fg) = interaction(is_dark);
    if is_dark {
        Palette {
            bg: hsl(220., 0.10, 0.09),
            panel: theme.secondary,
            panel_alt: theme.muted,
            fg: theme.foreground,
            muted_fg: theme.muted_foreground,
            border: theme.border,
            accent: interaction_tint(true),
            primary,
            primary_fg,
            danger: theme.danger,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            data_down: hsl(213., 0.94, 0.68),
            data_up: hsl(255., 0.92, 0.76),
            ok_text: hsl(142., 0.69, 0.58),
            warn_text: hsl(43., 0.96, 0.56),
            danger_text: hsl(0., 0.91, 0.71),
            is_dark,
        }
    } else {
        Palette {
            bg: hsl(210., 0.40, 0.975),
            panel: hsl(0., 0., 1.0),
            panel_alt: hsl(210., 0.30, 0.94),
            fg: theme.foreground,
            muted_fg: theme.muted_foreground,
            border: hsl(210., 0.25, 0.88),
            accent: interaction_tint(false),
            primary,
            primary_fg,
            danger: theme.danger,
            success: theme.success,
            warning: theme.warning,
            info: theme.info,
            data_down: hsl(221., 0.83, 0.53),
            data_up: hsl(262., 0.83, 0.58),
            ok_text: hsl(142., 0.72, 0.29),
            warn_text: hsl(26., 0.90, 0.37),
            danger_text: hsl(0., 0.74, 0.42),
            is_dark,
        }
    }
}

/// Latency grades in milliseconds. Colour is reserved for exceptions: only the
/// best and the degraded tiers are tinted, the normal tier keeps the text colour.
pub const LATENCY_EXCELLENT_MS: u32 = 120;
pub const LATENCY_NORMAL_MS: u32 = 500;
pub const LATENCY_SLOW_MS: u32 = 1000;

/// A measurement older than this is shown dimmed.
pub const LATENCY_STALE_SECS: i64 = 10 * 60;

pub fn latency_color_ms(p: &Palette, ms: Option<u32>) -> Hsla {
    match ms {
        Some(v) if v < LATENCY_EXCELLENT_MS => p.ok_text,
        Some(v) if v < LATENCY_NORMAL_MS => p.fg,
        Some(v) if v < LATENCY_SLOW_MS => p.warn_text,
        Some(_) => p.danger_text,
        None => p.muted_fg,
    }
}

/// Colour for a stored measurement; a failed probe is an error, not "untested".
pub fn latency_color(p: &Palette, entry: Option<&LatencyEntry>) -> Hsla {
    match entry {
        None => p.muted_fg,
        Some(entry) if entry.delay_ms.is_none() => p.danger_text,
        Some(entry) => latency_color_ms(p, entry.delay_ms),
    }
}

pub fn latency_age_secs(entry: &LatencyEntry) -> i64 {
    xrose_core::util::now_unix()
        .saturating_sub(entry.checked_at)
        .max(0)
}

pub fn latency_label(
    entry: Option<&LatencyEntry>,
    testing: bool,
    lang: xrose_core::settings::Language,
) -> String {
    if testing {
        return crate::i18n::tr(lang, "测速中", "Testing").to_string();
    }
    match entry {
        Some(entry) => entry.delay_ms.map_or_else(
            || {
                if entry.is_http() {
                    crate::i18n::tr(lang, "失败", "Failed")
                } else {
                    crate::i18n::tr(lang, "超时", "Timeout")
                }
                .to_string()
            },
            |ms| format!("{ms} ms"),
        ),
        None => "—".into(),
    }
}

/// Shared geometry and opacity tokens for the desktop surfaces.
pub const PAGE_PAD: f32 = 24.;
pub const CONTROL_HEIGHT: f32 = 32.;
pub const SIDEBAR_WIDTH: f32 = 220.;
pub const HEADER_HEIGHT: f32 = 54.;
pub const HEADER_DRAG_GAP: f32 = 24.;
pub const STATUS_HEIGHT: f32 = 28.;
pub const STATUS_END_RADIUS: f32 = STATUS_HEIGHT / 2. - 1.;
pub const STATUS_NODE_WIDTH: f32 = 220.;
pub const HEADER_TRAFFIC_POPOVER_WIDTH: f32 = 308.;
pub const HEADER_PRIVACY_EDGE_GAP: f32 = if cfg!(target_os = "macos") { 12. } else { 8. };
pub const ICON_BUTTON_SIZE: f32 = 28.;
pub const ICON_SIZE: f32 = 16.;
pub const NODE_ROW_HEIGHT: f32 = 64.;
pub const NODE_HTTP_WIDTH: f32 = 80.;
pub const SETTING_LABEL_WIDTH: f32 = 120.;
pub const DNS_NAME_COL_WIDTH: f32 = 168.;
pub const DNS_ACTION_WIDTH: f32 = 84.;
pub const FORM_FIELD_WIDTH: f32 = 240.;
pub const EMPTY_HEIGHT: f32 = 160.;
pub const DISABLED_OPACITY: f32 = 0.45;
pub const SEMANTIC_ALPHA: f32 = 0.14;
pub const TRAFFIC_HEIGHT: f32 = 48.;
pub const STATS_BAR_HEIGHT: f32 = 8.;
pub const STATS_DATE_WIDTH: f32 = 144.;
pub const STATS_VALUE_WIDTH: f32 = 96.;
pub const STATS_SHARE_WIDTH: f32 = 64.;
pub const TRAFFIC_WINDOW_MS: i64 = 60_000;
pub const TRAFFIC_STROKE: f32 = 2.;
pub const TRAFFIC_DASH: f32 = 4.;
pub const TRACE_CANVAS_PAD: f32 = 12.;
pub const PROGRESS_HEIGHT: f32 = 4.;
pub const DRAG_EDGE: f32 = 32.;
pub const DRAG_SCROLL_STEP: f32 = 12.;
pub const INSERTION_LINE: f32 = 2.;
pub const DIALOG_WIDTH: f32 = 440.;
pub const DIALOG_WIDE: f32 = 720.;
pub const MEMBER_LIST_HEIGHT: f32 = 192.;

pub fn switch_off() -> Hsla {
    hsl(0., 0., 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_palette() -> Palette {
        let dummy = gpui_kit::hsla(0., 0., 0., 1.);
        Palette {
            bg: dummy,
            panel: dummy,
            panel_alt: dummy,
            fg: gpui_kit::hsla(0.1, 0., 0., 1.),
            muted_fg: gpui_kit::hsla(0.2, 0., 0., 1.),
            border: dummy,
            accent: dummy,
            primary: dummy,
            primary_fg: dummy,
            danger: dummy,
            success: dummy,
            warning: dummy,
            info: dummy,
            data_down: dummy,
            data_up: dummy,
            ok_text: gpui_kit::hsla(0.3, 0., 0., 1.),
            warn_text: gpui_kit::hsla(0.4, 0., 0., 1.),
            danger_text: gpui_kit::hsla(0.5, 0., 0., 1.),
            is_dark: false,
        }
    }

    #[test]
    fn test_latency_color_ms_grades() {
        let p = dummy_palette();

        // < 120 ms: 优 (ok_text)
        assert_eq!(latency_color_ms(&p, Some(0)), p.ok_text);
        assert_eq!(latency_color_ms(&p, Some(119)), p.ok_text);

        // 120 ~ 499 ms: 正常 (fg)
        assert_eq!(latency_color_ms(&p, Some(120)), p.fg);
        assert_eq!(latency_color_ms(&p, Some(350)), p.fg);
        assert_eq!(latency_color_ms(&p, Some(499)), p.fg);

        // 500 ~ 999 ms: 偏慢 (warn_text)
        assert_eq!(latency_color_ms(&p, Some(500)), p.warn_text);
        assert_eq!(latency_color_ms(&p, Some(750)), p.warn_text);
        assert_eq!(latency_color_ms(&p, Some(999)), p.warn_text);

        // >= 1000 ms: 较差 (danger_text)
        assert_eq!(latency_color_ms(&p, Some(1000)), p.danger_text);
        assert_eq!(latency_color_ms(&p, Some(1500)), p.danger_text);

        // None: 未测速 (muted_fg)
        assert_eq!(latency_color_ms(&p, None), p.muted_fg);
    }

    #[test]
    fn test_neutral_hover_is_distinct_from_dark_panel_and_accent() {
        let mut p = dummy_palette();
        p.panel = hsl(0., 0., 0.15);
        p.accent = interaction_tint(false);
        assert_ne!(p.neutral_hover(), p.accent);
        assert_ne!(p.neutral_hover(), p.neutral_active());

        p.is_dark = true;
        p.accent = interaction_tint(true);
        assert_ne!(p.neutral_hover(), p.panel);
        assert_ne!(p.neutral_hover(), p.accent);
        assert_ne!(p.neutral_hover(), p.neutral_active());
    }

    #[test]
    fn test_solid_backdrop_uses_ice_blue_in_light_mode_and_panel_in_dark_mode() {
        let mut p = dummy_palette();
        p.panel = hsl(0., 0., 1.0);
        p.is_dark = false;
        assert_eq!(p.solid_backdrop(), Hsla::from(rgb(0xc5e1ec)));

        p.panel = hsl(0., 0., 0.15);
        p.is_dark = true;
        assert_eq!(p.solid_backdrop(), p.panel);
    }
}
