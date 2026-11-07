//! OS integration: system proxy, PAC, autostart, single instance, privileges, TUN checks,
//! window backdrops.

pub mod autostart;
pub mod pac;
pub(crate) mod port_owner;
pub mod privilege;
pub mod proxy;
pub mod single_instance;
pub mod tun;
pub mod tun_process;
pub mod window_effects;

pub use pac::{build_pac, PacServer, PacServerInfo};
pub use proxy::{detect_backend, ProxyBackend, ProxyManager, ProxySnapshot};
pub use single_instance::SingleInstance;
pub use tun::{diagnose as diagnose_tun, CheckLevel, DoctorCheck};

/// Human readable platform description for the About screen.
pub fn platform_label() -> String {
    format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH)
}
