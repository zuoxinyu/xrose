//! Main window lifetime. The tray and the service outlive a withdrawn KDE window.
use crate::{APP_ID, tray, ui, window_effects};
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use std::sync::Arc;
use xrose_core::{AppService, settings::ThemeMode};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShowTarget {
    Current,
    Settings,
    #[cfg(target_os = "macos")]
    About,
    #[cfg(target_os = "macos")]
    Updates,
}

pub struct MainWindow {
    service: AppService,
    visible: Option<(AnyWindowHandle, WeakEntity<ui::AppView>)>,
    bounds: WindowBounds,
    display: Option<DisplayId>,
    icon: Option<Arc<image::RgbaImage>>,
    #[cfg(target_os = "linux")]
    withdrawn: Option<ui::TrayWindowState>,
    #[cfg(target_os = "linux")]
    kde: bool,
}

impl MainWindow {
    pub fn new(service: &AppService, cx: &mut App) -> Result<Self> {
        let mut main = Self {
            service: service.clone(),
            visible: None,
            bounds: WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(800.)), cx)),
            display: None,
            icon: image::load_from_memory(include_bytes!("../assets/app-icon-256.png"))
                .ok()
                .map(|image| Arc::new(image.to_rgba8())),
            #[cfg(target_os = "linux")]
            withdrawn: None,
            #[cfg(target_os = "linux")]
            kde: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .any(|de| de.eq_ignore_ascii_case("KDE")),
        };
        main.open(cx)?;
        Ok(main)
    }

    fn open(&mut self, cx: &mut App) -> Result<()> {
        let service = &self.service;
        let options = WindowOptions {
            window_bounds: Some(self.bounds),
            display_id: self
                .display
                .filter(|id| cx.displays().iter().any(|d| d.id() == *id)),
            window_min_size: Some(size(px(960.), px(640.))),
            app_id: Some(APP_ID.into()),
            icon: self.icon.clone(),
            titlebar: Some(TitlebarOptions {
                title: Some("Xrose".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(16.))),
            }),
            window_background: window_effects::window_background(),
            #[cfg(target_os = "linux")]
            window_decorations: Some(WindowDecorations::Client),
            app_owns_titlebar_drag: true,
            ..Default::default()
        };
        let (handle, view) = open_window(options, cx, |window, cx| {
            if service.settings().theme == ThemeMode::System {
                window_effects::sync_system_appearance(window, cx);
            }
            window_effects::make_surface_translucent(cx);
            tray::register_main_window(window);
            window_effects::apply(window, cx);
            let appearance_service = service.clone();
            window
                .observe_window_appearance(move |window, cx| {
                    if appearance_service.settings().theme == ThemeMode::System {
                        window_effects::sync_system_appearance(window, cx);
                        window_effects::apply(window, cx);
                    }
                })
                .detach();
            #[cfg(not(target_os = "linux"))]
            let close_service = service.clone();
            window.on_window_should_close(cx, move |_window, _| {
                if tray::quitting() {
                    return true;
                }
                #[cfg(target_os = "linux")]
                tray::queue_command(tray::TrayCommand::CloseMainWindow);
                #[cfg(not(target_os = "linux"))]
                if close_service.settings().close_to_tray && tray::available() {
                    tray::hide_main_window();
                } else {
                    tray::queue_command(tray::TrayCommand::Quit);
                }
                false
            });
            cx.new(|cx| {
                let view = ui::AppView::new(service.clone(), window, cx);
                #[cfg(target_os = "linux")]
                let view = if let Some(state) = self.withdrawn.take() {
                    view.restore_from_tray(state, window, cx)
                } else {
                    view
                };
                view
            })
        })?;
        self.visible = Some((handle, view.downgrade()));
        Ok(())
    }

    pub fn minimize(&self, cx: &mut App) {
        if let Some((handle, _)) = &self.visible {
            let _ = handle.update(cx, |_, win, _| win.minimize_window());
        }
    }

    #[cfg(target_os = "linux")]
    pub fn close(&mut self, cx: &mut App) {
        let Some((handle, view)) = &self.visible else {
            return;
        };
        if !self.service.settings().close_to_tray || !tray::available() {
            tray::queue_command(tray::TrayCommand::Quit);
        } else if self.kde {
            let result = handle.update(cx, |_, window, cx| {
                let session = view.update(cx, |view, cx| view.tray_state(cx))?;
                self.bounds = window.window_bounds();
                self.display = window.display(cx).map(|d| d.id());
                cx.set_quit_mode(QuitMode::Explicit);
                window.remove_window();
                Ok::<_, anyhow::Error>(session)
            });
            if let Ok(Ok(session)) = result {
                self.withdrawn = Some(session);
                self.visible = None;
            }
        } else {
            self.minimize(cx);
        }
    }

    #[cfg(target_os = "linux")]
    pub fn restore_if_hidden(&mut self, cx: &mut App) {
        if self.withdrawn.is_some() {
            self.show(ShowTarget::Current, cx);
        }
    }

    pub fn show(&mut self, target: ShowTarget, cx: &mut App) {
        if self.visible.is_none() {
            if let Err(err) = self.open(cx) {
                self.service.toast(
                    xrose_core::service::ToastLevel::Error,
                    format!("无法打开主窗口：{err}"),
                );
                return;
            }
            cx.set_quit_mode(QuitMode::Default);
        }
        if let Some((handle, view)) = &self.visible {
            tray::show_main_window();
            let _ = handle.update(cx, |_, window, cx| {
                if target != ShowTarget::Current {
                    if window.has_active_dialog(cx) {
                        window.close_all_dialogs(cx);
                    }
                    let _ = view.update(cx, |view, cx| {
                        match target {
                            ShowTarget::Current => {}
                            ShowTarget::Settings => view.navigate(ui::Page::Settings, cx),
                            #[cfg(target_os = "macos")]
                            ShowTarget::About => view.navigate(ui::Page::About, cx),
                            #[cfg(target_os = "macos")]
                            ShowTarget::Updates => view.open_updates_settings(cx),
                        }
                        view.focus_handle.focus(window, cx);
                    });
                }
                window.activate_window();
            });
            cx.activate(true);
        }
    }

    pub fn tick(&self, cx: &mut App) -> bool {
        self.visible
            .as_ref()
            .is_none_or(|(_, view)| view.update(cx, |view, cx| view.tick(cx)).is_ok())
    }
}
