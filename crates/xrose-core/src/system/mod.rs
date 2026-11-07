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
