//! Window backdrop capabilities.
//!
//! The UI asks for a blurred window background; whether the compositor actually
//! renders one depends on the platform and, on Windows, on the user's
//! transparency preference. The answer never changes while the app runs, so it
//! is probed once and cached by the caller.

/// True when content behind a translucent window will really be blurred.
pub fn blur_backdrop_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        // NSVisualEffectView vibrancy is always available.
        true
    }
    #[cfg(target_os = "windows")]
    {
        windows_impl::acrylic_available()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Public DWM system materials require Windows 11 22H2 and transparency effects.
pub fn mica_backdrop_available() -> bool {
    #[cfg(windows)]
    {
        windows_impl::mica_available()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    /// Windows 10 1809 is the first build whose `SetWindowCompositionAttribute`
    /// exposes `ACCENT_ENABLE_ACRYLICBLURBEHIND`.
    const MIN_BUILD: u32 = 17_763;

    pub(super) fn acrylic_available() -> bool {
        transparency_effects_enabled()
            && build_number()
                .map(|build| build >= MIN_BUILD)
                .unwrap_or(false)
    }

    pub(super) fn mica_available() -> bool {
        transparency_effects_enabled() && build_number().is_some_and(|build| build >= 22_621)
    }

    /// With transparency effects switched off DWM ignores the accent policy, so
    /// the window would show the raw desktop through the sidebar instead of a
    /// blurred one.
    fn transparency_effects_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .ok()
            .and_then(|key| key.get_value::<u32, _>("EnableTransparency").ok())
            .map(|enabled| enabled != 0)
            .unwrap_or(true)
    }

    fn build_number() -> Option<u32> {
        let key = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
            .ok()?;
        key.get_value::<String, _>("CurrentBuildNumber")
            .ok()
            .or_else(|| key.get_value::<String, _>("CurrentBuild").ok())
            .and_then(|build| parse_build(&build))
    }

    /// Registry builds are strings and, on preview builds, may carry a suffix.
    fn parse_build(raw: &str) -> Option<u32> {
        raw.trim()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .filter(|digits| !digits.is_empty())
            .and_then(|digits| digits.parse().ok())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_build_numbers() {
            assert_eq!(parse_build("22631"), Some(22_631));
            assert_eq!(parse_build(" 19045 "), Some(19_045));
            assert_eq!(parse_build("26100.1742"), Some(26_100));
            assert_eq!(parse_build(""), None);
            assert_eq!(parse_build("abc"), None);
        }

        #[test]
        fn acrylic_check_is_consistent() {
            assert_eq!(acrylic_available(), acrylic_available());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_does_not_panic() {
        let _ = blur_backdrop_available();
    }
}
