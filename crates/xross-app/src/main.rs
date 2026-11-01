mod brand_assets;
mod i18n;
mod ui;

use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeMode as ComponentThemeMode};
use gpui_kit::*;
use xross_core::settings::ThemeMode;
use xross_core::system::SingleInstance;
use xross_core::AppService;

fn main() {
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
    let window_service = service.clone();

    application()
        .with_assets(brand_assets::BrandAssets)
        .run(move |cx| {
            init(cx);
            match theme_mode {
                ThemeMode::Light => Theme::change(ComponentThemeMode::Light, None, cx),
                ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, None, cx),
                ThemeMode::System => {}
            }
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
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    if matches!(theme_mode, ThemeMode::System) {
                        Theme::sync_system_appearance(Some(window), cx);
                    }
                    let service = window_service.clone();
                    cx.new(|cx| ui::AppView::new(service, window, cx))
                },
            );
            if let Err(err) = result {
                eprintln!("无法打开窗口：{err}");
            }
            cx.activate(true);
        });

    service.shutdown();
}
