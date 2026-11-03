// The GUI also hosts the scheduled privilege helper; neither needs a console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app_icon;
mod brand_assets;
mod i18n;
mod tray;
mod ui;
mod window_effects;

use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::*;
use xross_core::service::ToastLevel;
use xross_core::settings::ThemeMode;
use xross_core::system::SingleInstance;
use xross_core::AppService;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Privilege-helper entry points must run before the database/GUI starts.
    match xross_core::system::privilege::helper_main(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            eprintln!("提权助手：{err}");
            std::process::exit(1);
        }
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let service = match AppService::new() {
        Ok(service) => service,
        Err(err) => {
            eprintln!("Xross 启动失败：{err}");
            std::process::exit(1);
        }
    };

    // Single instance: the lock is held for the lifetime of the process.
    let _instance = if service.settings().single_instance {
        let path = service.paths().lock_file();
        match SingleInstance::acquire(&path) {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => {
                eprintln!("Xross 已在运行（锁文件：{}）", path.display());
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
    window_effects::configure(service.settings().backdrop);
    window_effects::set_follow_system(matches!(theme_mode, ThemeMode::System));
    let start_minimized =
        service.settings().start_minimized || args.iter().any(|arg| arg == "--minimized");
    let window_service = service.clone();
    let app_service = service.clone();
    let close_service = service.clone();

    app_icon::preinstall_executable_icon();
    application()
        .with_assets(brand_assets::BrandAssets)
        .run(move |cx| {
            init(cx);
            app_icon::install();
            match theme_mode {
                ThemeMode::Light => Theme::change(ComponentThemeMode::Light, None, cx),
                ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, None, cx),
                ThemeMode::System => {}
            }
            let mut tray = tray::TrayManager::new(&app_service);
            // Development preview for visual regression checks with an isolated profile.
            if cfg!(debug_assertions) && std::env::var_os("XROSS_TRAY_PREVIEW").is_some() {
                tray::queue_command(tray::TrayCommand::TogglePopup);
            }
            let hide_on_start = start_minimized;
            window_effects::make_surface_translucent(cx);
            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            let window_icon = image::load_from_memory(include_bytes!("../assets/app-icon-256.png"))
                .ok()
                .map(|image| Arc::new(image.to_rgba8()));
            let result = open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(960.), px(640.))),
                    app_id: Some("io.xross.Xross".into()),
                    icon: window_icon,
                    titlebar: Some(TitlebarOptions {
                        title: Some("Xross".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(16.), px(16.))),
                    }),
                    window_background: window_effects::window_background(),
                    app_owns_titlebar_drag: true,
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    if matches!(theme_mode, ThemeMode::System) {
                        window_effects::sync_system_appearance(window, cx);
                    }
                    window_effects::make_surface_translucent(cx);
                    tray::register_main_window(window);
                    window_effects::apply(window, cx);
                    let appearance_service = window_service.clone();
                    window
                        .observe_window_appearance(move |window, cx| {
                            if appearance_service.settings().theme == ThemeMode::System {
                                window_effects::sync_system_appearance(window, cx);
                                window_effects::apply(window, cx);
                            }
                        })
                        .detach();
                    let close_service = close_service.clone();
                    window.on_window_should_close(cx, move |window, cx| {
                        if tray::quitting() {
                            return true;
                        }
                        if close_service.settings().close_to_tray && tray::available() {
                            #[cfg(target_os = "linux")]
                            window.minimize_window();
                            #[cfg(not(target_os = "linux"))]
                            let _ = window;
                            tray::hide_main_window();
                            close_service.toast(
                                ToastLevel::Info,
                                "已最小化到托盘：单击图标打开浮窗，可显示主窗口或退出",
                            );
                            false
                        } else {
                            tray::request_quit();
                            cx.quit();
                            true
                        }
                    });
                    let service = window_service.clone();
                    cx.new(|cx| ui::AppView::new(service, window, cx))
                },
            );
            tray::setup_platform_window();
            let service_for_loop = app_service.clone();

            match result {
                Ok((main_window, view)) => {
                    let view = view.downgrade();
                    cx.spawn(async move |cx| {
                        let mut macos_configured = false;
                        let mut hide_on_start = hide_on_start;
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
                            if hide_on_start && tray::available() {
                                hide_on_start = false;
                                tray::hide_main_window();
                                #[cfg(target_os = "linux")]
                                let _ =
                                    main_window.update(cx, |_, window, _| window.minimize_window());
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
                                        if command == tray::TrayCommand::ShowSettings {
                                            let _ = view.update(cx, |view, cx| {
                                                view.page = ui::Page::Settings;
                                                cx.notify();
                                            });
                                        }
                                        tray::show_main_window();
                                        let _ = main_window
                                            .update(cx, |_, window, _| window.activate_window());
                                        cx.update(|cx| cx.activate(true));
                                    }
                                    tray::TrayCommand::TogglePopup => {}
                                    tray::TrayCommand::Quit => {
                                        tray::request_quit();
                                        service_for_loop.shutdown();
                                        cx.update(|cx| cx.quit());
                                        break;
                                    }
                                }
                            }
                            tray::watch_minimize(&service_for_loop);
                            if view.update(cx, |view, cx| view.tick(cx)).is_err() {
                                break;
                            }
                        }
                    })
                    .detach();
                }
                Err(err) => eprintln!("无法打开窗口：{err}"),
            }
            cx.activate(true);
        });

    service.shutdown();
}
