//! GPUI Kit flyout, independent of the native tray backend and main window.

use gpui_base::Disableable;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::RoutePreset;
use xross_core::settings::{Language, ThemeMode};
use xross_core::util::format_speed;
use xross_core::AppService;

use super::dashboard::{self, Dashboard, TrayAction};
use super::PopupAnchor;
use crate::i18n::tr;
use crate::ui::theme::palette;

#[derive(Default)]
pub struct PopupManager {
    window: Option<AnyWindowHandle>,
    view: Option<Entity<PopupView>>,
    last_refresh: Option<Instant>,
}

impl PopupManager {
    pub fn dismiss(&mut self, cx: &mut App) {
        if let (Some(handle), Some(view)) = (self.window, self.view.as_ref()) {
            let _ = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, _| view.dismiss(window))
            });
        }
    }

    pub fn toggle(&mut self, service: &AppService, anchor: Option<PopupAnchor>, cx: &mut App) {
        // Wayland cannot report global pointer hit-testing. Suppress the same
        // tray click's Activate when it immediately follows a focus dismissal.
        if cfg!(target_os = "linux")
            && self.view.as_ref().is_some_and(|view| {
                view.read(cx)
                    .deactivated_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
            })
        {
            if let Some(view) = self.view.as_ref() {
                view.update(cx, |view, _| view.deactivated_at = None);
            }
            return;
        }
        if self.view.as_ref().is_some_and(|view| view.read(cx).visible) {
            self.dismiss(cx);
            return;
        }
        let dashboard = dashboard::build(service);
        let fallback = cx
            .primary_display()
            .map(|display| display.visible_bounds())
            .unwrap_or(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1280.), px(800.)),
            ));
        let work_area = anchor
            .and_then(|a| a.work_area)
            .or_else(|| {
                anchor.and_then(|a| {
                    cx.displays()
                        .into_iter()
                        .find(|display| display.bounds().contains(&a.position))
                        .map(|display| display.visible_bounds())
                })
            })
            .unwrap_or(fallback);
        let desired_height = 452. + (dashboard.nodes.len().min(4) as f32 * 38.).max(76.);
        let bounds = popup_bounds(
            anchor.map(|a| a.position),
            work_area,
            size(px(400.), px(desired_height)),
        );
        if let (Some(handle), Some(view)) = (self.window, self.view.as_ref()) {
            let reused = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.refresh_dashboard(dashboard.clone(), window, cx);
                    view.visible = true;
                    view.selected = view
                        .dashboard
                        .nodes
                        .iter()
                        .position(|node| node.active)
                        .unwrap_or(0);
                    view.scroll.scroll_to_item(view.selected);
                    super::backend::show_popup(window, bounds);
                    window.focus(&view.focus, cx);
                    cx.notify();
                });
            });
            if reused.is_ok() {
                return;
            }
        }
        let error_service = service.clone();
        let service = service.clone();
        match open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                focus: !cfg!(windows),
                show: !cfg!(windows),
                app_id: Some("io.xross.Xross.Tray".into()),
                window_background: crate::window_effects::window_background(),
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            },
            cx,
            move |window, cx| {
                window.set_window_title("Xross · Tray");
                crate::window_effects::apply(window, cx);
                cx.new(|cx| PopupView::new(service, dashboard, window, cx))
            },
        ) {
            Ok((handle, view)) => {
                #[cfg(windows)]
                let _ = handle.update(cx, |_, window, cx| {
                    crate::window_effects::apply(window, cx);
                    super::backend::show_popup(window, bounds);
                });
                self.window = Some(handle);
                self.view = Some(view);
                self.last_refresh = Some(Instant::now());
            }
            Err(err) => error_service.toast(
                xross_core::service::ToastLevel::Error,
                format!("无法打开托盘浮窗：{err}"),
            ),
        }
    }

    pub fn tick(&mut self, service: &AppService, cx: &mut App) {
        if self
            .last_refresh
            .is_some_and(|last| last.elapsed() < Duration::from_millis(400))
        {
            return;
        }
        let Some(view) = self.view.as_ref() else {
            return;
        };
        if !view.read(cx).visible {
            return;
        }
        if let Some(handle) = self.window {
            let _ = handle.update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.refresh_dashboard(dashboard::build(service), window, cx);
                    cx.notify();
                })
            });
        }
        self.last_refresh = Some(Instant::now());
    }
}

/// Clamp both axes to the monitor work area, including negative monitor origins.
fn popup_bounds(
    anchor: Option<Point<Pixels>>,
    work: Bounds<Pixels>,
    desired: Size<Pixels>,
) -> Bounds<Pixels> {
    let gap = px(8.);
    let width = desired.width.min((work.size.width - gap * 2.).max(px(1.)));
    let height = desired
        .height
        .min((work.size.height - gap * 2.).max(px(1.)));
    let at = anchor.unwrap_or(point(work.right() - gap, work.bottom() + gap));
    let below = at.y < work.center().y;
    let x = (at.x - width / 2.)
        .max(work.left() + gap)
        .min(work.right() - width - gap);
    let y = if below {
        at.y + gap
    } else {
        at.y - height - gap
    };
    let y = y.max(work.top() + gap).min(work.bottom() - height - gap);
    Bounds::new(point(x, y), size(width, height))
}

#[derive(Clone)]
struct RouteOption {
    preset: RoutePreset,
    label: SharedString,
}

impl SelectItem for RouteOption {
    type Value = RoutePreset;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &RoutePreset {
        &self.preset
    }
}

fn route_options(lang: Language) -> Vec<RouteOption> {
    [
        (
            RoutePreset::BypassMainland,
            "绕过中国大陆",
            "Bypass mainland",
        ),
        (RoutePreset::GfwList, "GFWList", "GFWList"),
        (RoutePreset::Global, "全局代理", "Global proxy"),
        (RoutePreset::ProxyApps, "仅代理指定应用", "Selected apps"),
        (RoutePreset::Custom, "自定义", "Custom"),
    ]
    .into_iter()
    .map(|(preset, zh, en)| RouteOption {
        preset,
        label: tr(lang, zh, en),
    })
    .collect()
}

fn mesh_phase_info(
    phase: xross_core::mesh::MeshPhase,
    p: &crate::ui::theme::Palette,
    lang: Language,
) -> (Hsla, SharedString) {
    match phase {
        xross_core::mesh::MeshPhase::Connected => (p.success, tr(lang, "已连接", "Connected")),
        xross_core::mesh::MeshPhase::Starting => (p.info, tr(lang, "连接中", "Connecting")),
        xross_core::mesh::MeshPhase::NeedsLogin => (p.warning, tr(lang, "待登录", "Login req.")),
        xross_core::mesh::MeshPhase::Stopping => (p.warning, tr(lang, "断开中", "Stopping")),
        xross_core::mesh::MeshPhase::Failed => (p.danger, tr(lang, "异常", "Failed")),
        xross_core::mesh::MeshPhase::Disconnected => {
            (p.muted_fg, tr(lang, "未连接", "Disconnected"))
        }
    }
}

pub struct PopupView {
    service: AppService,
    dashboard: Dashboard,
    focus: FocusHandle,
    selected: usize,
    visible: bool,
    deactivated_at: Option<Instant>,
    scroll: ScrollHandle,
    route_select: Entity<SelectState<Vec<RouteOption>>>,
    _subscriptions: Vec<Subscription>,
}

impl PopupView {
    fn new(
        service: AppService,
        dashboard: Dashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() && !super::backend::pointer_on_icon() {
                this.dismiss(window);
                this.deactivated_at = Some(Instant::now());
            }
        });
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = weak.update(cx, |view, _| view.dismiss(window));
            false
        });
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.service.settings().theme == ThemeMode::System {
                crate::window_effects::sync_system_appearance(window, cx);
                crate::window_effects::apply(window, cx);
            }
            cx.notify();
        });
        let route_select = cx.new(|cx| {
            let mut state = SelectState::new(route_options(dashboard.language), None, window, cx);
            state.set_selected_value(&dashboard.route_preset, window, cx);
            state
        });
        let route_selection =
            cx.subscribe_in(&route_select, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(preset)) = event {
                    this.action(TrayAction::SetRoutePreset(*preset), window, cx);
                }
            });
        let selected = dashboard
            .nodes
            .iter()
            .position(|node| node.active)
            .unwrap_or(0);
        Self {
            service,
            dashboard,
            focus,
            selected,
            visible: true,
            deactivated_at: None,
            scroll: ScrollHandle::new(),
            route_select,
            _subscriptions: vec![activation, appearance, route_selection],
        }
    }

    fn refresh_dashboard(
        &mut self,
        dashboard: Dashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let language_changed = dashboard.language != self.dashboard.language;
        let selection_changed =
            self.route_select.read(cx).selected_value() != Some(&dashboard.route_preset);
        if language_changed || selection_changed {
            self.route_select.update(cx, |state, cx| {
                if language_changed {
                    state.set_items(route_options(dashboard.language), window, cx);
                }
                state.set_selected_value(&dashboard.route_preset, window, cx);
            });
        }
        self.dashboard = dashboard;
    }

    fn dismiss(&mut self, window: &mut Window) {
        self.visible = false;
        self.deactivated_at = None;
        super::backend::hide_popup(window);
    }

    fn action(&mut self, action: TrayAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = dashboard::dispatch(action, &self.service) {
            super::queue_command(command);
            self.dismiss(window);
        } else {
            self.refresh_dashboard(dashboard::build(&self.service), window, cx);
            cx.notify();
        }
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group_id) = self.dashboard.group_id.clone() else {
            return;
        };
        let Some(node) = self.dashboard.nodes.get(index) else {
            return;
        };
        self.selected = index;
        window.focus(&self.focus, cx);
        self.action(
            TrayAction::SelectNode {
                group_id,
                node_id: node.id.clone(),
            },
            window,
            cx,
        );
    }
}

impl Render for PopupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let d = &self.dashboard;
        let lang = d.language;
        let running = d.running;
        let node_rows = d
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let delay = node
                    .delay_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "—".into());
                let delay_color = crate::ui::theme::latency_color_ms(&p, node.delay_ms);
                div()
                    .id(("tray-node", index))
                    .border_1()
                    .border_color(if index == self.selected {
                        p.primary.opacity(0.5)
                    } else {
                        gpui_kit::hsla(0., 0., 0., 0.)
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(38.))
                    .px_2()
                    .rounded_md()
                    .bg(if node.active {
                        p.accent.opacity(0.65)
                    } else {
                        gpui_kit::hsla(0., 0., 0., 0.)
                    })
                    .hover(|s| s.bg(p.accent))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select(index, window, cx)),
                    )
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .text_color(p.primary)
                            .child(if node.active { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_xs()
                            .child(node.label.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(delay_color)
                            .child(delay),
                    )
            })
            .collect::<Vec<_>>();
        let translucent = crate::window_effects::blur_backdrop();
        let surface = if crate::window_effects::uses_mica() {
            gpui_kit::hsla(0., 0., 0., 0.)
        } else if translucent {
            p.panel.opacity(if p.is_dark { 0.30 } else { 0.25 })
        } else {
            p.panel
        };
        let card = if translucent {
            p.bg.opacity(if p.is_dark { 0.18 } else { 0.35 })
        } else {
            p.bg
        };
        div()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .bg(surface)
            .rounded_xl()
            .border_1()
            .border_color(p.border)
            .text_color(p.fg)
            .text_xs()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss(window),
                    "down" if this.focus.is_focused(window) => {
                        this.selected =
                            (this.selected + 1).min(this.dashboard.nodes.len().saturating_sub(1));
                        this.scroll.scroll_to_item(this.selected);
                        cx.notify();
                    }
                    "up" if this.focus.is_focused(window) => {
                        this.selected = this.selected.saturating_sub(1);
                        this.scroll.scroll_to_item(this.selected);
                        cx.notify();
                    }
                    "enter" if this.focus.is_focused(window) => {
                        this.select(this.selected, window, cx)
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(img("app-icon-64.png").size(px(30.)).flex_shrink_0())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_base()
                            .child("Xross"),
                    )
                    .child(
                        Button::new("tray-test")
                            .compact()
                            .ghost()
                            .icon(IconName::Activity)
                            .tooltip(tr(lang, "全部节点测速", "Test all nodes"))
                            .disabled(d.busy_latency)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::TestAll, window, cx)
                            })),
                    )
                    .child(
                        Button::new("tray-settings")
                            .compact()
                            .ghost()
                            .icon(IconName::Settings)
                            .tooltip(tr(lang, "设置", "Settings"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ShowSettings, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .bg(card)
                    .child(div().flex().gap_3().children([
                        {
                            let status_color = if running { p.success } else { p.muted_fg };
                            let status_text = tr(
                                lang,
                                if running { "运行中" } else { "已停止" },
                                if running { "Running" } else { "Stopped" },
                            );
                            let proxy_addr = d.proxy_address.clone();
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::Server)
                                                .size(px(12.))
                                                .text_color(p.muted_fg),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(11.))
                                                .text_color(p.muted_fg)
                                                .child(d.core_name.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .size(px(6.))
                                                .rounded_full()
                                                .flex_shrink_0()
                                                .bg(status_color),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_xs()
                                                .text_color(p.fg)
                                                .child(status_text),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(10.5))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(if running {
                                            p.muted_fg
                                        } else {
                                            p.muted_fg.opacity(0.5)
                                        })
                                        .child(proxy_addr),
                                )
                        },
                        {
                            let (et_color, et_text) = mesh_phase_info(d.easytier.phase, &p, lang);
                            let et_ip = d.easytier.ip.clone().unwrap_or_else(|| "-".to_string());
                            let has_ip = d.easytier.ip.is_some();
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::Network)
                                                .size(px(12.))
                                                .text_color(p.muted_fg),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(11.))
                                                .text_color(p.muted_fg)
                                                .child("EasyTier"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .size(px(6.))
                                                .rounded_full()
                                                .flex_shrink_0()
                                                .bg(et_color),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_xs()
                                                .text_color(p.fg)
                                                .child(et_text),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(10.5))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(if has_ip {
                                            p.muted_fg
                                        } else {
                                            p.muted_fg.opacity(0.5)
                                        })
                                        .child(et_ip),
                                )
                        },
                        {
                            let (ts_color, ts_text) = mesh_phase_info(d.tailscale.phase, &p, lang);
                            let ts_ip = d.tailscale.ip.clone().unwrap_or_else(|| "-".to_string());
                            let has_ip = d.tailscale.ip.is_some();
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::Globe)
                                                .size(px(12.))
                                                .text_color(p.muted_fg),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(11.))
                                                .text_color(p.muted_fg)
                                                .child("Tailscale"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .size(px(6.))
                                                .rounded_full()
                                                .flex_shrink_0()
                                                .bg(ts_color),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_xs()
                                                .text_color(p.fg)
                                                .child(ts_text),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(10.5))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(if has_ip {
                                            p.muted_fg
                                        } else {
                                            p.muted_fg.opacity(0.5)
                                        })
                                        .child(ts_ip),
                                )
                        },
                    ]))
                    .child(
                        div().flex().gap_3().children(
                            [
                                (
                                    tr(lang, "↓ 下载", "↓ Download"),
                                    format_speed(d.down_speed),
                                    p.data_down,
                                ),
                                (
                                    tr(lang, "↑ 上传", "↑ Upload"),
                                    format_speed(d.up_speed),
                                    p.data_up,
                                ),
                                (tr(lang, "累计流量", "Total traffic"), d.total.clone(), p.fg),
                            ]
                            .into_iter()
                            .map(|(label, value, color)| {
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(p.muted_fg)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_xs()
                                            .text_color(color)
                                            .child(value),
                                    )
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .bg(card)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!(
                                        "{} · {}",
                                        tr(lang, "出站节点", "Outbound nodes"),
                                        d.group_name
                                    )),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(11.))
                                    .text_color(p.muted_fg)
                                    .child(format!("{}", d.nodes.len())),
                            ),
                    )
                    .child(
                        div()
                            .id("tray-node-scroll")
                            .track_scroll(&self.scroll)
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .children(node_rows)
                            .when(d.nodes.is_empty(), |s| {
                                s.child(div().py_4().text_xs().text_color(p.muted_fg).child(tr(
                                    lang,
                                    "暂无节点，请在主窗口添加",
                                    "Add nodes in the main window",
                                )))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(div().flex_1().min_w(px(0.)).truncate().text_xs().child(tr(
                        lang,
                        "路由模式",
                        "Routing mode",
                    )))
                    .child(
                        div()
                            .w(px(match lang {
                                Language::Zh => 120.,
                                Language::En => 136.,
                            }))
                            .h(px(28.))
                            .flex_shrink_0()
                            .child(
                                Select::new(&self.route_select)
                                    .xsmall()
                                    .w_full()
                                    .h_full()
                                    .menu_width(px(160.))
                                    .menu_max_h(px(190.))
                                    .disabled(d.busy_core)
                                    .accessibility_label(tr(lang, "路由模式", "Routing mode")),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(div().flex_1().min_w(px(0.)).text_xs().child(tr(
                        lang,
                        "系统代理",
                        "System proxy",
                    )))
                    .child(Switch::new("tray-proxy").checked(d.system_proxy).on_change(
                        cx.listener(|this, _, window, cx| {
                            this.action(TrayAction::ToggleSystemProxy, window, cx)
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .flex_shrink_0()
                    .px_1()
                    .child(div().flex_1().min_w(px(0.)).text_xs().child("TUN"))
                    .when(!d.tun_supported, |row| {
                        row.child(div().text_size(px(11.)).text_color(p.muted_fg).child(tr(
                            lang,
                            "内核不支持",
                            "Unsupported core",
                        )))
                    })
                    .child(
                        Switch::new("tray-tun")
                            .checked(d.tun_enabled)
                            .disabled(!d.tun_supported || d.busy_core)
                            .on_change(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ToggleTun, window, cx)
                            })),
                    ),
            )
            .child(
                div().flex().flex_shrink_0().child(
                    Button::new("tray-core")
                        .xsmall()
                        .h(px(32.))
                        .w_full()
                        .primary()
                        .disabled(d.busy_core)
                        .label(tr(
                            lang,
                            if d.busy_core {
                                "处理中…"
                            } else if running {
                                "停止 Core"
                            } else {
                                "启动 Core"
                            },
                            if d.busy_core {
                                "Working…"
                            } else if running {
                                "Stop core"
                            } else {
                                "Start core"
                            },
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.action(TrayAction::ToggleCore, window, cx)
                        })),
                ),
            )
            .when_some(d.toast.clone(), |d, toast| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(p.muted_fg)
                        .line_clamp(2)
                        .child(toast),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_shrink_0()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        Button::new("tray-show")
                            .xsmall()
                            .h(px(28.))
                            .compact()
                            .ghost()
                            .label(tr(lang, "显示主窗口", "Show window"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::ShowWindow, window, cx)
                            })),
                    )
                    .child(
                        Button::new("tray-quit")
                            .xsmall()
                            .h(px(28.))
                            .compact()
                            .ghost()
                            .icon(IconName::LogOut)
                            .label(tr(lang, "退出", "Quit"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.action(TrayAction::Quit, window, cx)
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::popup_bounds;
    use gpui_kit::{point, px, size, Bounds};
    #[test]
    fn placement_stays_inside_work_area_at_every_edge() {
        let work = Bounds::new(point(px(-1600.), px(-200.)), size(px(1600.), px(860.)));
        for at in [
            work.origin,
            work.bottom_right(),
            point(px(-800.), px(-200.)),
            point(px(-800.), px(660.)),
        ] {
            let b = popup_bounds(Some(at), work, size(px(400.), px(600.)));
            assert!(b.left() >= work.left() && b.right() <= work.right());
            assert!(b.top() >= work.top() && b.bottom() <= work.bottom());
        }
    }
    #[test]
    fn small_screens_shrink_the_flyout() {
        let work = Bounds::new(point(px(0.), px(0.)), size(px(320.), px(480.)));
        let b = popup_bounds(None, work, size(px(400.), px(620.)));
        assert_eq!(b.size, size(px(304.), px(464.)));
    }
}
