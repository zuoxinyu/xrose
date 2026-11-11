//! OS integration: system proxy, PAC, autostart, single instance, privileges, TUN checks,
//! window backdrops.

pub mod autostart;
pub mod nat;
pub mod net_diag;
pub mod pac;
pub(crate) mod port_owner;
pub mod privilege;
pub mod proxy;
pub mod single_instance;
pub mod tun;
pub mod tun_process;
pub mod window_effects;

pub use pac::{PacServer, PacServerInfo, build_pac};
pub use proxy::{ProxyBackend, ProxyManager, ProxySnapshot, detect_backend};
pub use single_instance::SingleInstance;
pub use tun::{CheckLevel, DoctorCheck, diagnose as diagnose_tun};

/// Human readable platform description for the About screen.
pub fn platform_label() -> String {
    format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Pass a URL directly to the desktop launcher without a command shell.
pub(crate) async fn open_url(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let target: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        result as isize > 32
    }
    #[cfg(not(target_os = "windows"))]
    {
        let program = if cfg!(target_os = "macos") {
            "/usr/bin/open"
        } else {
            "xdg-open"
        };
        tokio::process::Command::new(program)
            .arg(url)
            .status()
            .await
            .is_ok_and(|status| status.success())
    }
}
