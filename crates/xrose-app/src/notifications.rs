//! Native notification authorization and delivery, independent of UI polling.

use std::time::{Duration, Instant};
use xrose_core::AppService;
use xrose_core::service::{NotificationEvent, ToastLevel};

pub fn install(service: &AppService) {
    let receiver = service.subscribe_notifications();
    let reporter = service.clone();
    let service = service.clone();
    let result = std::thread::Builder::new()
        .name("xrose-notifications".into())
        .spawn(move || {
            let mut recent = Vec::<(String, Instant)>::new();
            let mut reported_failure = false;
            let mut authorized = false;
            while let Ok(event) = receiver.recv() {
                let toast = match event {
                    NotificationEvent::Configure {
                        enabled,
                        user_initiated,
                    } => {
                        recent.clear();
                        reported_failure = false;
                        authorized = false;
                        if enabled && service.settings().system_notifications {
                            match request_permission() {
                                Ok(()) => {
                                    authorized = true;
                                    service.logs().push(
                                        xrose_core::core::LogStream::App,
                                        xrose_core::settings::LogLevel::Info,
                                        "系统通知权限已确认",
                                    );
                                    if user_initiated && service.settings().system_notifications {
                                        service.toast(
                                            ToastLevel::Success,
                                            crate::i18n::tr(
                                                service.settings().language,
                                                "系统通知已开启",
                                                "System notifications enabled",
                                            )
                                            .to_string(),
                                        );
                                    }
                                }
                                Err(error) if service.settings().system_notifications => {
                                    service.report_notification_error(&error);
                                    reported_failure = true;
                                }
                                Err(_) => {}
                            }
                        }
                        continue;
                    }
                    NotificationEvent::Show(toast) => toast,
                };
                // Resolve the preference after queuing, including while authorization was pending.
                if !authorized || !service.settings().system_notifications {
                    continue;
                }
                recent.retain(|(_, at)| at.elapsed() < Duration::from_secs(30));
                if recent.iter().any(|(message, _)| message == &toast.message) {
                    continue;
                }
                match show(toast.id, &toast.message) {
                    Ok(()) => {
                        recent.push((toast.message, Instant::now()));
                        reported_failure = false;
                    }
                    Err(error) if !reported_failure => {
                        service.report_notification_error(&error);
                        reported_failure = true;
                    }
                    Err(_) => {}
                }
            }
        });
    if let Err(error) = result {
        reporter.report_notification_error(&error.to_string());
    }
}

#[cfg(not(target_os = "macos"))]
fn request_permission() -> Result<(), String> {
    #[cfg(windows)]
    register_windows_app()?;
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
fn show(_id: u64, message: &str) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification.summary("Xrose").body(message);
    #[cfg(target_os = "linux")]
    notification.appname("Xrose").icon("xrose");
    #[cfg(windows)]
    {
        register_windows_app()?;
        notification.app_id("io.xrose.Xrose");
    }
    // notify-rust's Windows backend uses WinRT ToastNotificationManager.
    notification.show().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn register_windows_app() -> Result<(), String> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(r"Software\Classes\AppUserModelId\io.xrose.Xrose")
        .map_err(|e| e.to_string())?;
    key.set_value("DisplayName", &"Xrose")
        .map_err(|e| e.to_string())?;
    key.set_value("ShowInSettings", &1u32)
        .map_err(|e| e.to_string())?;
    let id: Vec<u16> = "io.xrose.Xrose\0".encode_utf16().collect();
    let result = unsafe {
        windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr())
    };
    if result < 0 {
        return Err(format!("设置通知标识失败：{result}"));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::{request_permission, show};

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn show(_id: u64, _message: &str) -> Result<(), String> {
    Err("当前平台不支持系统通知".into())
}
