//! Cross-platform system proxy with crash-safe restore.
//!
//! Before touching the desktop settings we snapshot the current values to
//! `proxy-snapshot.json`. If the app crashes while the proxy is on, the next
//! launch restores that snapshot before doing anything else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_context};
use crate::util::now_unix;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SystemProxyStatus {
    Unknown,
    Off,
    Xrose,
    Partial,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxySnapshot {
    pub backend: String,
    /// Raw backend values captured before applying (gsettings/kreadconfig/registry).
    pub entries: BTreeMap<String, String>,
    // Per-service values written by Xrose. During a transition either side
    // of a write may be present after a crash.
    #[cfg(target_os = "macos")]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    macos_writes: BTreeMap<String, Vec<macos::Value>>,
    pub applied_at: i64,
    pub http_host: String,
    pub http_port: u16,
    pub pac_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyBackend {
    Gnome,
    Kde,
    MacOs,
    Windows,
    None,
}

impl ProxyBackend {
    pub fn label(&self) -> &'static str {
        match self {
            ProxyBackend::Gnome => "GNOME (gsettings)",
            ProxyBackend::Kde => "KDE (kioslaverc)",
            ProxyBackend::MacOs => "macOS networksetup",
            ProxyBackend::Windows => "Windows Internet Settings",
            ProxyBackend::None => "未检测到",
        }
    }
}

pub fn detect_backend() -> ProxyBackend {
    #[cfg(target_os = "macos")]
    {
        return ProxyBackend::MacOs;
    }
    #[cfg(target_os = "windows")]
    {
        return ProxyBackend::Windows;
    }
    #[cfg(target_os = "linux")]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if desktop.contains("kde") || desktop.contains("plasma") {
            return ProxyBackend::Kde;
        }
        if command_ok("gsettings", &["help", "get"]) {
            return ProxyBackend::Gnome;
        }
        if desktop.contains("gnome")
            || desktop.contains("unity")
            || desktop.contains("cinnamon")
            || desktop.contains("budgie")
        {
            return ProxyBackend::Gnome;
        }
        ProxyBackend::None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        ProxyBackend::None
    }
}

#[allow(dead_code)]
fn command_ok(program: &str, args: &[&str]) -> bool {
    match Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::other(format!("执行 {program} 失败：{e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::other(format!(
            "{program} {} 失败：{}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// GNOME / gsettings
// ---------------------------------------------------------------------------

#[allow(dead_code)]
const GNOME_KEYS: [(&str, &str, &str); 9] = [
    ("org.gnome.system.proxy", "mode", "none"),
    ("org.gnome.system.proxy", "autoconfig-url", "''"),
    ("org.gnome.system.proxy", "ignore-hosts", "[]"),
    ("org.gnome.system.proxy.http", "host", "''"),
    ("org.gnome.system.proxy.http", "port", "0"),
    ("org.gnome.system.proxy.http", "use-authentication", "false"),
    ("org.gnome.system.proxy", "use-same-proxy", "false"),
    ("org.gnome.system.proxy.socks", "host", "''"),
    ("org.gnome.system.proxy.socks", "port", "0"),
];

#[cfg(target_os = "linux")]
fn gnome_capture() -> Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for (schema, key, fallback) in GNOME_KEYS {
        let value =
            run("gsettings", &["get", schema, key]).unwrap_or_else(|_| fallback.to_string());
        entries.insert(format!("{schema}/{key}"), value);
    }
    Ok(entries)
}

#[cfg(target_os = "linux")]
fn gnome_set(schema: &str, key: &str, value: &str) -> Result<()> {
    run("gsettings", &["set", schema, key, value]).map(|_| ())
}

#[cfg(target_os = "linux")]
fn gnome_apply(host: &str, port: u16, pac_url: Option<&str>) -> Result<()> {
    match pac_url {
        Some(url) => {
            gnome_set(
                "org.gnome.system.proxy",
                "autoconfig-url",
                &format!("'{url}'"),
            )?;
            gnome_set("org.gnome.system.proxy.http", "host", "''")?;
            gnome_set("org.gnome.system.proxy", "mode", "'auto'")?;
        }
        None => {
            gnome_set("org.gnome.system.proxy.http", "host", &format!("'{host}'"))?;
            gnome_set("org.gnome.system.proxy.http", "port", &port.to_string())?;
            gnome_set("org.gnome.system.proxy.https", "host", &format!("'{host}'"))?;
            gnome_set("org.gnome.system.proxy.https", "port", &port.to_string())?;
            gnome_set("org.gnome.system.proxy.socks", "host", &format!("'{host}'"))?;
            gnome_set("org.gnome.system.proxy.socks", "port", &port.to_string())?;
            gnome_set("org.gnome.system.proxy", "use-same-proxy", "true")?;
            gnome_set(
                "org.gnome.system.proxy",
                "ignore-hosts",
                "['localhost', '127.0.0.0/8', '::1', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16']",
            )?;
            gnome_set("org.gnome.system.proxy", "mode", "'manual'")?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn gnome_restore(entries: &BTreeMap<String, String>) -> Result<()> {
    // Order matters: restoring the mode last avoids flapping.
    let order = |key: &str| if key.ends_with("/mode") { 1 } else { 0 };
    let mut items: Vec<_> = entries.iter().collect();
    items.sort_by_key(|(key, _)| order(key));
    for (full_key, value) in items {
        let Some((schema, key)) = full_key.split_once('/') else {
            continue;
        };
        let _ = gnome_set(schema, key, value);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// KDE / kioslaverc
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn kde_read(key: &str) -> Option<String> {
    let value = run(
        "kreadconfig5",
        &[
            "--file",
            "kioslaverc",
            "--group",
            "Proxy Settings",
            "--key",
            key,
        ],
    )
    .ok()
    .filter(|v| !v.is_empty())
    .or_else(|| {
        run(
            "kreadconfig6",
            &[
                "--file",
                "kioslaverc",
                "--group",
                "Proxy Settings",
                "--key",
                key,
            ],
        )
        .ok()
        .filter(|v| !v.is_empty())
    });
    value
}

#[cfg(target_os = "linux")]
fn kde_write(key: &str, value: &str) -> Result<()> {
    let program = if command_ok("kwriteconfig5", &["--help"]) {
        "kwriteconfig5"
    } else {
        "kwriteconfig6"
    };
    run(
        program,
        &[
            "--file",
            "kioslaverc",
            "--group",
            "Proxy Settings",
            "--key",
            key,
            value,
        ],
    )
    .map(|_| ())
}

#[cfg(target_os = "linux")]
fn kde_capture() -> BTreeMap<String, String> {
    let keys = [
        "ProxyType",
        "httpProxy",
        "httpsProxy",
        "socksProxy",
        "NoProxyFor",
        "Proxy Config Script",
    ];
    keys.iter()
        .filter_map(|key| kde_read(key).map(|value| (key.to_string(), value)))
        .collect()
}

#[cfg(target_os = "linux")]
fn kde_apply(host: &str, port: u16, pac_url: Option<&str>) -> Result<()> {
    match pac_url {
        Some(url) => {
            kde_write("Proxy Config Script", url)?;
            kde_write("ProxyType", "2")?;
        }
        None => {
            kde_write("httpProxy", &format!("http://{host} {port}"))?;
            kde_write("httpsProxy", &format!("http://{host} {port}"))?;
            kde_write("socksProxy", &format!("socks://{host} {port}"))?;
            kde_write(
                "NoProxyFor",
                "localhost,127.0.0.1,::1,10.0.0.0/8,172.16.0.0/12,192.168.0.0/16",
            )?;
            kde_write("ProxyType", "1")?;
        }
    }
    // Ask KDE to reload proxy settings if possible.
    let _ = Command::new("dbus-send")
        .args([
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:''",
        ])
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn kde_restore(entries: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in entries {
        let _ = kde_write(key, value);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) mod observer;

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    fn read(name: &str) -> Option<String> {
        use winreg::RegKey;
        let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let key = hkcu.open_subkey(KEY).ok()?;
        key.get_value::<String, _>(name)
            .ok()
            .or_else(|| key.get_value::<u32, _>(name).ok().map(|v| v.to_string()))
    }

    fn write(name: &str, value: &str) -> Result<()> {
        use winreg::RegKey;
        let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let key = hkcu
            .create_subkey(KEY)
            .map_err(|e| Error::other(format!("打开注册表失败：{e}")))?
            .0;
        // ProxyEnable and ProxyOverride are DWORD / REG_SZ respectively.
        if name == "ProxyEnable" {
            let dword: u32 = value.parse().unwrap_or(0);
            key.set_value(name, &dword)
                .map_err(|e| Error::other(format!("写入注册表失败：{e}")))?;
        } else {
            key.set_value(name, &value.to_string())
                .map_err(|e| Error::other(format!("写入注册表失败：{e}")))?;
        }
        Ok(())
    }

    pub fn refresh() {
        use windows_sys::Win32::Networking::WinInet::{
            INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
        };
        unsafe {
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null(),
                0,
            );
        }
    }

    pub fn capture() -> BTreeMap<String, String> {
        [
            "ProxyEnable",
            "ProxyServer",
            "AutoConfigURL",
            "ProxyOverride",
        ]
        .iter()
        .filter_map(|name| read(name).map(|value| (name.to_string(), value)))
        .collect()
    }

    pub fn apply(host: &str, port: u16, pac_url: Option<&str>) -> Result<()> {
        match pac_url {
            Some(url) => {
                write("AutoConfigURL", url)?;
                write("ProxyEnable", "0")?;
            }
            None => {
                write("ProxyServer", &format!("{host}:{port}"))?;
                write(
                    "ProxyOverride",
                    "localhost;127.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;192.168.*;<local>",
                )?;
                write("ProxyEnable", "1")?;
                let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";
                use winreg::RegKey;
                let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
                if let Ok((key, _)) = hkcu.create_subkey(key) {
                    let _ = key.delete_value("AutoConfigURL");
                }
            }
        }
        refresh();
        Ok(())
    }

    pub fn restore(entries: &BTreeMap<String, String>) -> Result<()> {
        for (name, value) in entries {
            let _ = write(name, value);
        }
        refresh();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct ProxyManager {
    snapshot_path: PathBuf,
    backend: ProxyBackend,
}

impl ProxyManager {
    pub fn new(snapshot_path: PathBuf) -> Self {
        Self {
            snapshot_path,
            backend: detect_backend(),
        }
    }

    pub fn backend(&self) -> ProxyBackend {
        self.backend.clone()
    }

    pub fn has_snapshot(&self) -> bool {
        self.snapshot_path.exists()
    }

    fn read_snapshot(&self) -> Result<ProxySnapshot> {
        Ok(serde_json::from_slice(&std::fs::read(
            &self.snapshot_path,
        )?)?)
    }

    pub fn load_snapshot(&self) -> Option<ProxySnapshot> {
        std::fs::read(&self.snapshot_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    }

    pub(super) fn capture(&self) -> Result<BTreeMap<String, String>> {
        match self.backend {
            ProxyBackend::Gnome => {
                #[cfg(target_os = "linux")]
                {
                    return gnome_capture();
                }
                #[allow(unreachable_code)]
                Ok(BTreeMap::new())
            }
            ProxyBackend::Kde => {
                #[cfg(target_os = "linux")]
                {
                    return Ok(kde_capture());
                }
                #[allow(unreachable_code)]
                Ok(BTreeMap::new())
            }
            ProxyBackend::MacOs => {
                #[cfg(target_os = "macos")]
                {
                    return macos::capture();
                }
                #[allow(unreachable_code)]
                Ok(BTreeMap::new())
            }
            ProxyBackend::Windows => {
                #[cfg(target_os = "windows")]
                {
                    return Ok(windows_impl::capture());
                }
                #[allow(unreachable_code)]
                Ok(BTreeMap::new())
            }
            ProxyBackend::None => Err(Error::other(
                "未检测到支持的桌面环境，无法设置系统代理（可手动配置 127.0.0.1:7890）",
            )),
        }
    }

    /// Applies the system proxy, snapshotting the previous state first.
    pub fn apply(&self, host: &str, port: u16, pac_url: Option<&str>) -> Result<ProxySnapshot> {
        self.apply_endpoints(host, port, Some((host, port)), pac_url, false)
    }

    pub(crate) fn apply_endpoints(
        &self,
        host: &str,
        port: u16,
        _socks: Option<(&str, u16)>,
        pac_url: Option<&str>,
        _refresh: bool,
    ) -> Result<ProxySnapshot> {
        let entries = self.capture()?;
        let snapshot = if self.has_snapshot() {
            let mut snapshot = self.read_snapshot()?;
            for (key, raw) in &entries {
                snapshot
                    .entries
                    .entry(key.clone())
                    .or_insert_with(|| raw.clone());
            }
            snapshot
        } else {
            ProxySnapshot {
                #[cfg(target_os = "macos")]
                macos_writes: Default::default(),
                backend: self.backend.label().into(),
                entries: entries.clone(),
                applied_at: now_unix(),
                http_host: host.into(),
                http_port: port,
                pac_url: pac_url.map(str::to_string),
            }
        };
        #[cfg(target_os = "macos")]
        if self.backend == ProxyBackend::MacOs {
            let mut snapshot = snapshot;
            let targets = macos::targets(host, port, _socks, pac_url);
            macos::apply(
                &entries,
                &mut snapshot,
                &targets,
                _refresh,
                &self.snapshot_path,
            )?;
            return Ok(snapshot);
        }
        crate::util::atomic_write_private(&self.snapshot_path, &serde_json::to_vec(&snapshot)?)?;
        match self.backend {
            ProxyBackend::Gnome => {
                #[cfg(target_os = "linux")]
                gnome_apply(host, port, pac_url)?;
            }
            ProxyBackend::Kde => {
                #[cfg(target_os = "linux")]
                kde_apply(host, port, pac_url)?;
            }
            ProxyBackend::MacOs => unreachable!("macOS has its own snapshot transaction"),
            ProxyBackend::Windows => {
                #[cfg(target_os = "windows")]
                windows_impl::apply(host, port, pac_url)?;
            }
            ProxyBackend::None => {
                return Err(Error::other("未检测到支持的桌面环境"));
            }
        }
        Ok(snapshot)
    }

    /// Restores the snapshot and removes it.
    pub fn restore(&self) -> Result<()> {
        if !self.has_snapshot() {
            return Ok(());
        }
        let snapshot = self.read_snapshot()?;
        match self.backend {
            ProxyBackend::Gnome => {
                #[cfg(target_os = "linux")]
                gnome_restore(&snapshot.entries)?;
            }
            ProxyBackend::Kde => {
                #[cfg(target_os = "linux")]
                kde_restore(&snapshot.entries)?;
            }
            ProxyBackend::MacOs => {
                #[cfg(target_os = "macos")]
                macos::restore(snapshot, &self.snapshot_path)?;
            }
            ProxyBackend::Windows => {
                #[cfg(target_os = "windows")]
                windows_impl::restore(&snapshot.entries)?;
            }
            ProxyBackend::None => {}
        }
        std::fs::remove_file(&self.snapshot_path)
            .map_err(|e| io_context(&self.snapshot_path, e))?;
        Ok(())
    }

    /// Called at startup: if a snapshot exists, the previous run crashed with
    /// the proxy on, so restore it before showing the window.
    pub fn recover_after_crash(&self) -> Option<ProxySnapshot> {
        let snapshot = self.load_snapshot()?;
        self.restore().ok()?;
        Some(snapshot)
    }
}

pub fn snapshot_file_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_snapshot_remains_available_after_failed_restore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        std::fs::write(&path, b"{").unwrap();
        assert!(ProxyManager::new(path.clone()).restore().is_err());
        assert!(path.exists());
    }

    #[test]
    fn snapshot_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let snap = ProxySnapshot {
            #[cfg(target_os = "macos")]
            macos_writes: Default::default(),
            backend: "GNOME (gsettings)".into(),
            entries: BTreeMap::from([("org.gnome.system.proxy/mode".into(), "'none'".into())]),
            applied_at: now_unix(),
            http_host: "127.0.0.1".into(),
            http_port: 7890,
            pac_url: None,
        };
        crate::util::atomic_write_json(&path, &snap).unwrap();
        let loaded: ProxySnapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded, snap);
    }

    #[test]
    fn backend_detection_is_total() {
        let backend = detect_backend();
        assert!(!backend.label().is_empty());
    }
}
