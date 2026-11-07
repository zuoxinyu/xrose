//! Login/startup integration for the three desktop platforms.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result, io_context};

pub fn is_enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux_path().map(|p| p.exists()).unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        macos_path().map(|p| p.exists()).unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        use winreg::RegKey;
        let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .ok()
            .and_then(|key| key.get_value::<String, _>("Xrose").ok())
            .is_some()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| io_context(Path::new("current_exe"), e))?;
    let args: Vec<String> = std::env::args().collect();
    let home = args
        .windows(2)
        .find(|args| args[0] == "--home")
        .map(|args| PathBuf::from(&args[1]))
        .or_else(|| std::env::var_os("XROSE_HOME").map(PathBuf::from));
    let arguments = login_arguments(&exe, home.as_deref());
    #[cfg(target_os = "linux")]
    {
        let path = linux_path()?;
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
            }
            let desktop = format!(
                "[Desktop Entry]\nType=Application\nName=Xrose\nComment=Xray / sing-box client\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                arguments
                    .iter()
                    .map(|s| desktop_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            std::fs::write(&path, desktop).map_err(|e| io_context(&path, e))?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|e| io_context(&path, e))?;
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let path = macos_path()?;
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
            }
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>io.xrose.app</string>
  <key>ProgramArguments</key>
  <array>{}</array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
                arguments
                    .iter()
                    .map(|s| format!("<string>{}</string>", escape_xml(s)))
                    .collect::<Vec<_>>()
                    .join("")
            );
            std::fs::write(&path, plist).map_err(|e| io_context(&path, e))?;
            // Clear persistent disable overrides left by older registrations.
            // Do not load/bootout the job while the GUI is changing preferences.
            set_macos_job_enabled(true)?;
        } else {
            set_macos_job_enabled(false)?;
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| io_context(&path, e))?;
            }
        }
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        use winreg::RegKey;
        let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| Error::other(format!("打开注册表失败：{e}")))?;
        if enabled {
            key.set_value(
                "Xrose",
                &arguments
                    .iter()
                    .map(|s| windows_quote(s))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
            .map_err(|e| Error::other(format!("写入注册表失败：{e}")))?;
        } else {
            let _ = key.delete_value("Xrose");
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (enabled, arguments);
        Err(Error::other("当前平台不支持开机自启"))
    }
}

#[cfg(target_os = "linux")]
fn linux_path() -> Result<PathBuf> {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map_err(|_| Error::other("无法确定用户配置目录"))?;
    Ok(config.join("autostart/xrose.desktop"))
}

#[cfg(target_os = "macos")]
fn set_macos_job_enabled(enabled: bool) -> Result<()> {
    let uid = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()?;
    let uid = String::from_utf8_lossy(&uid.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| Error::other("无法确定登录用户，请重试"))?;
    let result = std::process::Command::new("/bin/launchctl")
        .arg(if enabled { "enable" } else { "disable" })
        .arg(format!("gui/{uid}/io.xrose.app"))
        .status()?;
    if !result.success() {
        return Err(Error::other("无法更新登录启动状态，请检查系统权限"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| Error::other("无法确定用户目录"))?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/io.xrose.app.plist"))
}

#[cfg(target_os = "macos")]
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn login_arguments(exe: &Path, home: Option<&Path>) -> Vec<String> {
    let mut args = vec![exe.to_string_lossy().into_owned(), "--autostart".into()];
    if let Some(home) = home {
        // Keep portable profiles independent of the login process working directory.
        let home = if home.is_absolute() {
            home.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(home)
        };
        args.extend(["--home".into(), home.to_string_lossy().into_owned()]);
    }
    args
}

#[cfg(any(target_os = "linux", test))]
fn desktop_quote(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '%' => escaped.push_str("%%"),
            '\\' => escaped.push_str("\\\\\\\\"),
            '"' | '$' | '`' => {
                escaped.push_str("\\\\");
                escaped.push(ch);
            }
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

#[cfg(any(windows, test))]
fn windows_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(slashes));
        }
        slashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_entry_keeps_portable_home_and_uses_login_mode() {
        let args = login_arguments(
            Path::new("/Apps/Xrose App"),
            Some(Path::new("/tmp/profile space")),
        );
        assert_eq!(
            args,
            [
                "/Apps/Xrose App",
                "--autostart",
                "--home",
                "/tmp/profile space"
            ]
        );
        assert_eq!(desktop_quote("/tmp/100% app"), "\"/tmp/100%% app\"");
        assert_eq!(
            windows_quote("C:\\path with space\\"),
            "\"C:\\path with space\\\\\""
        );
    }
}
