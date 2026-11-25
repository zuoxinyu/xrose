// The GUI also hosts the scheduled privilege helper; neither needs a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app_icon;
mod app_menu;
mod brand_assets;
mod i18n;
mod main_window;
mod notifications;
mod tray;
mod ui;
#[cfg(target_os = "linux")]
mod wayland;
mod window_effects;

use gpui_kit::component::{Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::*;
use xrose_core::AppService;
use xrose_core::settings::ThemeMode;
use xrose_core::system::SingleInstance;

const APP_ID: &str = "io.xrose.Xrose";

fn main() {
    let args = std::env::args_os().skip(1);
    let args: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
    // Privilege-helper entry points must run before the database/GUI starts.
    match xrose_core::system::privilege::helper_main(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            eprintln!("提权助手：{err}");
            std::process::exit(1);
        }
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let paths = args
        .windows(2)
        .find(|args| args[0] == "--home")
        .map(|args| xrose_core::paths::AppPaths::from_root(std::path::Path::new(&args[1])))
        .unwrap_or_else(xrose_core::paths::AppPaths::discover);
    let prepare_only = args.iter().any(|arg| arg == "--prepare-resources");
    let single_instance_hint = xrose_core::Settings::load(&paths.settings_file())
        .map(|settings| settings.single_instance)
        .unwrap_or(true);
    // Wake a running GUI instance before touching on-disk state so a standard
    // launch can still surface an elevated instance running in a protected folder.
    if !prepare_only && single_instance_hint && wake_running_instance(&paths, false) {
        return;
    }
    if let Err(error) = xrose_core::bundled::prepare(&paths) {
        report_startup_error(
            &paths,
            StartupStage::PrepareResources,
            &error,
            !prepare_only,
        );
        std::process::exit(1);
    }
    if prepare_only {
        return;
    }
    let startup_paths = paths.clone();
    let service = match AppService::with_paths(paths) {
        Ok(service) => service,
        Err(err) => {
            if single_instance_hint && wake_running_instance(&startup_paths, false) {
                return;
            }
            report_startup_error(&startup_paths, StartupStage::StartApp, &err, true);
            std::process::exit(1);
        }
    };

    // Single instance: the lock is held for the lifetime of the process.
    let mut _instance = if service.settings().single_instance {
        let path = service.paths().lock_file();
        match SingleInstance::acquire(&path) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                if !wake_running_instance(service.paths(), true) {
                    eprintln!("Xrose 已在运行（锁文件：{}）", path.display());
                }
                return;
            }
            Err(err) => {
                eprintln!("无法创建单实例锁：{err}");
                None
            }
        }
    } else {
        None
    };
    if let Some(guard) = _instance.as_mut() {
        guard.on_wakeup(|| tray::queue_command(tray::TrayCommand::ShowWindow));
    }

    #[cfg(target_os = "macos")]
    let _proxy_observer = service.observe_system_proxy();
    let theme_mode = service.settings().theme;
    window_effects::set_follow_system(matches!(theme_mode, ThemeMode::System));
    let start_minimized =
        service.settings().start_minimized || args.iter().any(|arg| arg == "--minimized");
    let app_service = service.clone();

    app_icon::preinstall_executable_icon();
    application()
        .with_assets(brand_assets::BrandAssets)
        .run(move |cx| {
            init(cx);
            ui::expand_motion::init_system_reduce_motion(cx);
            ui::expand_motion::apply_motion_mode(app_service.settings().motion, cx);
            app_menu::install(&app_service, cx);
            #[cfg(target_os = "linux")]
            wayland::init(cx);
            window_effects::configure(app_service.settings().backdrop);
            // Native termination can bypass the tray command loop.
            let quit_service = app_service.clone();
            cx.on_app_quit(move |_| {
                quit_service.shutdown();
                std::future::ready(())
            })
            .detach();
            app_icon::install();
            match theme_mode {
                ThemeMode::Light => Theme::change(ComponentThemeMode::Light, None, cx),
                ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, None, cx),
                ThemeMode::System => {}
            }
            notifications::install(&app_service);
            let mut tray = tray::TrayManager::new(&app_service);
            app_service.run_startup_actions();
            // Development preview for visual regression checks with an isolated profile.
            if cfg!(debug_assertions) && std::env::var_os("XROSE_TRAY_PREVIEW").is_some() {
                tray::queue_command(tray::TrayCommand::TogglePopup);
            }
            window_effects::make_surface_translucent(cx);
            let mut main = match main_window::MainWindow::new(&app_service, cx) {
                Ok(main) => main,
                Err(err) => {
                    eprintln!("无法打开窗口：{err}");
                    cx.quit();
                    return;
                }
            };
            tray::setup_platform_window();
            let service_for_loop = app_service.clone();
            cx.spawn(async move |cx| {
                let mut macos_configured = false;
                if start_minimized {
                    cx.update(|cx| main.minimize(cx));
                }
                let mut backdrop = service_for_loop.settings().backdrop;
                let mut motion = service_for_loop.settings().motion;
                loop {
                    tokio::select! {
                        _ = tray::wait_for_command() => {}
                        _ = cx.background_executor().timer(std::time::Duration::from_millis(200)) => {}
                    }
                    if !macos_configured {
                        tray::setup_platform_window();
                        macos_configured = true;
                    }
                    let new_backdrop = service_for_loop.settings().backdrop;
                    if backdrop != new_backdrop {
                        backdrop = new_backdrop;
                        cx.update(|cx| window_effects::refresh_all(backdrop, cx));
                    }
                    let new_motion = service_for_loop.settings().motion;
                    if motion != new_motion {
                        motion = new_motion;
                        cx.update(|cx| ui::expand_motion::apply_motion_mode(motion, cx));
                    }
                    let language = service_for_loop.settings().language;
                    cx.update(|cx| app_menu::sync(language, cx));
                    let command = cx.update(|cx| tray.tick(&service_for_loop, cx));
                    if let Some(command) = command {
                        if let Some(target) = show_target(command) {
                            cx.update(|cx| {
                                main.show(target, cx);
                                #[cfg(target_os = "linux")]
                                tray.dismiss_popup(cx);
                            });
                        } else {
                            match command {
                                #[cfg(target_os = "linux")]
                                tray::TrayCommand::CloseMainWindow => {
                                    cx.update(|cx| main.close(cx));
                                }
                                #[cfg(target_os = "linux")]
                                tray::TrayCommand::TrayUnavailable => {
                                    cx.update(|cx| main.restore_if_hidden(cx));
                                }
                                tray::TrayCommand::ShowWindow
                                | tray::TrayCommand::ShowFailedConnections
                                | tray::TrayCommand::TogglePopup => {}
                                #[cfg(target_os = "macos")]
                                tray::TrayCommand::ShowAbout | tray::TrayCommand::ShowUpdates => {}
                                tray::TrayCommand::Quit => {
                                    // Keep the event loop alive to finish quitting after cleanup.
                                    #[cfg(target_os = "linux")]
                                    cx.update(|cx| main.minimize(cx));
                                    tray::hide_main_window();
                                    cx.background_executor()
                                        .spawn(async move { service_for_loop.shutdown() })
                                        .await;
                                    tray::request_quit();
                                    cx.update(|cx| cx.quit());
                                    break;
                                }
                            }
                        }
                    }
                    if !cx.update(|cx| main.tick(cx)) {
                        break;
                    }
                }
            })
            .detach();
            cx.activate(true);
        });

    service.shutdown();
}

fn show_target(command: tray::TrayCommand) -> Option<main_window::ShowTarget> {
    match command {
        tray::TrayCommand::ShowWindow => Some(main_window::ShowTarget::Current),
        tray::TrayCommand::ShowFailedConnections => {
            Some(main_window::ShowTarget::FailedConnections)
        }
        #[cfg(target_os = "macos")]
        tray::TrayCommand::ShowAbout => Some(main_window::ShowTarget::About),
        #[cfg(target_os = "macos")]
        tray::TrayCommand::ShowUpdates => Some(main_window::ShowTarget::Updates),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum StartupStage {
    PrepareResources,
    StartApp,
}

fn wake_running_instance(paths: &xrose_core::paths::AppPaths, retry: bool) -> bool {
    let attempts = if retry { 6 } else { 1 };
    for attempt in 0..attempts {
        if tray::wake_existing(paths) {
            return true;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    false
}

fn format_startup_error(
    paths: &xrose_core::paths::AppPaths,
    stage: StartupStage,
    error: &xrose_core::Error,
    lang: xrose_core::settings::Language,
) -> String {
    let prefix = match stage {
        StartupStage::PrepareResources => crate::i18n::tr(
            lang,
            "初始化随附资源失败：",
            "Failed to initialize bundled resources: ",
        ),
        StartupStage::StartApp => {
            crate::i18n::tr(lang, "Xrose 启动失败：", "Failed to start Xrose: ")
        }
    };
    let portable_hint = if paths.is_portable_data() && error.is_permission_or_readonly() {
        crate::i18n::tr(
            lang,
            "\n\n当前程序目录无写入权限。便携版会在程序目录下读写 XroseData，请将整个目录移至普通用户可写的位置运行，不要放在 Program Files 等受保护目录中。",
            "\n\nThe application directory is not writable. Portable mode stores data in XroseData next to the executable; move Xrose to a user-writable folder instead of Program Files or another protected directory.",
        )
    } else {
        SharedString::default()
    };
    format!("{prefix}{error}{portable_hint}")
}

fn report_startup_error(
    paths: &xrose_core::paths::AppPaths,
    stage: StartupStage,
    error: &xrose_core::Error,
    interactive: bool,
) {
    let lang = xrose_core::Settings::load(&paths.settings_file())
        .map(|settings| settings.language)
        .unwrap_or_default();
    let message = format_startup_error(paths, stage, error, lang);
    eprintln!("{message}");
    #[cfg(windows)]
    if interactive {
        show_windows_error_dialog(&message);
    }
    #[cfg(not(windows))]
    let _ = interactive;
}

#[cfg(windows)]
fn show_windows_error_dialog(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = "Xrose\0".encode_utf16().collect();
    // SAFETY: text and caption are NUL-terminated UTF-16 buffers valid for the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_ICONERROR | MB_OK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{StartupStage, format_startup_error};
    use xrose_core::settings::Language;

    #[test]
    fn startup_error_adds_portable_directory_guidance_on_permission_failure() {
        let dir = std::env::temp_dir().join(format!("xrose-startup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("xrose.portable"), "full").unwrap();
        let portable = xrose_core::paths::AppPaths::from_root(&dir.join("XroseData"));
        let denied = xrose_core::error::io_context(
            &portable.data_dir,
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
        );

        let zh = format_startup_error(&portable, StartupStage::StartApp, &denied, Language::Zh);
        assert!(zh.starts_with("Xrose 启动失败："));
        assert!(zh.contains("Program Files"));

        let en = format_startup_error(
            &portable,
            StartupStage::PrepareResources,
            &denied,
            Language::En,
        );
        assert!(en.starts_with("Failed to initialize bundled resources: "));
        assert!(en.contains("XroseData"));

        let normal = xrose_core::paths::AppPaths::from_root(&dir.join("Standard"));
        let plain = format_startup_error(&normal, StartupStage::StartApp, &denied, Language::Zh);
        assert!(!plain.contains("Program Files"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
