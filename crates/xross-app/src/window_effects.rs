//! Native window materials shared by the main window and tray flyout.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[cfg(windows)]
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Theme;
use gpui_kit::{App, Window, WindowBackgroundAppearance};
use parking_lot::RwLock;
use xross_core::settings::BackdropStyle;

static STYLE: RwLock<BackdropStyle> = RwLock::new(BackdropStyle::Auto);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static BLUR_AVAILABLE: OnceLock<bool> = OnceLock::new();
static MICA_AVAILABLE: OnceLock<bool> = OnceLock::new();

pub fn configure(style: BackdropStyle) {
    *STYLE.write() = style;
    ACTIVE.store(
        effective_style(style) != BackdropStyle::Opaque,
        Ordering::Relaxed,
    );
}

pub fn blur_backdrop() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn effective_style(style: BackdropStyle) -> BackdropStyle {
    let blur =
        *BLUR_AVAILABLE.get_or_init(xross_core::system::window_effects::blur_backdrop_available);
    let mica =
        *MICA_AVAILABLE.get_or_init(xross_core::system::window_effects::mica_backdrop_available);
    resolve_style(style, cfg!(windows), cfg!(target_os = "macos"), blur, mica)
}

fn resolve_style(
    style: BackdropStyle,
    windows: bool,
    macos: bool,
    blur: bool,
    mica: bool,
) -> BackdropStyle {
    use BackdropStyle::*;
    match style {
        Auto if windows && mica => MicaAlt,
        Auto if windows && blur => Acrylic,
        Auto if macos && blur => Vibrancy,
        Mica | MicaAlt if windows && mica => style,
        Acrylic if windows && blur => Acrylic,
        Vibrancy if macos && blur => Vibrancy,
        _ => Opaque,
    }
}

/// Mica already supplies its own tint; painting a tint over it hides the material.
pub fn uses_mica() -> bool {
    blur_backdrop()
        && matches!(
            effective_style(*STYLE.read()),
            BackdropStyle::Mica | BackdropStyle::MicaAlt
        )
}

pub fn window_background() -> WindowBackgroundAppearance {
    match effective_style(*STYLE.read()) {
        BackdropStyle::Mica => WindowBackgroundAppearance::MicaBackdrop,
        BackdropStyle::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
        BackdropStyle::Acrylic | BackdropStyle::Vibrancy => WindowBackgroundAppearance::Blurred,
        _ => WindowBackgroundAppearance::Opaque,
    }
}

/// Root surfaces stay clear; each view supplies its own opaque fallback fill.
/// Also configures high-contrast input borders and readable muted foreground
/// across both dark and light modes.
pub fn make_surface_translucent(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.tokens.background = gpui_kit::hsla(0., 0., 0., 0.).into();
        if theme.mode.is_dark() {
            let input_border = gpui_kit::hsla(0., 0., 0.38, 1.0);
            theme.colors.input = input_border;
            theme.tokens.input = input_border.into();
            theme.colors.muted_foreground = gpui_kit::hsla(0., 0., 0.65, 1.0);
            theme.tokens.muted_foreground = theme.colors.muted_foreground.into();
        } else {
            let input_border = gpui_kit::hsla(0., 0., 0.58, 1.0);
            theme.colors.input = input_border;
            theme.tokens.input = input_border.into();
            theme.colors.muted_foreground = gpui_kit::hsla(0., 0., 0.42, 1.0);
            theme.tokens.muted_foreground = theme.colors.muted_foreground.into();
        }
    });
}

pub fn apply(window: &mut Window, cx: &mut App) {
    let appearance = window_background();
    #[cfg(windows)]
    let active = windows::apply(window, appearance, cx.theme().mode.is_dark());
    #[cfg(not(windows))]
    let active = {
        window.set_background_appearance(appearance);
        appearance != WindowBackgroundAppearance::Opaque
    };
    ACTIVE.store(active, Ordering::Relaxed);
    make_surface_translucent(cx);
    window.refresh();
}

/// Reapply native state as well as repainting after theme/material changes.
pub fn refresh_all(style: BackdropStyle, cx: &mut App) {
    configure(style);
    // A settings click is still borrowing its window; update all native windows
    // after that event completes so the current window is included as well.
    cx.defer(|cx| {
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, cx| apply(window, cx));
        }
        cx.refresh_windows();
    });
}

#[cfg(windows)]
mod windows {
    use super::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmExtendFrameIntoClientArea, DwmGetWindowAttribute, DwmSetWindowAttribute,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::UI::Controls::MARGINS;

    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        color: u32,
        animation: u32,
    }
    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut std::ffi::c_void,
        size: usize,
    }

    /// GPUI's Mica setter leaves the old Acrylic accent policy intact. Disable
    /// WCA_ACCENT_POLICY explicitly; setting only DWMSBT_NONE does not clear it.
    unsafe fn clear_accent(hwnd: HWND) -> bool {
        unsafe {
            let name: Vec<u16> = "user32.dll\0".encode_utf16().collect();
            let module = GetModuleHandleW(name.as_ptr());
            let Some(proc) = GetProcAddress(module, b"SetWindowCompositionAttribute\0".as_ptr())
            else {
                return false;
            };
            let set: unsafe extern "system" fn(HWND, *mut CompositionData) -> i32 =
                std::mem::transmute(proc);
            let mut policy = AccentPolicy {
                state: 0,
                flags: 0,
                color: 0,
                animation: 0,
            };
            let mut data = CompositionData {
                attribute: 19,
                data: &mut policy as *mut _ as _,
                size: std::mem::size_of::<AccentPolicy>(),
            };
            set(hwnd, &mut data) != 0
        }
    }

    pub fn apply(window: &mut Window, appearance: WindowBackgroundAppearance, dark: bool) -> bool {
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };
        let RawWindowHandle::Win32(raw) = handle.as_raw() else {
            return false;
        };
        let hwnd = raw.hwnd.get() as HWND;
        // Keep GPUI's render target alpha clear. Native DWM setup follows the
        // GPUI setter so an old accent cannot overwrite the requested material.
        window.set_background_appearance(appearance);
        let modern = *MICA_AVAILABLE
            .get_or_init(xross_core::system::window_effects::mica_backdrop_available);
        let material = match appearance {
            WindowBackgroundAppearance::MicaBackdrop => 2_i32,
            WindowBackgroundAppearance::MicaAltBackdrop => 4_i32,
            WindowBackgroundAppearance::Blurred if modern => 3_i32,
            _ => 1_i32,
        };
        let legacy_acrylic = appearance == WindowBackgroundAppearance::Blurred && !modern;
        unsafe {
            let accent_cleared = legacy_acrylic || clear_accent(hwnd);
            let none = 1_i32;
            DwmSetWindowAttribute(hwnd, 38, &none as *const _ as _, 4);
            let dark = i32::from(dark);
            DwmSetWindowAttribute(hwnd, 20, &dark as *const _ as _, 4);
            let round = 2_i32;
            DwmSetWindowAttribute(hwnd, 33, &round as *const _ as _, 4);
            let margin = if material != 1 { -1 } else { 0 };
            let margins = MARGINS {
                cxLeftWidth: margin,
                cxRightWidth: margin,
                cyTopHeight: margin,
                cyBottomHeight: margin,
            };
            let frame = DwmExtendFrameIntoClientArea(hwnd, &margins);
            let result = DwmSetWindowAttribute(hwnd, 38, &material as *const _ as _, 4);
            let mut actual = 0_i32;
            let readback = DwmGetWindowAttribute(hwnd, 38, &mut actual as *mut _ as _, 4);
            if std::env::var_os("XROSS_DEBUG").is_some() {
                eprintln!(
                    "[backdrop] {:?}: accent_cleared={accent_cleared}, frame={frame:#x}, result={result:#x}, readback={readback:#x}, native={actual}",
                    appearance
                );
            }
            if material != 1
                && (!accent_cleared
                    || frame < 0
                    || result < 0
                    || readback < 0
                    || actual != material)
            {
                DwmSetWindowAttribute(hwnd, 38, &none as *const _ as _, 4);
                window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                clear_accent(hwnd);
                return false;
            }
        }
        appearance != WindowBackgroundAppearance::Opaque
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materials_degrade_on_unsupported_platforms() {
        use BackdropStyle::*;
        assert_eq!(resolve_style(Auto, true, false, true, true), MicaAlt);
        assert_eq!(resolve_style(Auto, true, false, true, false), Acrylic);
        assert_eq!(resolve_style(Mica, true, false, true, false), Opaque);
        assert_eq!(resolve_style(Auto, false, true, true, false), Vibrancy);
        assert_eq!(resolve_style(Acrylic, false, true, true, false), Opaque);
        assert_eq!(resolve_style(Auto, false, false, false, false), Opaque);
        assert_eq!(resolve_style(Opaque, true, false, true, true), Opaque);
        assert_eq!(resolve_style(Auto, true, false, false, false), Opaque);
    }
}
