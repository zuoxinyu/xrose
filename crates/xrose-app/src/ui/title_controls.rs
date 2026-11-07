//! Caption buttons and drag regions for the custom window chrome.
//!
//! The main window is opened without a system title bar
//! (`TitlebarOptions::appears_transparent`), so nothing is drawn by the OS. On
//! Windows the buttons only need to be painted: marking them with a
//! [`WindowControlArea`] lets the window procedure answer `WM_NCHITTEST` with
//! `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE`, and the shell performs the action
//! itself. Other platforms get an explicit click handler.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::theme::Palette;

/// Width of one caption button, matched to the Windows metrics (46px).
const CONTROL_WIDTH: Pixels = px(46.);

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize { maximized: bool },
    Close,
}

impl Control {
    fn id(self) -> &'static str {
        match self {
            Control::Minimize => "window-minimize",
            Control::Maximize { .. } => "window-maximize",
            Control::Close => "window-close",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Control::Minimize => IconName::WindowMinimize,
            Control::Maximize { maximized: false } => IconName::WindowMaximize,
            Control::Maximize { maximized: true } => IconName::WindowRestore,
            Control::Close => IconName::WindowClose,
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Control::Minimize => WindowControlArea::Min,
            Control::Maximize { .. } => WindowControlArea::Max,
            Control::Close => WindowControlArea::Close,
        }
    }
}

/// The minimize / maximize / close cluster. Renders nothing on macOS, where
/// the system draws its own traffic lights.
#[derive(IntoElement)]
pub struct WindowControls {
    palette: Palette,
}

impl WindowControls {
    pub fn new(palette: Palette) -> Self {
        Self { palette }
    }
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if cfg!(target_os = "macos") {
            return div().into_any_element();
        }

        let p = self.palette;
        let maximized = window.is_maximized();
        let controls = [
            Control::Minimize,
            Control::Maximize { maximized },
            Control::Close,
        ];

        div()
            .id("window-controls")
            .flex()
            .items_center()
            .h_full()
            .flex_shrink_0()
            .children(controls.into_iter().map(|control| {
                let is_close = matches!(control, Control::Close);
                let hover_bg = if is_close { p.danger } else { p.panel_alt };
                let hover_fg = if is_close {
                    gpui_kit::hsla(0., 0., 1., 1.)
                } else {
                    p.fg
                };
                let pressed_bg = if is_close { p.danger } else { p.accent };

                div()
                    .id(control.id())
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(CONTROL_WIDTH)
                    .h_full()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .text_color(p.muted_fg)
                    .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
                    .active(move |style| style.bg(pressed_bg).text_color(hover_fg))
                    .when(cfg!(target_os = "windows"), |this| {
                        // Leave the click to the window procedure, which turns the
                        // hit-test result into the matching system command.
                        this.window_control_area(control.area())
                    })
                    .when(!cfg!(target_os = "windows"), |this| {
                        this.on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            match control {
                                Control::Minimize => window.minimize_window(),
                                Control::Maximize { .. } => window.zoom_window(),
                                Control::Close => {
                                    crate::tray::queue_command(crate::tray::TrayCommand::Quit)
                                }
                            }
                        })
                    })
                    .child(Icon::new(control.icon()).size(px(14.)))
            }))
            .into_any_element()
    }
}
