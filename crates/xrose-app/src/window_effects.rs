//! Native window materials shared by the main window and tray flyout.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[cfg(any(windows, target_os = "macos"))]
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Theme;
use gpui_kit::{App, Window, WindowBackgroundAppearance};
use parking_lot::RwLock;
use xrose_core::settings::BackdropStyle;

static STYLE: RwLock<BackdropStyle> = RwLock::new(BackdropStyle::Auto);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FOLLOW_SYSTEM: AtomicBool = AtomicBool::new(true);
#[cfg(not(target_os = "linux"))]
static BLUR_AVAILABLE: OnceLock<bool> = OnceLock::new();
static MICA_AVAILABLE: OnceLock<bool> = OnceLock::new();

pub fn configure(style: BackdropStyle) {
    *STYLE.write() = style;
    ACTIVE.store(
        effective_style(style) != BackdropStyle::Opaque,
        Ordering::Relaxed,
    );
}

pub fn set_follow_system(follow: bool) {
    FOLLOW_SYSTEM.store(follow, Ordering::Relaxed);
}

/// Clear any native per-window appearance override and sync `Theme` with the
/// operating system's current light/dark setting.
pub fn sync_system_appearance(window: &mut Window, cx: &mut App) {
    set_follow_system(true);
    #[cfg(target_os = "macos")]
    macos::clear_appearance_override(window);
    Theme::sync_system_appearance(Some(window), cx);
}

pub fn blur_backdrop() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn effective_style(style: BackdropStyle) -> BackdropStyle {
    #[cfg(target_os = "linux")]
    let blur = crate::wayland::desktop().blur;
    #[cfg(not(target_os = "linux"))]
    let blur =
        *BLUR_AVAILABLE.get_or_init(xrose_core::system::window_effects::blur_backdrop_available);
    let mica =
        *MICA_AVAILABLE.get_or_init(xrose_core::system::window_effects::mica_backdrop_available);
    resolve_style(
        style,
        cfg!(windows),
        cfg!(any(target_os = "macos", target_os = "linux")),
        blur,
        mica,
    )
}

fn resolve_style(
    style: BackdropStyle,
    windows: bool,
    frosted: bool,
    blur: bool,
    mica: bool,
) -> BackdropStyle {
    use BackdropStyle::*;
    match style {
        Auto if windows && mica => MicaAlt,
        Auto if windows && blur => Acrylic,
        Auto if frosted && blur => Vibrancy,
        Mica | MicaAlt if windows && mica => style,
        Acrylic if windows && blur => Acrylic,
        Vibrancy if frosted && blur => Vibrancy,
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

/// Normal, hover, and active backgrounds for bordered default/secondary buttons.
/// Normal fill stays fully transparent so buttons inherit their host card, tinted
/// selection row, or translucent window surface.
pub(crate) fn secondary_button_surfaces(
    dark: bool,
) -> (gpui_kit::Hsla, gpui_kit::Hsla, gpui_kit::Hsla) {
    let clear = gpui_kit::hsla(0., 0., 0., 0.);
    if dark {
        (
            clear,
            gpui_kit::hsla(0., 0., 1., 0.08),
            gpui_kit::hsla(0., 0., 1., 0.14),
        )
    } else {
        (
            clear,
            gpui_kit::hsla(0., 0., 0., 0.05),
            gpui_kit::hsla(0., 0., 0., 0.09),
        )
    }
}

/// Root surfaces stay clear; each view supplies its own opaque fallback fill.
/// Also configures high-contrast input borders and readable muted foreground
/// across both dark and light modes.
pub fn make_surface_translucent(cx: &mut App) {
    Theme::update(cx, |theme| {
        #[cfg(windows)]
        {
            theme.font_family = "Segoe UI".into();
        }
        theme.tokens.background = gpui_kit::hsla(0., 0., 0., 0.).into();
        let dark = theme.mode.is_dark();
        theme.tokens.switch = crate::ui::theme::switch_off().into();
        // One interaction color (cyan) for primary buttons, toggles and focus;
        // the soft tint drives hover/selection surfaces. Red stays reserved for
        // destructive actions.
        let (primary, primary_fg) = crate::ui::theme::interaction(dark);
        let (hover, active) = if dark {
            (
                crate::ui::theme::hsl(188., 0.86, 0.60),
                crate::ui::theme::hsl(188., 0.86, 0.46),
            )
        } else {
            (
                crate::ui::theme::hsl(192., 0.81, 0.27),
                crate::ui::theme::hsl(192., 0.81, 0.23),
            )
        };
        let tint = crate::ui::theme::interaction_tint(dark);
        theme.colors.primary = primary;
        theme.tokens.primary = primary.into();
        theme.colors.primary_foreground = primary_fg;
        theme.tokens.primary_foreground = primary_fg.into();
        theme.colors.primary_hover = hover;
        theme.tokens.primary_hover = hover.into();
        theme.colors.primary_active = active;
        theme.tokens.primary_active = active.into();
        // Button tokens are independent from the interaction tokens in GPUI Kit.
        let (btn_bg, btn_hover, btn_active) = secondary_button_surfaces(dark);
        theme.colors.button = btn_bg;
        theme.tokens.button = btn_bg.into();
        theme.colors.button_hover = btn_hover;
        theme.tokens.button_hover = btn_hover.into();
        theme.colors.button_active = btn_active;
        theme.tokens.button_active = btn_active.into();
        theme.colors.button_secondary = btn_bg;
        theme.tokens.button_secondary = btn_bg.into();
        theme.colors.button_secondary_hover = btn_hover;
        theme.tokens.button_secondary_hover = btn_hover.into();
        theme.colors.button_secondary_active = btn_active;
        theme.tokens.button_secondary_active = btn_active.into();
        theme.colors.button_primary = theme.colors.foreground;
        theme.tokens.button_primary = theme.colors.button_primary.into();
        theme.colors.button_primary_foreground = if dark {
            theme.colors.secondary
        } else {
            crate::ui::theme::hsl(0., 0., 1.)
        };
        theme.tokens.button_primary_foreground = theme.colors.button_primary_foreground.into();
        theme.colors.button_primary_hover = theme.colors.muted_foreground;
        theme.tokens.button_primary_hover = theme.colors.button_primary_hover.into();
        theme.colors.button_primary_active = theme.colors.foreground;
        theme.tokens.button_primary_active = theme.colors.button_primary_active.into();
        theme.colors.accent = tint;
        theme.tokens.accent = tint.into();
        theme.colors.ring = primary;
        theme.tokens.ring = primary.into();
        if dark {
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
    #[cfg(target_os = "macos")]
    let active = macos::apply(
        window,
        appearance,
        cx.theme().mode.is_dark(),
        FOLLOW_SYSTEM.load(Ordering::Relaxed),
    );
    #[cfg(not(any(windows, target_os = "macos")))]
    let active = {
        #[cfg(target_os = "linux")]
        let native = crate::wayland::main_window::background(window, cx, appearance);
        #[cfg(not(target_os = "linux"))]
        let native = appearance;
        window.set_background_appearance(native);
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

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use cocoa::base::{id, nil};
    use cocoa::foundation::NSString;
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn clear_appearance_override(window: &mut Window) {
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(raw) = handle.as_raw() {
                unsafe {
                    let view = raw.ns_view.as_ptr() as id;
                    let ns_win: id = msg_send![view, window];
                    if ns_win != nil {
                        let null_appearance: id = nil;
                        let _: () = msg_send![ns_win, setAppearance: null_appearance];
                    }
                    let null_appearance: id = nil;
                    let _: () = msg_send![view, setAppearance: null_appearance];
                }
            }
        }
    }

    pub fn apply(
        window: &mut Window,
        appearance: WindowBackgroundAppearance,
        dark: bool,
        follow_system: bool,
    ) -> bool {
        window.set_background_appearance(appearance);
        if follow_system {
            clear_appearance_override(window);
        } else if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::AppKit(raw) = handle.as_raw() {
                unsafe {
                    let view = raw.ns_view.as_ptr() as id;
                    let ns_win: id = msg_send![view, window];
                    let name = if dark {
                        "NSAppearanceNameDarkAqua"
                    } else {
                        "NSAppearanceNameAqua"
                    };
                    let name_ns = NSString::alloc(nil).init_str(name);
                    if name_ns != nil {
                        let ns_appearance: id =
                            msg_send![class!(NSAppearance), appearanceNamed: name_ns];
                        let _: () = msg_send![name_ns, release];
                        if ns_appearance != nil {
                            if ns_win != nil {
                                let _: () = msg_send![ns_win, setAppearance: ns_appearance];
                            }
                            let _: () = msg_send![view, setAppearance: ns_appearance];
                        }
                    }
                }
            }
        }
        appearance != WindowBackgroundAppearance::Opaque
    }
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
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, GWL_STYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION, WS_EX_TOOLWINDOW,
        WS_SYSMENU,
    };

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

    /// Compute the target `GWL_STYLE` when toggling DWM's native caption buttons.
    ///
    /// Fullscreen windows lack `WS_CAPTION` and are left untouched. Tool windows
    /// (`WS_EX_TOOLWINDOW`, such as GPUI's `WindowKind::PopUp` tray flyout) receive
    /// `WS_CAPTION` from `CreateWindowExW` when created with `WS_OVERLAPPED`, but
    /// are intentionally created without `WS_SYSMENU`; never enable `WS_SYSMENU` on
    /// them, or DWM composites a native close button in the top-right corner under
    /// Acrylic (`DwmExtendFrameIntoClientArea(-1)`).
    pub(super) fn resolve_caption_button_style(style: u32, ex_style: u32, visible: bool) -> u32 {
        if (style & WS_CAPTION) != WS_CAPTION {
            return style;
        }
        if (ex_style & WS_EX_TOOLWINDOW) != 0 {
            return style & !WS_SYSMENU;
        }
        if visible {
            style | WS_SYSMENU
        } else {
            style & !WS_SYSMENU
        }
    }

    /// Toggle DWM's native non-client caption buttons on captioned top-level windows.
    ///
    /// With `DwmExtendFrameIntoClientArea(-1)` and a translucent titlebar, DWM still
    /// composites native minimize/maximize/close glyphs whenever `WS_SYSMENU` is set,
    /// causing them to overlap Xrose's custom `WindowControls`. Clearing `WS_SYSMENU`
    /// while keeping `WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX`
    /// removes the DWM buttons (`DWMWA_CAPTION_BUTTON_BOUNDS` width becomes 0) while
    /// preserving borders, rounded corners, maximized work-area bounds, double-click
    /// maximize, `Alt+F4`, and `SC_*` commands.
    unsafe fn set_native_caption_buttons(hwnd: HWND, visible: bool) {
        unsafe {
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            let desired = resolve_caption_button_style(style, ex_style, visible);
            if desired != style {
                SetWindowLongW(hwnd, GWL_STYLE, desired as i32);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
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
            .get_or_init(xrose_core::system::window_effects::mica_backdrop_available);
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
            let translucent_header = matches!(material, 2 | 4);
            set_native_caption_buttons(hwnd, !translucent_header);
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
            if std::env::var_os("XROSE_DEBUG").is_some() {
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
                let zero_margins = MARGINS {
                    cxLeftWidth: 0,
                    cxRightWidth: 0,
                    cyTopHeight: 0,
                    cyBottomHeight: 0,
                };
                DwmExtendFrameIntoClientArea(hwnd, &zero_margins);
                set_native_caption_buttons(hwnd, true);
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

    #[test]
    fn secondary_buttons_use_transparent_normal_fill() {
        for dark in [false, true] {
            let (normal, hover, active) = secondary_button_surfaces(dark);
            assert_eq!(normal.a, 0.0);
            assert!(hover.a > 0.0 && hover.a < active.a);
        }
    }

    #[cfg(windows)]
    #[test]
    fn tool_windows_never_gain_native_close_button() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_SYSMENU,
        };

        // Tray flyout (`WindowKind::PopUp`): has WS_CAPTION + WS_EX_TOOLWINDOW, no WS_SYSMENU.
        assert_eq!(
            windows::resolve_caption_button_style(WS_CAPTION, WS_EX_TOOLWINDOW, true),
            WS_CAPTION
        );
        assert_eq!(
            windows::resolve_caption_button_style(WS_CAPTION | WS_SYSMENU, WS_EX_TOOLWINDOW, true),
            WS_CAPTION
        );

        // Main window: toggles WS_SYSMENU based on visibility.
        assert_eq!(
            windows::resolve_caption_button_style(WS_CAPTION | WS_SYSMENU, WS_EX_APPWINDOW, false),
            WS_CAPTION
        );
        assert_eq!(
            windows::resolve_caption_button_style(WS_CAPTION, WS_EX_APPWINDOW, true),
            WS_CAPTION | WS_SYSMENU
        );

        // Fullscreen window: no WS_CAPTION, left untouched.
        assert_eq!(
            windows::resolve_caption_button_style(0, WS_EX_APPWINDOW, true),
            0
        );
    }
}
