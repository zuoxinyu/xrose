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
    pub tun_strict_route: bool,
    pub autostart: bool,
    pub autostart_proxy: bool,
    /// Explicit profile identities; None leaves that provider disconnected at login.
    pub autostart_tailscale: Option<String>,
    pub autostart_easytier: Option<String>,
    pub system_notifications: bool,
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
            tun_strict_route: true,
            autostart: false,
            autostart_proxy: false,
            autostart_tailscale: None,
            autostart_easytier: None,
            system_notifications: true,
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
    pub fn login_enabled(&self) -> bool {
        self.autostart
            || self.autostart_proxy
            || self.autostart_tailscale.is_some()
            || self.autostart_easytier.is_some()
    }

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

    /// Whether changing these preferences affects the selected core's next run.
    /// Client preferences and controls applied through runtime APIs are excluded.
    pub(crate) fn requires_core_restart(&self, old: &Self, model: &crate::model::AppModel) -> bool {
        use crate::model::{GroupKind, InboundKind};
        if self.core != old.core {
            return true;
        }
        let core = self.core;
        if self.binary_override(core) != old.binary_override(core)
            || (self.binary_override(core).is_none()
                && self.active_version(core) != old.active_version(core))
        {
            return true;
        }
        if model
            .inbounds
            .iter()
            .any(|i| i.enabled && i.kind == InboundKind::Mixed)
        {
            let address = |s: &Self| {
                if s.allow_lan {
                    "0.0.0.0".to_string()
                } else if s.listen_host.is_empty() {
                    "127.0.0.1".to_string()
                } else {
                    s.listen_host.clone()
                }
            };
            if self.mixed_port != old.mixed_port || address(self) != address(old) {
                return true;
            }
        }
        if model.inbounds.iter().any(|i| i.kind == InboundKind::Tun)
            && self.effective_tun() != old.effective_tun()
        {
            return true;
        }
        if self.effective_tun() && self.tun_strict_route != old.tun_strict_route {
            return true;
        }
        let log_changed = match core {
            CoreKind::Xray => self.log_level.xray() != old.log_level.xray(),
            CoreKind::SingBox => self.log_level.singbox() != old.log_level.singbox(),
        };
        if log_changed
            || match core {
                CoreKind::Xray => self.stats_port != old.stats_port,
                CoreKind::SingBox => self.api_port != old.api_port,
            }
        {
            return true;
        }
        let automatic = model.groups.iter().filter(|g| {
            g.enabled
                && g.members
                    .iter()
                    .filter_map(|id| model.node(id))
                    .any(|n| n.usable_with(core))
        });
        match core {
            CoreKind::Xray => {
                automatic
                    .clone()
                    .any(|g| matches!(g.kind, GroupKind::UrlTest | GroupKind::Fallback))
                    && (self.urltest_url != old.urltest_url
                        || self.urltest_interval_secs.max(30) != old.urltest_interval_secs.max(30))
            }
            CoreKind::SingBox => {
                automatic.clone().any(|g| {
                    matches!(g.kind, GroupKind::UrlTest | GroupKind::Fallback) && g.url.is_none()
                }) && self.urltest_url != old.urltest_url
            }
        }
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

    /// Whether the selected core should start a native TUN inbound.
    pub fn effective_tun(&self) -> bool {
        self.tun_enabled && self.core.capabilities().native_tun
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn independent_login_choices_roundtrip_and_old_settings_default_off() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.login_enabled());
        assert!(settings.autostart_tailscale.is_none());
        settings.autostart_proxy = true;
        assert!(settings.login_enabled());
        settings.autostart_proxy = false;
        settings.autostart_tailscale = Some("test-tailnet".into());
        settings.autostart_easytier = Some("test-network".into());
        assert!(settings.login_enabled());
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&settings).unwrap()).unwrap(),
            settings
        );
    }

    #[test]
    fn client_preferences_do_not_require_a_core_restart() {
        let old = Settings::default();
        let model = crate::model::AppModel::default();
        let mut new = old.clone();
        new.language = Language::En;
        new.theme = ThemeMode::Dark;
        new.backdrop = BackdropStyle::Opaque;
        new.privacy_mode = !old.privacy_mode;
        new.show_tray_speed = !old.show_tray_speed;
        new.minimize_to_tray = !old.minimize_to_tray;
        new.close_to_tray = !old.close_to_tray;
        new.autostart = !old.autostart;
        new.start_minimized = !old.start_minimized;
        new.single_instance = !old.single_instance;
        new.system_proxy = !old.system_proxy;
        new.pac_enabled = !old.pac_enabled;
        new.pac_port += 10;
        new.watchdog = !old.watchdog;
        new.watchdog_max_retries += 1;
        new.log_max_bytes += 1024;
        new.log_keep += 1;
        new.subscription_update_hours += 1;
        new.check_app_updates = !old.check_app_updates;
        new.last_update_check = Some(123);
        for core in CoreKind::all() {
            let mut baseline = old.clone();
            baseline.core = core;
            new.core = core;
            assert!(!new.requires_core_restart(&baseline, &model));
        }
    }

    #[test]
    fn core_changes_and_only_the_selected_cores_api_and_binary_require_restart() {
        let model = crate::model::AppModel::default();
        for core in CoreKind::all() {
            let old = Settings {
                core,
                ..Default::default()
            };
            let mutations: &[fn(&mut Settings)] = &[
                |s| s.mixed_port += 1,
                |s| s.allow_lan = true,
                |s| s.listen_host = "::1".into(),
                |s| s.tun_enabled = true,
                |s| s.log_level = LogLevel::Error,
                |s| {
                    s.active_versions
                        .insert(s.core.as_str().into(), "test-version".into());
                },
                |s| {
                    s.binary_paths
                        .insert(s.core.as_str().into(), "test-binary".into());
                },
            ];
            for mutation in mutations {
                let mut new = old.clone();
                mutation(&mut new);
                assert!(new.requires_core_restart(&old, &model));
            }
            let other = if core == CoreKind::Xray {
                CoreKind::SingBox
            } else {
                CoreKind::Xray
            };
            let mut new = old.clone();
            new.core = other;
            assert!(new.requires_core_restart(&old, &model));
            let mut new = old.clone();
            new.active_versions
                .insert(other.as_str().into(), "test-version".into());
            new.binary_paths
                .insert(other.as_str().into(), "test-binary".into());
            assert!(!new.requires_core_restart(&old, &model));
            new.api_port += 1;
            assert_eq!(
                new.requires_core_restart(&old, &model),
                core == CoreKind::SingBox
            );
            new.api_port = old.api_port;
            new.stats_port += 1;
            assert_eq!(
                new.requires_core_restart(&old, &model),
                core == CoreKind::Xray
            );
        }
        let old = Settings {
            core: CoreKind::Xray,
            log_level: LogLevel::Trace,
            allow_lan: true,
            ..Default::default()
        };
        let mut new = old.clone();
        new.log_level = LogLevel::Debug;
        new.listen_host = "::1".into();
        assert!(new.requires_core_restart(&old, &model));
        new.log_level = old.log_level;
        assert!(!new.requires_core_restart(&old, &model));
    }

    #[test]
    fn latency_preferences_restart_only_groups_that_embed_the_defaults() {
        use crate::model::{AppModel, GroupKind, Node, OutboundGroup, Protocol};
        let mut model = AppModel::default();
        let node = Node::new("fixture", Protocol::Http, "example.com", 80);
        model.add_nodes(vec![node.clone()]);
        model.groups.clear();
        let old = Settings::default();
        let mut new = old.clone();
        new.urltest_url = "https://example.com/check".into();
        assert!(!new.requires_core_restart(&old, &model));
        let mut group = OutboundGroup::new("automatic", GroupKind::UrlTest);
        group.members = vec![node.id];
        model.groups.push(group);
        assert!(new.requires_core_restart(&old, &model));
        model.groups.last_mut().unwrap().url = Some("https://example.com/custom".into());
        assert!(!new.requires_core_restart(&old, &model));
        let old = Settings {
            core: CoreKind::Xray,
            ..old
        };
        let mut new = old.clone();
        new.urltest_interval_secs += 1;
        assert!(new.requires_core_restart(&old, &model));
        new.core = CoreKind::SingBox;
        let old = Settings {
            core: CoreKind::SingBox,
            ..old
        };
        assert!(!new.requires_core_restart(&old, &model));
    }

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
