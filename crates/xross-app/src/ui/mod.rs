mod connections;
mod dns;
mod logs;
mod nodes;
mod routing;
mod settings;
mod subscriptions;
mod theme;
mod widgets;

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{RouteActionKind, RoutePreset};
use xross_core::service::UiSnapshot;
use xross_core::settings::{Language, LogLevel};
use xross_core::trace::{TracePhase, TraceStepKind};
use xross_core::AppService;

use self::theme::{palette, Palette};
use self::widgets::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Nodes,
    Subscriptions,
    Routing,
    Dns,
    Connections,
    Logs,
    Settings,
}

impl Page {
    fn all() -> [Page; 7] {
        [
            Page::Nodes,
            Page::Subscriptions,
            Page::Routing,
            Page::Dns,
            Page::Connections,
            Page::Logs,
            Page::Settings,
        ]
    }

    fn icon(&self) -> &'static str {
        match self {
            Page::Nodes => "◈",
            Page::Subscriptions => "⇅",
            Page::Routing => "⇄",
            Page::Dns => "▤",
            Page::Connections => "⇆",
            Page::Logs => "≡",
            Page::Settings => "⚙",
        }
    }

    fn zh(&self) -> &'static str {
        match self {
            Page::Nodes => "节点",
            Page::Subscriptions => "订阅",
            Page::Routing => "路由",
            Page::Dns => "DNS",
            Page::Connections => "连接",
            Page::Logs => "日志",
            Page::Settings => "设置",
        }
    }

    fn en(&self) -> &'static str {
        match self {
            Page::Nodes => "Nodes",
            Page::Subscriptions => "Subscriptions",
            Page::Routing => "Routing",
            Page::Dns => "DNS",
            Page::Connections => "Connections",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherKind {
    Domain,
    Suffix,
    Geo,
    Ip,
    Process,
    Port,
}

impl MatcherKind {
    fn label(&self, lang: Language) -> SharedString {
        let (zh, en) = match self {
            MatcherKind::Domain => ("完整域名", "Exact domain"),
            MatcherKind::Suffix => ("域名后缀", "Domain suffix"),
            MatcherKind::Geo => ("GeoSite", "GeoSite"),
            MatcherKind::Ip => ("IP / CIDR", "IP / CIDR"),
            MatcherKind::Process => ("进程名", "Process"),
            MatcherKind::Port => ("端口", "Port"),
        };
        tr(lang, zh, en)
    }
}

fn tr(lang: Language, zh: &'static str, en: &'static str) -> SharedString {
    crate::i18n::tr(lang, zh, en)
}

fn text_input(
    window: &mut Window,
    cx: &mut Context<AppView>,
    placeholder: &str,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()))
}

pub struct AppView {
    pub(crate) service: AppService,
    pub(crate) snapshot: UiSnapshot,
    pub(crate) logo_badge: std::sync::Arc<gpui_kit::RenderImage>,
    pub(crate) logo_hero: std::sync::Arc<gpui_kit::RenderImage>,
    pub(crate) page: Page,
    pub(crate) collapsed: bool,

    pub(crate) sub_name: Entity<InputState>,
    pub(crate) sub_url: Entity<InputState>,
    pub(crate) import_input: Entity<InputState>,
    pub(crate) log_search: Entity<InputState>,
    pub(crate) rule_name: Entity<InputState>,
    pub(crate) rule_value: Entity<InputState>,
    pub(crate) dns_tag: Entity<InputState>,
    pub(crate) dns_address: Entity<InputState>,
    pub(crate) dns_domains: Entity<InputState>,
    pub(crate) host_domain: Entity<InputState>,
    pub(crate) host_ips: Entity<InputState>,
    pub(crate) port_input: Entity<InputState>,
    pub(crate) endpoint_input: Entity<InputState>,
    pub(crate) trace_input: Entity<InputState>,
    pub(crate) trace_anim: f32,
    pub(crate) auto_trace_started: bool,
    pub(crate) last_refresh: std::time::Instant,

    pub(crate) show_subscription_form: bool,
    pub(crate) show_rule_form: bool,
    pub(crate) show_dns_form: bool,
    pub(crate) show_host_form: bool,
    pub(crate) matcher_kind: MatcherKind,
    pub(crate) rule_action: RouteActionKind,
    pub(crate) selected_group: Option<String>,
    pub(crate) log_level: LogLevel,
    pub(crate) qr_node: Option<String>,
    pub(crate) show_qr: bool,
}

impl AppView {
    pub fn new(service: AppService, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let snapshot = service.snapshot();
        let mixed_port = snapshot.settings.mixed_port.to_string();
        let model = snapshot.model.clone();
        let default_group = model
            .default_group_id()
            .or_else(|| model.groups.first().map(|g| g.id.clone()));

        let sub_name = text_input(window, cx, "订阅名称（可选）");
        let sub_url = text_input(
            window,
            cx,
            "https://example.com/api/v1/client/subscribe?token=...",
        );
        let import_input = text_input(
            window,
            cx,
            "粘贴 vmess:// / vless:// / trojan:// / ss:// 链接",
        );
        let log_search = text_input(window, cx, "搜索日志…");
        let rule_name = text_input(window, cx, "规则名称");
        let rule_value = text_input(window, cx, "example.com, 或 cn（Geo），或 10.0.0.0/8");
        let dns_tag = text_input(window, cx, "tag，例如 remote");
        let dns_address = text_input(window, cx, "1.1.1.1 或 https://1.1.1.1/dns-query");
        let dns_domains = text_input(
            window,
            cx,
            "域名条件，例如 geosite:geolocation-!cn（逗号分隔）",
        );
        let host_domain = text_input(window, cx, "example.com");
        let host_ips = text_input(window, cx, "1.2.3.4, 5.6.7.8");
        let port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("7890")
                .default_value(mixed_port)
        });
        let endpoint_input = text_input(
            window,
            cx,
            "自定义 core 可执行文件路径（留空使用已安装版本）",
        );
        let trace_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(
                    "https://example.com 或 tcp://1.1.1.1:443 / udp://8.8.8.8:53 / icmp://1.1.1.1",
                )
                .default_value("https://www.gstatic.com/generate_204")
        });

        let initial_page = match std::env::var("XROSS_PAGE").unwrap_or_default().as_str() {
            "subscriptions" => Page::Subscriptions,
            "routing" => Page::Routing,
            "dns" => Page::Dns,
            "connections" => Page::Connections,
            "logs" => Page::Logs,
            "settings" => Page::Settings,
            _ => Page::Nodes,
        };
        let mut view = Self {
            snapshot,
            logo_badge: crate::brand_assets::logo_badge(),
            logo_hero: crate::brand_assets::logo_hero(),
            page: initial_page,
            collapsed: false,
            service,
            sub_name,
            sub_url,
            import_input,
            log_search,
            rule_name,
            rule_value,
            dns_tag,
            dns_address,
            dns_domains,
            host_domain,
            host_ips,
            port_input,
            endpoint_input,
            trace_input,
            trace_anim: 0.0,
            auto_trace_started: false,
            last_refresh: std::time::Instant::now(),
            show_subscription_form: false,
            show_rule_form: false,
            show_dns_form: false,
            show_host_form: false,
            matcher_kind: MatcherKind::Suffix,
            rule_action: RouteActionKind::Proxy,
            selected_group: default_group,
            log_level: LogLevel::Info,
            qr_node: None,
            show_qr: false,
        };
        view
    }

    fn tr(&self, zh: &'static str, en: &'static str) -> SharedString {
        tr(self.snapshot.settings.language, zh, en)
    }

    fn lang(&self) -> Language {
        self.snapshot.settings.language
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        self.snapshot = self.service.snapshot();
        cx.notify();
    }

    /// Called by the main-thread tick task. Polls state, advances the trace
    /// animation and repaints only when something changed.
    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
        if !self.auto_trace_started {
            self.auto_trace_started = true;
            if let Ok(target) = std::env::var("XROSS_TRACE") {
                if !target.trim().is_empty() {
                    let _ = self.service.start_trace(&target);
                }
            }
        }
        let now = std::time::Instant::now();
        let current = self.service.trace_snapshot();
        let active = current
            .as_ref()
            .map(|trace| !matches!(trace.phase, TracePhase::Done | TracePhase::Failed))
            .unwrap_or(false);
        let changed = match (&current, &self.snapshot.trace) {
            (Some(new), Some(old)) => {
                new.phase != old.phase
                    || new.response_ms != old.response_ms
                    || new.summary != old.summary
                    || new.steps.iter().zip(old.steps.iter()).any(|(a, b)| {
                        a.status != b.status
                            || a.detail != b.detail
                            || a.duration_ms != b.duration_ms
                    })
            }
            (Some(_), None) => true,
            (None, Some(_)) => true,
            (None, None) => false,
        };
        if current.is_some() {
            self.snapshot.trace = current;
        }
        if active {
            self.trace_anim = (self.trace_anim + 0.018) % 1.0;
            cx.notify();
            return;
        }
        let refresh_due = now.duration_since(self.last_refresh) >= Duration::from_millis(400);
        if refresh_due {
            self.snapshot = self.service.snapshot();
            self.last_refresh = now;
            if self
                .selected_group
                .as_ref()
                .map(|id| self.snapshot.model.group(id).is_none())
                .unwrap_or(true)
            {
                self.selected_group = self.snapshot.model.default_group_id();
            }
            cx.notify();
        } else if changed {
            cx.notify();
        }
    }

    fn trace_step_label(&self, kind: TraceStepKind) -> SharedString {
        let (zh, en) = match kind {
            TraceStepKind::Source => ("请求源", "Source"),
            TraceStepKind::Inbound => ("入站", "Inbound"),
            TraceStepKind::Dns => ("DNS 解析", "DNS"),
            TraceStepKind::Route => ("路由 / 分流", "Route"),
            TraceStepKind::Outbound => ("出站节点", "Outbound"),
            TraceStepKind::Remote => ("目标服务", "Remote"),
            TraceStepKind::Response => ("响应返回", "Response"),
        };
        self.tr(zh, en)
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let width = if self.collapsed { 68.0 } else { 216.0 };
        div()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.panel)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .h(px(64.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                img(self.logo_badge.clone())
                                    .w(px(if self.collapsed { 30. } else { 32. }))
                                    .h(px(if self.collapsed { 30. } else { 32. })),
                            )
                            .when(!self.collapsed, |d| {
                                d.child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(p.fg)
                                        .child("Xross"),
                                )
                            }),
                    )
                    .child(
                        Button::new("collapse")
                            .compact()
                            .ghost()
                            .label(if self.collapsed { "»" } else { "«" })
                            .tooltip(if self.collapsed {
                                "展开菜单"
                            } else {
                                "收起菜单"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.collapsed = !this.collapsed;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex().flex_col().gap_1().p_2().flex_1().children(
                Page::all().into_iter().map(|page| {
                    let active = self.page == page;
                    div()
                        .id(SharedString::from(format!("nav-{page:?}")))
                        .flex()
                        .items_center()
                        .gap_3()
                        .h(px(40.))
                        .px_3()
                        .rounded_md()
                        .cursor_pointer()
                        .when(active, |d| d.bg(p.accent))
                        .hover(move |s| s.bg(p.panel_alt))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page = page;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(22.))
                                .text_center()
                                .text_base()
                                .text_color(if active { p.primary } else { p.muted_fg })
                                .child(page.icon()),
                        )
                        .when(!self.collapsed, |d| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(if active { p.fg } else { p.muted_fg })
                                    .child(tr(lang, page.zh(), page.en())),
                            )
                        })
                }),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(dot(if self.snapshot.process.phase.is_alive() {
                                p.success
                            } else {
                                p.muted_fg
                            }))
                            .when(!self.collapsed, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(self.snapshot.process.phase.label()),
                                )
                            }),
                    )
                    .when(!self.collapsed, |d| {
                        d.child(div().text_xs().text_color(p.muted_fg).child(format!(
                            "v{} · {}",
                            self.snapshot.app_version,
                            self.snapshot.settings.core.display_name()
                        )))
                    }),
            )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let running = self.snapshot.process.phase.is_alive();
        let status_color = if running { p.success } else { p.muted_fg };
        div()
            .h(px(64.))
            .px_6()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.border)
            .bg(p.bg)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.fg)
                            .child(self.tr(self.page.zh(), self.page.en())),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                        "配置变更需要重启 core 生效（不支持热加载）",
                        "Config changes require a core restart (no hot reload)",
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_0p5()
                            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                "↓ {}  ↑ {}",
                                xross_core::util::format_speed(self.snapshot.runtime.down_speed),
                                xross_core::util::format_speed(self.snapshot.runtime.up_speed)
                            )))
                            .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                "{} {}",
                                self.tr("累计", "Total"),
                                xross_core::util::format_bytes(
                                    self.snapshot.runtime.up_total
                                        + self.snapshot.runtime.down_total
                                )
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(p.panel)
                            .border_1()
                            .border_color(p.border)
                            .child(dot(status_color))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(p.fg)
                                    .child(self.snapshot.process.phase.label()),
                            ),
                    )
                    .when(self.snapshot.config_dirty, |d| {
                        d.child(
                            Button::new("apply-config")
                                .compact()
                                .warning()
                                .label(self.tr("应用配置", "Apply"))
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.service.apply_config();
                                })),
                        )
                    })
                    .child(
                        Button::new("toggle-core")
                            .compact()
                            .when(running, |b| b.danger())
                            .when(!running, |b| b.primary())
                            .label(if running {
                                self.tr("停止", "Stop")
                            } else {
                                self.tr("启动", "Start")
                            })
                            .loading(self.snapshot.busy.iter().any(|b| b == "core"))
                            .on_click(cx.listener(|this, _, _, _| {
                                this.service.toggle_core();
                            })),
                    ),
            )
    }

    fn page_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Nodes => self.page_nodes(cx).into_any_element(),
            Page::Subscriptions => self.page_subscriptions(cx).into_any_element(),
            Page::Routing => self.page_routing(cx).into_any_element(),
            Page::Dns => self.page_dns(cx).into_any_element(),
            Page::Connections => self.page_connections(cx).into_any_element(),
            Page::Logs => self.page_logs(cx).into_any_element(),
            Page::Settings => self.page_settings(window, cx).into_any_element(),
        }
    }

    fn toasts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .absolute()
            .right(px(18.))
            .bottom(px(18.))
            .w(px(420.))
            .flex()
            .flex_col()
            .gap_2()
            .children(self.snapshot.toasts.iter().rev().take(5).map(|toast| {
                let color = match toast.level {
                    xross_core::service::ToastLevel::Info => p.info,
                    xross_core::service::ToastLevel::Success => p.success,
                    xross_core::service::ToastLevel::Warning => p.warning,
                    xross_core::service::ToastLevel::Error => p.danger,
                };
                let id = toast.id;
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(p.panel)
                    .border_1()
                    .border_color(color)
                    .shadow_md()
                    .child(dot(color))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(p.fg)
                            .child(toast.message.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("toast-{id}")))
                            .compact()
                            .ghost()
                            .label("×")
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.service.dismiss_toast(id);
                            })),
                    )
            }))
    }

    fn sparkline(&self, p: &Palette) -> impl IntoElement {
        let samples = self.snapshot.runtime.history.clone();
        let max = samples
            .iter()
            .map(|s| s.up_speed + s.down_speed)
            .max()
            .unwrap_or(1)
            .max(1);
        let recent: Vec<_> = samples.iter().rev().take(72).cloned().collect();
        div()
            .h(px(48.))
            .w_full()
            .flex()
            .items_end()
            .gap_0p5()
            .children(recent.into_iter().rev().map(|sample| {
                let value = sample.up_speed + sample.down_speed;
                let height = ((value as f32 / max as f32) * 44.0).max(2.0);
                div().flex_1().h(px(height)).rounded_sm().bg(
                    if sample.down_speed >= sample.up_speed {
                        p.primary
                    } else {
                        p.info
                    },
                )
            }))
    }

    fn route_preset_label(&self, preset: RoutePreset) -> SharedString {
        self.tr(
            preset.label(),
            match preset {
                RoutePreset::BypassMainland => "Bypass mainland",
                RoutePreset::GfwList => "GFWList",
                RoutePreset::Global => "Global",
                RoutePreset::ProxyApps => "Selected apps",
                RoutePreset::Custom => "Custom",
            },
        )
    }
}

impl Drop for AppView {
    fn drop(&mut self) {
        self.service.shutdown();
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .relative()
            .flex()
            .size_full()
            .bg(p.bg)
            .text_color(p.fg)
            .font_family(cx.theme().font_family.clone())
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(self.header(cx))
                    .child(
                        div()
                            .id("content-scroll")
                            .flex_1()
                            .w_full()
                            .overflow_y_scroll()
                            .p_6()
                            .child(self.page_content(window, cx)),
                    ),
            )
            .child(self.toasts(cx))
    }
}
