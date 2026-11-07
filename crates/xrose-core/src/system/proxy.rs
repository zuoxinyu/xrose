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
use crate::util::{atomic_write_json, now_unix};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxySnapshot {
    pub backend: String,
    /// Raw backend values captured before applying (gsettings/kreadconfig/registry).
    pub entries: BTreeMap<String, String>,
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
fn macos_network_services() -> Result<Vec<String>> {
    let output = run("networksetup", &["-listallnetworkservices"])?;
    Ok(output
        .lines()
        .skip(1)
        .map(|line| line.trim().trim_start_matches('*').trim().to_string())
        .filter(|line| !line.is_empty() && !line.contains("denotes"))
        .collect())
}

#[cfg(target_os = "macos")]
fn macos_capture() -> Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for service in macos_network_services()? {
        for (key, args) in [
            ("webproxy", vec!["-getwebproxy"]),
            ("securewebproxy", vec!["-getsecurewebproxy"]),
            ("socksproxy", vec!["-getsocksfirewallproxy"]),
            ("autoproxy", vec!["-getautoproxyurl"]),
        ] {
            let mut args = args;
            args.push(&service);
            if let Ok(value) = run("networksetup", &args) {
                entries.insert(format!("{service}/{key}"), value);
            }
        }
    }
    Ok(entries)
}

#[cfg(target_os = "macos")]
fn macos_apply(host: &str, port: u16, pac_url: Option<&str>) -> Result<()> {
    let services = macos_network_services()?;
    let port = port.to_string();
    for service in services {
        match pac_url {
            Some(url) => {
                run("networksetup", &["-setautoproxyurl", &service, url])?;
                run("networksetup", &["-setautoproxystate", &service, "on"])?;
                let _ = run("networksetup", &["-setwebproxystate", &service, "off"]);
                let _ = run(
                    "networksetup",
                    &["-setsecurewebproxystate", &service, "off"],
                );
                let _ = run(
                    "networksetup",
                    &["-setsocksfirewallproxystate", &service, "off"],
                );
            }
            None => {
                // A previous PAC selection must no longer bypass the local monitor.
                run("networksetup", &["-setautoproxystate", &service, "off"])?;
                run("networksetup", &["-setwebproxy", &service, host, &port])?;
                run(
                    "networksetup",
                    &["-setsecurewebproxy", &service, host, &port],
                )?;
                run(
                    "networksetup",
                    &["-setsocksfirewallproxy", &service, host, &port],
                )?;
                let _ = run("networksetup", &["-setwebproxystate", &service, "on"]);
                let _ = run("networksetup", &["-setsecurewebproxystate", &service, "on"]);
                let _ = run(
                    "networksetup",
                    &["-setsocksfirewallproxystate", &service, "on"],
                );
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_restore(entries: &BTreeMap<String, String>) -> Result<()> {
    for (key, raw) in entries {
        let Some((service, kind)) = key.split_once('/') else {
            continue;
        };
        let host = raw
            .lines()
            .find_map(|l| l.strip_prefix("Server: ").map(|s| s.trim().to_string()));
        let port = raw
            .lines()
            .find_map(|l| l.strip_prefix("Port: ").map(|s| s.trim().to_string()));
        let enabled = raw
            .lines()
            .find_map(|l| l.strip_prefix("Enabled: ").map(|s| s.trim() == "Yes"))
            .unwrap_or(false);
        match kind {
            "webproxy" | "securewebproxy" | "socksproxy" => {
                let state_arg = match kind {
                    "webproxy" => "-setwebproxystate",
                    "securewebproxy" => "-setsecurewebproxystate",
                    _ => "-setsocksfirewallproxystate",
                };
                let _ = run(
                    "networksetup",
                    &[state_arg, service, if enabled { "on" } else { "off" }],
                );
                if let (Some(host), Some(port)) = (&host, &port) {
                    let set_arg = match kind {
                        "webproxy" => "-setwebproxy",
                        "securewebproxy" => "-setsecurewebproxy",
                        _ => "-setsocksfirewallproxy",
                    };
                    let _ = run("networksetup", &[set_arg, service, host, port]);
                }
            }
            "autoproxy" => {
                let url = raw
                    .lines()
                    .find_map(|l| l.strip_prefix("URL: ").map(|s| s.trim().to_string()));
                if let Some(url) = url {
                    let _ = run("networksetup", &["-setautoproxyurl", service, &url]);
                }
                let _ = run(
                    "networksetup",
                    &[
                        "-setautoproxystate",
                        service,
                        if enabled { "on" } else { "off" },
                    ],
                );
            }
            _ => {}
        }
    }
    Ok(())
}

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
                    return macos_capture();
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
        let entries = if let Some(existing) = self.load_snapshot() {
            existing.entries
        } else {
            let entries = self.capture()?;
            let snapshot = ProxySnapshot {
                backend: self.backend.label().into(),
                entries: entries.clone(),
                applied_at: now_unix(),
                http_host: host.into(),
                http_port: port,
                pac_url: pac_url.map(str::to_string),
            };
            atomic_write_json(&self.snapshot_path, &snapshot)?;
            entries
        };
        let _ = entries;
        match self.backend {
            ProxyBackend::Gnome => {
                #[cfg(target_os = "linux")]
                gnome_apply(host, port, pac_url)?;
            }
            ProxyBackend::Kde => {
                #[cfg(target_os = "linux")]
                kde_apply(host, port, pac_url)?;
            }
            ProxyBackend::MacOs => {
                #[cfg(target_os = "macos")]
                macos_apply(host, port, pac_url)?;
            }
            ProxyBackend::Windows => {
                #[cfg(target_os = "windows")]
                windows_impl::apply(host, port, pac_url)?;
            }
            ProxyBackend::None => {
                return Err(Error::other("未检测到支持的桌面环境"));
            }
        }
        self.load_snapshot()
            .ok_or_else(|| Error::other("保存系统代理快照失败"))
    }

    /// Restores the snapshot and removes it.
    pub fn restore(&self) -> Result<()> {
        let Some(snapshot) = self.load_snapshot() else {
            return Ok(());
        };
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
                macos_restore(&snapshot.entries)?;
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
        let _ = self.restore();
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
    fn snapshot_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let snap = ProxySnapshot {
            backend: "GNOME (gsettings)".into(),
            entries: BTreeMap::from([("org.gnome.system.proxy/mode".into(), "'none'".into())]),
            applied_at: now_unix(),
            http_host: "127.0.0.1".into(),
            http_port: 7890,
            pac_url: None,
        };
        atomic_write_json(&path, &snap).unwrap();
        let loaded: ProxySnapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded, snap);
    }

    #[test]
    fn backend_detection_is_total() {
        let backend = detect_backend();
        assert!(!backend.label().is_empty());
    }
}
