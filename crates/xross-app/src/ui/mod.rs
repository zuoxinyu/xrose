mod about;
mod connections;
mod dialogs;
mod dns;
mod logs;
mod manual_node;
mod mesh;
mod nodes;
mod routing;
mod settings;
mod subscriptions;
pub(crate) mod theme;
mod title_controls;
mod widgets;

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::AppService;
use xross_core::diagnostics::{ConnectionFilterKind, DiagnosticRuleKind};
use xross_core::model::{RouteActionKind, RoutePreset};
use xross_core::service::UiSnapshot;
use xross_core::settings::{Language, LogLevel};
use xross_core::trace::TraceStepKind;

use self::manual_node::ManualNodeForm;
use self::theme::{Palette, palette};
use self::title_controls::WindowControls;
use self::widgets::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Mesh,
    Nodes,
    Subscriptions,
    Routing,
    Dns,
    Connections,
    Diagnostics,
    Logs,
    Settings,
    About,
}

impl Page {
    fn all() -> [Page; 9] {
        [
            Page::Nodes,
            Page::Subscriptions,
            Page::Routing,
            Page::Dns,
            Page::Mesh,
            Page::Connections,
            Page::Diagnostics,
            Page::Logs,
            Page::Settings,
        ]
    }

    fn icon(&self) -> IconName {
        match self {
            Page::Mesh => IconName::Network,
            Page::Nodes => IconName::Server,
            Page::Subscriptions => IconName::Rss,
            Page::Routing => IconName::Route,
            Page::Dns => IconName::Globe,
            Page::Connections => IconName::Activity,
            Page::Diagnostics => IconName::Radar,
            Page::Logs => IconName::Terminal,
            Page::Settings => IconName::Settings,
            Page::About => IconName::Info,
        }
    }

    fn zh(&self) -> &'static str {
        match self {
            Page::Mesh => "组网",
            Page::Nodes => "节点",
            Page::Subscriptions => "订阅",
            Page::Routing => "路由",
            Page::Dns => "DNS",
            Page::Connections => "连接",
            Page::Diagnostics => "测试",
            Page::Logs => "日志",
            Page::Settings => "设置",
            Page::About => "关于",
        }
    }

    fn en(&self) -> &'static str {
        match self {
            Page::Mesh => "Mesh networks",
            Page::Nodes => "Nodes",
            Page::Subscriptions => "Subscriptions",
            Page::Routing => "Routing",
            Page::Dns => "DNS",
            Page::Connections => "Connections",
            Page::Diagnostics => "Diagnostics",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
            Page::About => "About",
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
    mesh_form: mesh::MeshForm,
    mesh_expanded: std::collections::HashSet<String>,
    mesh_overflow: std::collections::HashMap<String, bool>,
    pub(crate) service: AppService,
    pub(crate) snapshot: UiSnapshot,
    pub(crate) logo_badge: std::sync::Arc<gpui_kit::RenderImage>,
    pub(crate) logo_hero: std::sync::Arc<gpui_kit::RenderImage>,
    pub(crate) page: Page,

    pub(crate) focus_handle: FocusHandle,
    pub(crate) node_search: Entity<InputState>,
    pub(crate) show_import_box: bool,
    pub(crate) show_manual_form: bool,
    pub(crate) manual_form: ManualNodeForm,
    pub(crate) manual_error: Option<String>,
    pub(crate) editing_node: Option<String>,
    pub(crate) hide_ips: bool,
    /// Overflow menu of the node list toolbar (batch management, cleanup).
    pub(crate) toolbar_menu_open: bool,
    /// Nodes of the latency run started from this window; they show a spinner
    /// instead of a stale value while the run is busy.
    pub(crate) latency_pending: std::collections::HashSet<String>,
    pub(crate) node_menu_open: Option<String>,
    pub(crate) sort_by_latency: bool,
    pub(crate) group_by_subscription: bool,
    pub(crate) collapsed_subs: std::collections::HashSet<String>,
    pub(crate) batch_mode: bool,
    pub(crate) batch_selected: std::collections::HashSet<String>,
    pub(crate) show_clean_confirm: bool,
    pub(crate) sub_name: Entity<InputState>,
    pub(crate) sub_url: Entity<InputState>,
    pub(crate) import_input: Entity<InputState>,
    pub(crate) log_search: Entity<InputState>,
    log_list: logs::LogListState,
    pub(crate) rule_name: Entity<InputState>,
    pub(crate) rule_group_name: Entity<InputState>,
    pub(crate) rule_group_id: Option<String>,
    pub(crate) rule_move_id: Option<String>,
    pub(crate) rule_value: Entity<InputState>,
    pub(crate) dns_tag: Entity<InputState>,
    pub(crate) dns_address: Entity<InputState>,
    pub(crate) dns_domains: Entity<InputState>,
    pub(crate) host_domain: Entity<InputState>,
    pub(crate) host_ips: Entity<InputState>,
    pub(crate) port_input: Entity<InputState>,
    pub(crate) endpoint_input: Entity<InputState>,
    pub(crate) trace_input: Entity<InputState>,
    pub(crate) connection_search: Entity<InputState>,
    pub(crate) connection_filter: ConnectionFilterKind,
    pub(crate) show_recorded_connections: bool,
    pub(crate) connections_limit: usize,
    pub(crate) selected_connection: Option<xross_core::runtime::ClashConnection>,
    pub(crate) diagnostic_rule_kind: DiagnosticRuleKind,
    pub(crate) diagnostic_rule_action: RouteActionKind,
    pub(crate) expanded_trace_step: Option<TraceStepKind>,
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
        let node_search = text_input(window, cx, "按名称或地址过滤节点…");
        let log_search = text_input(window, cx, "搜索日志…");
        let rule_name = text_input(window, cx, "规则名称");
        let rule_group_name = text_input(window, cx, "分组名称");
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
        let manual_form = ManualNodeForm::new(window, cx);

        let initial_page = match std::env::var("XROSS_PAGE").unwrap_or_default().as_str() {
            "mesh" => Page::Mesh,
            "subscriptions" => Page::Subscriptions,
            "routing" => Page::Routing,
            "dns" => Page::Dns,
            "connections" => Page::Connections,
            "diagnostics" | "test" | "tests" => Page::Diagnostics,
            "logs" => Page::Logs,
            "settings" => Page::Settings,
            "about" => Page::About,
            _ => Page::Nodes,
        };
        // Debug hook used by the screenshot/regression workflow.
        let show_manual_form = std::env::var_os("XROSS_MANUAL").is_some();
        let hide_ips = snapshot.settings.privacy_mode;
        let view = Self {
            mesh_form: mesh::MeshForm::new(window, cx),
            mesh_expanded: std::collections::HashSet::new(),
            mesh_overflow: std::collections::HashMap::new(),
            snapshot,
            logo_badge: crate::brand_assets::logo_badge(),
            logo_hero: crate::brand_assets::logo_hero(),
            page: initial_page,
            service,
            focus_handle: cx.focus_handle(),
            node_search,
            show_import_box: false,
            show_manual_form,
            manual_form,
            manual_error: None,
            editing_node: None,
            hide_ips,
            toolbar_menu_open: false,
            latency_pending: std::collections::HashSet::new(),
            node_menu_open: None,
            sort_by_latency: false,
            group_by_subscription: false,
            collapsed_subs: std::collections::HashSet::new(),
            batch_mode: false,
            batch_selected: std::collections::HashSet::new(),
            show_clean_confirm: false,
            sub_name,
            sub_url,
            import_input,
            log_search,
            log_list: logs::LogListState::new(),
            rule_name,
            rule_group_name,
            rule_group_id: None,
            rule_move_id: None,
            rule_value,
            dns_tag,
            dns_address,
            dns_domains,
            host_domain,
            host_ips,
            port_input,
            endpoint_input,
            trace_input,
            connection_search: text_input(window, cx, "过滤域名、IP 或进程…"),
            connection_filter: ConnectionFilterKind::All,
            show_recorded_connections: false,
            connections_limit: 50,
            selected_connection: None,
            diagnostic_rule_kind: DiagnosticRuleKind::Domain,
            diagnostic_rule_action: RouteActionKind::Proxy,
            expanded_trace_step: None,
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
        };
        view.refresh_header_location();
        view.localize_inputs(window, cx);
        if show_manual_form {
            cx.defer_in(window, |this, window, cx| {
                this.open_node_dialog(None, true, window, cx);
            });
        }
        view
    }

    fn tr(&self, zh: &'static str, en: &'static str) -> SharedString {
        tr(self.snapshot.settings.language, zh, en)
    }

    fn lang(&self) -> Language {
        self.snapshot.settings.language
    }

    fn localize_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        for (input, placeholder) in [
            (
                &self.node_search,
                self.tr("按名称或地址过滤节点…", "Filter nodes by name or address…"),
            ),
            (&self.log_search, self.tr("搜索日志…", "Search logs…")),
            (
                &self.connection_search,
                self.tr("过滤域名、IP 或进程…", "Filter domain, IP or process…"),
            ),
            (
                &self.import_input,
                self.tr(
                    "粘贴 vmess:// / vless:// / trojan:// / ss:// 链接",
                    "Paste vmess:// / vless:// / trojan:// / ss:// links",
                ),
            ),
            (
                &self.sub_name,
                self.tr("订阅名称（可选）", "Subscription name (optional)"),
            ),
            (&self.rule_name, self.tr("规则名称", "Rule name")),
            (
                &self.rule_value,
                self.tr(
                    "example.com, 或 cn（Geo），或 10.0.0.0/8",
                    "example.com, cn (Geo), or 10.0.0.0/8",
                ),
            ),
            (
                &self.dns_tag,
                self.tr("tag，例如 remote", "Tag, e.g. remote"),
            ),
            (
                &self.dns_address,
                self.tr(
                    "1.1.1.1 或 https://1.1.1.1/dns-query",
                    "1.1.1.1 or https://1.1.1.1/dns-query",
                ),
            ),
            (
                &self.dns_domains,
                self.tr(
                    "域名条件，例如 geosite:geolocation-!cn（逗号分隔）",
                    "Domain conditions, e.g. geosite:geolocation-!cn (comma separated)",
                ),
            ),
            (
                &self.endpoint_input,
                self.tr(
                    "自定义 core 可执行文件路径（留空使用已安装版本）",
                    "Custom core executable path (leave empty to use installed version)",
                ),
            ),
            (
                &self.trace_input,
                self.tr(
                    "https://example.com 或 tcp://1.1.1.1:443 / udp://8.8.8.8:53 / icmp://1.1.1.1",
                    "https://example.com or tcp://1.1.1.1:443 / udp://8.8.8.8:53 / icmp://1.1.1.1",
                ),
            ),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        self.snapshot = self.service.snapshot();
        self.refresh_header_location();
        cx.notify();
    }

    fn refresh_header_location(&self) {
        if let Some(node) = self.snapshot.selected_node(self.selected_group.as_deref()) {
            self.service.request_node_location(&node.id);
        }
    }

    /// Poll state without starving live connections while a trace is active.
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
        self.snapshot.trace = current;
        let refresh_due = now.duration_since(self.last_refresh) >= Duration::from_millis(400);
        if refresh_due {
            self.snapshot = self.service.snapshot();
            self.last_refresh = now;
            if !self.snapshot.busy.iter().any(|b| b == "latency") {
                self.latency_pending.clear();
            }
            if self
                .selected_group
                .as_ref()
                .map(|id| self.snapshot.model.group(id).is_none())
                .unwrap_or(true)
            {
                self.selected_group = self.snapshot.model.default_group_id();
            }
            self.refresh_header_location();
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
        div()
            .id("sidebar-container")
            .w(px(220.))
            .h_full()
            .flex()
            .flex_col()
            .bg(p.sidebar())
            .border_r_1()
            .border_color(p.border)
            .when(cfg!(target_os = "macos"), |d| {
                d.child(
                    div()
                        .id("sidebar-traffic-lights-spacer")
                        .h(px(36.))
                        .w_full()
                        .on_mouse_down(MouseButton::Left, |ev, window, _| {
                            if ev.click_count == 2 {
                                window.titlebar_double_click();
                            } else {
                                window.start_window_move();
                            }
                        }),
                )
            })
            .child(
                div()
                    .id("sidebar-brand")
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_start()
                    .gap_3()
                    .when(cfg!(target_os = "windows"), |d| {
                        d.window_control_area(WindowControlArea::Drag)
                    })
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        if ev.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    })
                    .child(img(self.logo_badge.clone()).w(px(28.)).h(px(28.)))
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::BOLD)
                            .text_color(p.fg)
                            .child("Xross"),
                    ),
            )
            .child(div().flex().flex_col().gap_1().p_2().flex_1().children(
                Page::all().into_iter().map(|page| {
                    let active = self.page == page;
                    div()
                        .id(SharedString::from(format!("nav-{page:?}")))
                        .flex()
                        .items_center()
                        .justify_start()
                        .gap_3()
                        .px_3()
                        .h(px(40.))
                        .rounded_md()
                        .cursor_pointer()
                        .when(active, |d| d.bg(p.accent))
                        .hover(move |s| s.bg(p.panel_alt))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page = page;
                            cx.notify();
                        }))
                        .child(Icon::new(page.icon()).size(px(18.)).text_color(if active {
                            p.primary
                        } else {
                            p.muted_fg
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(if active {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(if active { p.fg } else { p.muted_fg })
                                .child(tr(lang, page.zh(), page.en())),
                        )
                }),
            ))
            .child({
                let about_active = self.page == Page::About;
                div().p_2().border_t_1().border_color(p.border).child(
                    div()
                        .id("nav-About")
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .h(px(36.))
                        .rounded_md()
                        .cursor_pointer()
                        .when(about_active, |d| d.bg(p.accent))
                        .hover(move |s| s.bg(p.panel_alt))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::About;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_xs()
                                .font_weight(if about_active {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(if about_active { p.primary } else { p.fg })
                                .child(Icon::new(IconName::Info).size(px(13.)))
                                .child(self.tr("关于", "About")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted_fg)
                                .child(format!("v{}", self.snapshot.app_version)),
                        ),
                )
            })
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let hide = self.hide_ips;
        let is_nodes_page = self.page == Page::Nodes;
        let running = self.snapshot.process.phase.is_alive();
        let core_busy = self.snapshot.busy.iter().any(|b| b == "core");
        let status_color = if running { p.success } else { p.muted_fg };
        let node = self.snapshot.selected_node(self.selected_group.as_deref());
        let latency = node.and_then(|n| self.snapshot.latency.get(&n.id));
        let delay = latency
            .map(|entry| {
                entry
                    .delay_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| self.tr("超时", "Timeout").to_string())
            })
            .unwrap_or_else(|| self.tr("未测速", "Untested").to_string());
        let delay_color = theme::latency_color(&p, latency);
        let delay_stale =
            latency.is_some_and(|e| theme::latency_age_secs(e) > theme::LATENCY_STALE_SECS);
        // Privacy mode masks everything derived from the server address, so a
        // screenshot of the title bar cannot reveal where the node is.
        let node_name = node
            .map(|n| {
                if hide && !n.server.is_empty() {
                    n.name.replace(&n.server, "•••")
                } else {
                    n.name.clone()
                }
            })
            .unwrap_or_else(|| self.tr("未选择节点", "No node selected").to_string());
        let location = node.and_then(|n| self.snapshot.node_location(n));
        let location_label = if hide {
            self.tr("位置已隐藏", "Location hidden").to_string()
        } else {
            location
                .and_then(|entry| entry.label.clone())
                .unwrap_or_else(|| {
                    if location.is_none_or(|entry| entry.pending) {
                        self.tr("归属地查询中…", "Looking up location…").to_string()
                    } else {
                        self.tr("归属地未知", "Location unknown").to_string()
                    }
                })
        };
        let stop_hover_bg = p.danger;
        let start_hover_bg = p.primary.opacity(0.85);
        let location_tip = if hide {
            self.tr(
                "已隐藏服务器地址与位置，点击显示",
                "Addresses and location hidden — click to reveal",
            )
        } else {
            self.tr(
                "点击隐藏服务器地址与位置",
                "Click to hide server addresses and location",
            )
        };
        div()
            .id("header-bar")
            .h(px(54.))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(p.border)
            .bg(if p.is_dark { p.bg } else { p.panel })
            .child(
                // Noninteractive node metadata shares the caption drag region;
                // clicking the location line toggles privacy mode.
                div()
                    .id("header-drag")
                    .flex()
                    .items_center()
                    .px_4()
                    .h_full()
                    .flex_1()
                    .min_w(px(0.))
                    .when(cfg!(target_os = "windows"), |d| {
                        d.window_control_area(WindowControlArea::Drag)
                    })
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        if ev.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    })
                    .when(!is_nodes_page, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_0p5()
                                .min_w(px(0.))
                                .w_full()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .min_w(px(0.))
                                        .w_full()
                                        .child(
                                            div()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(p.fg)
                                                .child(node_name),
                                        )
                                        .when(node.is_some(), |d| {
                                            d.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(delay_color)
                                                    .when(delay_stale, |d| d.opacity(0.6))
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .child(delay),
                                            )
                                        }),
                                )
                                .when(node.is_some(), |d| {
                                    d.child(
                                        div()
                                            .id("header-location-toggle")
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .min_w(px(0.))
                                            .max_w_full()
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .hover(move |s| s.text_color(p.fg))
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(location_tip.clone()).build(window, cx)
                                            })
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation();
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.hide_ips = !this.hide_ips;
                                                let mut settings = this.snapshot.settings.clone();
                                                settings.privacy_mode = this.hide_ips;
                                                this.service.update_settings(settings);
                                                this.sync(cx);
                                            }))
                                            .child(
                                                Icon::new(if hide {
                                                    IconName::EyeOff
                                                } else {
                                                    IconName::MapPin
                                                })
                                                .size(px(12.))
                                                .flex_shrink_0(),
                                            )
                                            .child(
                                                div()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .child(location_label),
                                            ),
                                    )
                                }),
                        )
                    }),
            )
            .child(
                // Centered status pill: 1px inner padding + rounded right corners on
                // the action segment keep its fill strictly inside the pill border.
                div()
                    .id("header-status")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .px_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(28.))
                            .p(px(1.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(p.panel)
                            .border_1()
                            .border_color(p.border)
                            .child({
                                let status_tip = running.then(|| {
                                    let proxy_text =
                                        if self.snapshot.active_proxy_addresses.is_empty() {
                                            self.tr("未启用", "Disabled").to_string()
                                        } else {
                                            self.snapshot.active_proxy_addresses.join(", ")
                                        };
                                    let tun_text =
                                        self.snapshot.active_tun_interface.clone().unwrap_or_else(
                                            || self.tr("未启用", "Disabled").to_string(),
                                        );
                                    (
                                        format!("{}：{}", self.tr("代理地址", "Proxy"), proxy_text),
                                        format!(
                                            "{}：{}",
                                            self.tr("TUN 网卡", "TUN interface"),
                                            tun_text
                                        ),
                                    )
                                });
                                div()
                                    .id("header-status-phase")
                                    .flex()
                                    .items_center()
                                    .h_full()
                                    .gap_2()
                                    .pl_3()
                                    .pr_2p5()
                                    .when_some(status_tip, |d, (proxy_line, tun_line)| {
                                        d.tooltip(move |window, cx| {
                                            let proxy_line = proxy_line.clone();
                                            let tun_line = tun_line.clone();
                                            Tooltip::element(move |_, _| {
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_0p5()
                                                    .child(proxy_line.clone())
                                                    .child(tun_line.clone())
                                            })
                                            .build(window, cx)
                                        })
                                    })
                                    .child(dot(status_color))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if running { p.fg } else { p.muted_fg })
                                            .child(match self.snapshot.process.phase {
                                                xross_core::core::ProcessPhase::Stopped => {
                                                    self.tr("已停止", "Stopped")
                                                }
                                                xross_core::core::ProcessPhase::Starting => {
                                                    self.tr("启动中", "Starting")
                                                }
                                                xross_core::core::ProcessPhase::Running => {
                                                    self.tr("运行中", "Running")
                                                }
                                                xross_core::core::ProcessPhase::Stopping => {
                                                    self.tr("停止中", "Stopping")
                                                }
                                                xross_core::core::ProcessPhase::Failed => {
                                                    self.tr("启动失败", "Failed")
                                                }
                                            }),
                                    )
                            })
                            .child(
                                div()
                                    .id("toggle-core")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .h_full()
                                    .px_3()
                                    .rounded_r_full()
                                    .cursor_pointer()
                                    .border_l_1()
                                    .border_color(p.border)
                                    .when(running, |d| {
                                        d.text_color(p.danger_text).hover(move |style| {
                                            style
                                                .bg(stop_hover_bg)
                                                .text_color(gpui_kit::hsla(0., 0., 1., 1.))
                                        })
                                    })
                                    .when(!running, |d| {
                                        d.bg(p.primary)
                                            .text_color(p.primary_fg)
                                            .hover(move |style| style.bg(start_hover_bg))
                                    })
                                    .on_click(cx.listener(|this, _, _, _| {
                                        this.service.toggle_core();
                                    }))
                                    .child(
                                        div().text_xs().font_weight(FontWeight::SEMIBOLD).child(
                                            if core_busy {
                                                "…".to_string()
                                            } else if running {
                                                self.tr("停止", "Stop").to_string()
                                            } else {
                                                self.tr("启动", "Start").to_string()
                                            },
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                // Traffic figures on the right; the empty span before them is a drag region.
                div()
                    .id("header-traffic")
                    .flex()
                    .items_center()
                    .justify_end()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .child(
                        div()
                            .id("header-traffic-drag")
                            .flex_1()
                            .min_w(px(8.))
                            .h_full()
                            .when(cfg!(target_os = "windows"), |d| {
                                d.window_control_area(WindowControlArea::Drag)
                            })
                            .on_mouse_down(MouseButton::Left, |ev, window, _| {
                                if ev.click_count == 2 {
                                    window.titlebar_double_click();
                                } else {
                                    window.start_window_move();
                                }
                            }),
                    )
                    .when(!is_nodes_page, |d| {
                        d.child(
                            div()
                                .id("header-traffic-figures")
                                .flex()
                                .flex_col()
                                .items_end()
                                .justify_center()
                                .h_full()
                                .flex_shrink_0()
                                .when(cfg!(target_os = "windows"), |d| d.pr_2())
                                .when(!cfg!(target_os = "windows"), |d| d.pr_4())
                                .gap_0p5()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .gap_2()
                                        .text_xs()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(div().text_color(p.data_down).child(format!(
                                            "↓ {}",
                                            xross_core::util::format_speed(
                                                self.snapshot.runtime.down_speed
                                            )
                                        )))
                                        .child(div().text_color(p.data_up).child(format!(
                                            "↑ {}",
                                            xross_core::util::format_speed(
                                                self.snapshot.runtime.up_speed
                                            )
                                        ))),
                                )
                                .child(div().text_xs().text_color(p.muted_fg).child(format!(
                                    "{} {}",
                                    self.tr("累计", "Total"),
                                    xross_core::util::format_bytes(
                                        self.snapshot.runtime.up_total
                                            + self.snapshot.runtime.down_total
                                    )
                                ))),
                        )
                    })
                    .child(WindowControls::new(p)),
            )
    }

    fn config_dirty_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .mx_6()
            .mt_3()
            .px_4()
            .py_2p5()
            .rounded_lg()
            .bg(p.accent)
            .border_1()
            .border_color(p.warning)
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size(px(16.))
                            .text_color(p.warning),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.fg)
                            .child(self.tr(
                                "有未应用的配置变更，需重启 Core 生效",
                                "Unapplied config changes. Restart Core to apply.",
                            )),
                    ),
            )
            .child(
                Button::new("apply-dirty-config")
                    .compact()
                    .primary()
                    .label(self.tr("立即应用并重启", "Apply & Restart"))
                    .on_click(cx.listener(|this, _, _, _| {
                        this.service.apply_config();
                    })),
            )
    }

    fn page_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Nodes => self.page_nodes(window, cx).into_any_element(),
            Page::Subscriptions => self.page_subscriptions(cx).into_any_element(),
            Page::Routing => self.page_routing(cx).into_any_element(),
            Page::Dns => self.page_dns(cx).into_any_element(),
            Page::Connections => self.page_connections(cx).into_any_element(),
            Page::Diagnostics => self.page_diagnostics(cx).into_any_element(),
            Page::Logs => self.page_logs(cx).into_any_element(),
            Page::Settings => self.page_settings(window, cx).into_any_element(),
            Page::About => self.page_about(cx).into_any_element(),
            Page::Mesh => self.page_mesh(cx).into_any_element(),
        }
    }

    fn toasts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .absolute()
            .right(px(18.))
            .bottom(px(18.))
            .w(px(420.))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                self.snapshot
                    .toasts
                    .iter()
                    .rev()
                    .filter(|toast| {
                        toast.visible_in_app(self.snapshot.settings.system_notifications)
                    })
                    .take(5)
                    .map(|toast| {
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
                            .w_full()
                            .min_w(px(0.))
                            .gap_2()
                            .p_3()
                            .rounded_md()
                            .bg(p.panel)
                            .border_1()
                            .border_color(color)
                            .shadow_md()
                            .child(
                                div()
                                    .h(px(20.))
                                    .flex()
                                    .items_center()
                                    .flex_shrink_0()
                                    .child(dot(color)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_sm()
                                    .line_height(px(20.))
                                    .whitespace_normal()
                                    .overflow_hidden()
                                    .text_color(p.fg)
                                    .child(toast.message.clone()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("toast-{id}")))
                                    .compact()
                                    .ghost()
                                    .icon(IconName::X)
                                    .flex_shrink_0()
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.service.dismiss_toast(id);
                                    })),
                            )
                    }),
            )
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
                        p.data_down
                    } else {
                        p.data_up
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
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let mods = event.keystroke.modifiers;
                let key = event.keystroke.key.as_str();
                // Development shortcut for repeated flyout lifecycle checks.
                #[cfg(debug_assertions)]
                if (mods.control || mods.platform) && mods.shift && key == "t" {
                    crate::tray::queue_command(crate::tray::TrayCommand::TogglePopup);
                }
                if mods.platform {
                    match key {
                        "," => {
                            this.page = Page::Settings;
                            cx.notify();
                        }
                        "1" => {
                            this.page = Page::Nodes;
                            cx.notify();
                        }
                        "2" => {
                            this.page = Page::Subscriptions;
                            cx.notify();
                        }
                        "3" => {
                            this.page = Page::Routing;
                            cx.notify();
                        }
                        "4" => {
                            this.page = Page::Dns;
                            cx.notify();
                        }
                        "5" => {
                            this.page = Page::Connections;
                            cx.notify();
                        }
                        "6" => {
                            this.page = Page::Diagnostics;
                            cx.notify();
                        }
                        "7" => {
                            this.page = Page::Logs;
                            cx.notify();
                        }
                        "8" => {
                            this.page = Page::Settings;
                            cx.notify();
                        }
                        "r" => {
                            this.service
                                .test_latency(xross_core::service::LatencyScope::All);
                            cx.notify();
                        }
                        "q" => {
                            crate::tray::request_quit();
                            cx.quit();
                        }
                        _ => {}
                    }
                }
            }))
            .relative()
            .flex()
            .size_full()
            .bg(if crate::window_effects::blur_backdrop() {
                hsla(0., 0., 0., 0.)
            } else {
                p.bg
            })
            .text_color(p.fg)
            .font_family(cx.theme().font_family.clone())
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .bg(p.bg)
                    .overflow_hidden()
                    .child(self.header(cx))
                    .when(self.snapshot.config_dirty, |d| {
                        d.child(self.config_dirty_banner(cx))
                    })
                    .child(
                        div()
                            .id("content-scroll")
                            .flex_1()
                            .min_h(px(0.))
                            .w_full()
                            .when(self.page == Page::Logs, |d| {
                                d.flex().flex_col().overflow_hidden()
                            })
                            .when(self.page != Page::Logs, |d| d.overflow_y_scroll())
                            .p_6()
                            .child(self.page_content(window, cx)),
                    ),
            )
            .child(self.toasts(cx))
    }
}
