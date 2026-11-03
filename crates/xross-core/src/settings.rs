use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::CoreKind;
use crate::util::atomic_write_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    Zh,
    En,
}

impl Language {
    pub fn code(&self) -> &'static str {
        match self {
            Language::Zh => "zh-CN",
            Language::En => "en",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Language::Zh => "简体中文",
            Language::En => "English",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Window material preference; unsupported materials degrade to an opaque surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackdropStyle {
    #[default]
    Auto,
    Mica,
    MicaAlt,
    Acrylic,
    Vibrancy,
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warning",
            LogLevel::Error => "error",
        }
    }
    /// Xray uses `warning`, sing-box uses `warn`.
    pub fn xray(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warning",
            LogLevel::Error => "error",
        }
    }
    pub fn singbox(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub core: CoreKind,
    pub language: Language,
    pub theme: ThemeMode,
    pub backdrop: BackdropStyle,
    pub listen_host: String,
    pub mixed_port: u16,
    pub allow_lan: bool,
    pub api_port: u16,
    pub stats_port: u16,
    pub system_proxy: bool,
    pub pac_enabled: bool,
    pub pac_port: u16,
    pub tun_enabled: bool,
    pub autostart: bool,
    pub start_minimized: bool,
    /// Minimizing the window hides it into the tray instead of the taskbar.
    pub minimize_to_tray: bool,
    /// Closing the window hides it into the tray instead of quitting.
    pub close_to_tray: bool,
    pub single_instance: bool,
    pub log_level: LogLevel,
    pub log_max_bytes: u64,
    pub log_keep: usize,
    pub watchdog: bool,
    pub watchdog_max_retries: u32,
    pub urltest_url: String,
    pub urltest_interval_secs: u64,
    pub subscription_update_hours: u64,
    /// Version tag selected per core (empty = latest installed).
    pub active_versions: BTreeMap<String, String>,
    /// Custom binary path override per core.
    pub binary_paths: BTreeMap<String, String>,
    pub check_app_updates: bool,
    pub last_update_check: Option<i64>,
    pub show_tray_speed: bool,
    /// Masks server addresses and node locations in the UI (on by default).
    pub privacy_mode: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            core: CoreKind::SingBox,
            language: Language::Zh,
            theme: ThemeMode::System,
            backdrop: BackdropStyle::Auto,
            listen_host: "127.0.0.1".into(),
            mixed_port: 7890,
            allow_lan: false,
            api_port: 9090,
            stats_port: 10085,
            system_proxy: false,
            pac_enabled: false,
            pac_port: 7891,
            tun_enabled: false,
            autostart: false,
            start_minimized: false,
            minimize_to_tray: true,
            close_to_tray: true,
            single_instance: true,
            log_level: LogLevel::Info,
            log_max_bytes: 10 * 1024 * 1024,
            log_keep: 5,
            watchdog: true,
            watchdog_max_retries: 5,
            urltest_url: "https://www.gstatic.com/generate_204".into(),
            urltest_interval_secs: 300,
            subscription_update_hours: 12,
            check_app_updates: true,
            last_update_check: None,
            show_tray_speed: true,
            privacy_mode: true,
            active_versions: BTreeMap::new(),
            binary_paths: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    pub fn http_port(&self) -> u16 {
        self.mixed_port
    }

    pub fn active_version(&self, core: CoreKind) -> Option<&String> {
        self.active_versions
            .get(core.as_str())
            .filter(|v| !v.is_empty())
    }

    pub fn binary_override(&self, core: CoreKind) -> Option<&String> {
        self.binary_paths
            .get(core.as_str())
            .filter(|v| !v.is_empty())
    }

    /// TUN streaming on Xray needs an external stack; UI and validator call this.
    pub fn effective_tun(&self) -> bool {
        self.tun_enabled && self.core.capabilities().native_tun
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_keep_defaults_and_material_preference_roundtrips() {
        let old: Settings = serde_json::from_str(r#"{"language":"en","mixed_port":8899}"#).unwrap();
        assert_eq!(old.language, Language::En);
        assert_eq!(old.mixed_port, 8899);
        assert_eq!(old.backdrop, BackdropStyle::Auto);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            backdrop: BackdropStyle::MicaAlt,
            ..old
        };
        settings.save(&path).unwrap();
        let restored = Settings::load(&path).unwrap();
        assert_eq!(restored, settings);
        let json = std::fs::read_to_string(path).unwrap();
        assert!(json.contains("mica_alt"));
    }
}
