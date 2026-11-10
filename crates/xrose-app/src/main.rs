// The GUI also hosts the scheduled privilege helper; neither needs a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app_icon;
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
    let service = match AppService::with_paths(paths) {
        Ok(service) => service,
        Err(err) => {
            eprintln!("Xrose 启动失败：{err}");
            std::process::exit(1);
        }
    };

    // Single instance: the lock is held for the lifetime of the process.
    let _instance = if service.settings().single_instance {
        let path = service.paths().lock_file();
        match SingleInstance::acquire(&path) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                eprintln!("Xrose 已在运行（锁文件：{}）", path.display());
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
                    let command = cx.update(|cx| tray.tick(&service_for_loop, cx));
                    if let Some(command) = command {
                        match command {
                            tray::TrayCommand::ShowWindow
                            | tray::TrayCommand::ShowSettings => {
                                cx.update(|cx| {
                                    main.show(command == tray::TrayCommand::ShowSettings, cx);
                                    #[cfg(target_os = "linux")]
                                    tray.dismiss_popup(cx);
                                });
                            }
                            #[cfg(target_os = "linux")]
                            tray::TrayCommand::CloseMainWindow => {
                                cx.update(|cx| main.close(cx));
                            }
                            #[cfg(target_os = "linux")]
                            tray::TrayCommand::TrayUnavailable => {
                                cx.update(|cx| main.restore_if_hidden(cx));
                            }
                            tray::TrayCommand::TogglePopup => {}
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
