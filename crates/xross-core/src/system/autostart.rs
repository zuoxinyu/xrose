//! Login/startup integration for the three desktop platforms.

use std::path::{Path, PathBuf};

use crate::error::{io_context, Error, Result};

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
            .and_then(|key| key.get_value::<String, _>("Xross").ok())
            .is_some()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let path = linux_path()?;
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
            }
            let exe =
                std::env::current_exe().map_err(|e| io_context(Path::new("current_exe"), e))?;
            let desktop = format!(
                "[Desktop Entry]\nType=Application\nName=Xross\nComment=Xray / sing-box client\nExec=\"{}\" --minimized\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
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
            let exe =
                std::env::current_exe().map_err(|e| io_context(Path::new("current_exe"), e))?;
            let plist = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>io.xross.app</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>--minimized</string></array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
                exe.display()
            );
            std::fs::write(&path, plist).map_err(|e| io_context(&path, e))?;
            let _ = std::process::Command::new("launchctl")
                .args(["load", "-w"])
                .arg(&path)
                .status();
        } else {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&path)
                .status();
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
            let exe =
                std::env::current_exe().map_err(|e| io_context(Path::new("current_exe"), e))?;
            key.set_value("Xross", &format!("\"{}\" --minimized", exe.display()))
                .map_err(|e| Error::other(format!("写入注册表失败：{e}")))?;
        } else {
            let _ = key.delete_value("Xross");
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = enabled;
        Err(Error::other("当前平台不支持开机自启"))
    }
}

#[cfg(target_os = "linux")]
fn linux_path() -> Result<PathBuf> {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map_err(|_| Error::other("无法确定用户配置目录"))?;
    Ok(config.join("autostart/xross.desktop"))
}

#[cfg(target_os = "macos")]
fn macos_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| Error::other("无法确定用户目录"))?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/io.xross.app.plist"))
}
