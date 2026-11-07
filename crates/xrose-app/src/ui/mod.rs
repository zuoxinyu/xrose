mod about;
mod connections;
mod dialogs;
mod dns;
mod logs;
mod manual_node;
mod menus;
mod mesh;
mod mesh_sync;
mod nodes;
mod route_drag;
mod route_transfer;
mod routing;
mod settings;
mod subscriptions;
pub(crate) mod theme;
mod title_controls;
mod traffic_statistics;
mod widgets;

use std::time::Duration;

use gpui_base::{Disableable, Selectable};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::AppService;
use xrose_core::core::logs::LogFilter;
use xrose_core::diagnostics::{ConnectionFilterKind, DiagnosticRuleKind};
use xrose_core::model::{RouteActionKind, RoutePreset};
use xrose_core::service::UiSnapshot;
use xrose_core::settings::Language;
use xrose_core::trace::TraceStepKind;

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
            Page::Connections => IconName::ArrowLeftRight,
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
    RuleSet,
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
            MatcherKind::RuleSet => ("规则集", "Rule set"),
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
    traffic_ui: traffic_statistics::TrafficUi,
    mesh_form: mesh::MeshForm,
    forward_editor: Option<mesh::ForwardEditor>,
    mesh_sync_ui: mesh_sync::SyncUi,
    mesh_probe_feedback: mesh::ProbeFeedback,
    mesh_expanded: std::collections::HashSet<String>,
    collapsed_mesh_devices: std::collections::HashSet<String>,
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
    pub(crate) hide_traffic_chart: bool,
    /// Nodes of the latency run started from this window; they show a spinner
    /// instead of a stale value while the run is busy.
    pub(crate) latency_pending: std::collections::HashSet<String>,
    pub(crate) sort_by_latency: bool,
    pub(crate) group_by_subscription: bool,
    pub(crate) collapsed_subs: std::collections::HashSet<String>,
    pub(crate) batch_mode: bool,
    pub(crate) batch_selected: std::collections::HashSet<String>,
    pub(crate) sub_name: Entity<InputState>,
    pub(crate) sub_url: Entity<InputState>,
    pub(crate) import_input: Entity<InputState>,
    pub(crate) log_search: Entity<InputState>,
    log_list: logs::LogListState,
    log_filter: LogFilter,
    pub(crate) rule_name: Entity<InputState>,
    pub(crate) rule_group_id: Option<String>,
    route_drag_epoch: u64,
    route_scroll_running: bool,
    action_menu: Option<WeakEntity<gpui_kit::component::menu::PopupMenu>>,
    active_route_drag: Option<route_drag::RouteDrag>,
    content_scroll: ScrollHandle,
    settings_tab: settings::SettingsTab,
    settings_scroll: ScrollHandle,
    settings_tab_scroll: ScrollHandle,
    settings_tab_focus: [FocusHandle; 6],
    pub(crate) rule_value: Entity<InputState>,
    pub(crate) dns_tag: Entity<InputState>,
    pub(crate) dns_address: Entity<InputState>,
    pub(crate) dns_domains: Entity<InputState>,
    pub(crate) host_domain: Entity<InputState>,
    pub(crate) host_ips: Entity<InputState>,
    pub(crate) port_input: Entity<InputState>,
    network_advanced_open: bool,
    lan_port_input: Entity<InputState>,
    lan_user_input: Entity<InputState>,
    lan_password_input: Entity<InputState>,
    update_agent_input: Entity<InputState>,
    pub(crate) endpoint_input: Entity<InputState>,
    pub(crate) trace_input: Entity<InputState>,
    pub(crate) connection_search: Entity<InputState>,
    pub(crate) connection_filter: ConnectionFilterKind,
    pub(crate) show_recorded_connections: bool,
    pub(crate) connections_limit: usize,
    pub(crate) selected_connection: Option<xrose_core::runtime::ClashConnection>,
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
}

impl AppView {
    pub fn new(service: AppService, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let snapshot = service.snapshot();
        let mixed_port = snapshot.settings.mixed_port.to_string();
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
        let lan_port_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(snapshot.settings.lan_port.to_string())
        });
        let lan_user_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(snapshot.settings.lan_account.username.clone())
        });
        let lan_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(snapshot.settings.lan_account.password.clone())
        });
        let update_agent_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(snapshot.settings.update_user_agent.clone())
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

        let initial_page = match std::env::var("XROSE_PAGE").unwrap_or_default().as_str() {
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
        let show_manual_form = std::env::var_os("XROSE_MANUAL").is_some();
        let hide_ips = snapshot.settings.privacy_mode;
        let traffic_ui = traffic_statistics::TrafficUi::new(window, cx);
        let view = Self {
            traffic_ui,
            mesh_form: mesh::MeshForm::new(window, cx),
            forward_editor: None,
            mesh_sync_ui: mesh_sync::SyncUi::new(window, cx),
            mesh_probe_feedback: mesh::ProbeFeedback::default(),
            mesh_expanded: std::collections::HashSet::new(),
            collapsed_mesh_devices: std::collections::HashSet::new(),
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
            hide_traffic_chart: false,
            latency_pending: std::collections::HashSet::new(),
            sort_by_latency: false,
            group_by_subscription: false,
            collapsed_subs: std::collections::HashSet::new(),
            batch_mode: false,
            batch_selected: std::collections::HashSet::new(),
            sub_name,
            sub_url,
            import_input,
            log_search,
            log_list: logs::LogListState::new(),
            log_filter: LogFilter::default(),
            rule_name,
            rule_group_id: None,
            route_drag_epoch: 0,
            route_scroll_running: false,
            action_menu: None,
            active_route_drag: None,
            content_scroll: ScrollHandle::new(),
            settings_tab: settings::SettingsTab::default(),
            settings_scroll: ScrollHandle::new(),
            settings_tab_scroll: ScrollHandle::new(),
            settings_tab_focus: std::array::from_fn(|_| cx.focus_handle()),
            rule_value,
            dns_tag,
            dns_address,
            dns_domains,
            host_domain,
            host_ips,
            port_input,
            network_advanced_open: false,
            lan_port_input,
            lan_user_input,
            lan_password_input,
            update_agent_input,
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
        if let Some(node) = self.snapshot.selected_node(None) {
            self.service.request_node_location(&node.id);
        }
    }

    /// Poll state without starving live connections while a trace is active.
    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
        self.statistics_tick(cx);
        if !self.auto_trace_started {
            self.auto_trace_started = true;
            if let Ok(target) = std::env::var("XROSE_TRACE") {
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
            if !self.snapshot.busy.iter().any(|key| key == "mesh") {
                self.mesh_probe_feedback.observe(&self.snapshot.mesh, now);
            }
            if !self.snapshot.busy.iter().any(|b| b == "latency") {
                self.latency_pending.clear();
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
                            .child("Xrose"),
                    ),
            )
            .child(div().flex().flex_col().gap_1().p_2().flex_1().children(
                Page::all().into_iter().map(|page| {
                    let active = self.page == page;
                    let has_update =
                        page == Page::Settings && self.snapshot.settings_update_available();
                    div().child(
                        Button::new(SharedString::from(format!("nav-{page:?}")))
                            .ghost()
                            .selected(active)
                            .w_full()
                            .h(px(40.))
                            .justify_start()
                            .accessibility_label(if has_update {
                                tr(lang, "设置，有更新", "Settings, update available")
                            } else {
                                tr(lang, page.zh(), page.en())
                            })
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(Icon::new(page.icon()).size(px(18.)))
                                    .child(tr(lang, page.zh(), page.en()))
                                    .when(has_update, |d| {
                                        d.child(div().flex_1()).child(pill(
                                            tr(lang, "更新", "Update"),
                                            p.info,
                                            p.info.opacity(0.1),
                                        ))
                                    }),
                            )
                            .text_color(if active { p.primary } else { p.muted_fg })
                            .when(active, |b| b.bg(p.accent))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.route_drag_epoch += 1;
                                this.dismiss_action_menu(cx);
                                this.page = page;
                                cx.notify();
                            })),
                    )
                }),
            ))
            .child(self.sidebar_traffic(cx))
            .child({
                let about_active = self.page == Page::About;
                div().p_2().child(
                    Button::new("nav-About")
                        .ghost()
                        .w_full()
                        .h(px(36.))
                        .selected(about_active)
                        .accessibility_label(self.tr("关于", "About"))
                        .when(about_active, |b| b.bg(p.accent))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.route_drag_epoch += 1;
                            this.dismiss_action_menu(cx);
                            this.page = Page::About;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .w_full()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .child(Icon::new(IconName::Info).size(px(theme::ICON_SIZE)))
                                        .child(self.tr("关于", "About")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(format!("v{}", self.snapshot.app_version)),
                                ),
                        ),
                )
            })
    }

    fn sidebar_traffic(&self, cx: &App) -> impl IntoElement {
        let p = palette(cx);
        let running = self.snapshot.process.phase.is_alive();
        let traffic_label = |symbol: &'static str, label: SharedString| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().w(px(12.)).flex_shrink_0().text_center().child(symbol))
                .child(label)
        };
        div()
            .id("sidebar-traffic")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .px_4()
            .py_2()
            .gap_1()
            .text_xs()
            .children(
                [
                    (
                        "↓",
                        self.tr("下行", "Download"),
                        self.snapshot.runtime.down_speed,
                        p.data_down,
                    ),
                    (
                        "↑",
                        self.tr("上行", "Upload"),
                        self.snapshot.runtime.up_speed,
                        p.data_up,
                    ),
                ]
                .into_iter()
                .map(|(symbol, label, speed, color)| {
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_color(if running { color } else { p.muted_fg })
                        .child(traffic_label(symbol, label))
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(xrose_core::util::format_speed(speed)),
                        )
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_color(p.muted_fg)
                    .child(traffic_label("∑", self.tr("累计", "Total")))
                    .child(xrose_core::util::format_bytes(
                        self.snapshot
                            .runtime
                            .up_total
                            .saturating_add(self.snapshot.runtime.down_total),
                    )),
            )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let running = self.snapshot.process.phase.is_alive();
        let core_busy = self.snapshot.busy.iter().any(|b| b == "core");
        let failed = self.snapshot.process.phase == xrose_core::core::ProcessPhase::Failed;
        let (action_icon, action_label) = if running {
            (IconName::Square, self.tr("停止", "Stop"))
        } else if failed {
            (IconName::RotateCw, self.tr("重试", "Retry"))
        } else {
            (IconName::Play, self.tr("启动", "Start"))
        };
        let node = self.snapshot.selected_node(None);
        let node_name = node
            .map(|n| {
                if self.hide_ips && !n.server.is_empty() {
                    n.name.replace(&n.server, "•••")
                } else {
                    n.name.clone()
                }
            })
            .unwrap_or_else(|| self.tr("选择节点", "Select node").to_string());
        let state_label = match self.snapshot.process.phase {
            xrose_core::core::ProcessPhase::Stopped => self.tr("已停止", "Stopped"),
            xrose_core::core::ProcessPhase::Starting => self.tr("启动中", "Starting"),
            xrose_core::core::ProcessPhase::Running => self.tr("运行中", "Running"),
            xrose_core::core::ProcessPhase::Stopping => self.tr("停止中", "Stopping"),
            xrose_core::core::ProcessPhase::Failed => self.tr("启动失败", "Failed"),
        };
        let mut node_tip = format!("{node_name} · {state_label}");
        let latency = node.and_then(|n| self.snapshot.latency.get(&n.id));
        let testing = node.is_some_and(|n| self.latency_pending.contains(&n.id))
            && self.snapshot.busy.iter().any(|key| key == "latency");
        let node_delay = theme::latency_label(latency, testing, self.lang());
        if let Some(entry) = node.and_then(|n| self.snapshot.latency.get(&n.id)) {
            if let Some(ms) = entry.delay_ms {
                node_tip.push_str(&format!(" · {ms} ms"));
            }
        }
        if running && !self.hide_ips {
            if let Some(label) = node
                .and_then(|n| self.snapshot.node_location(n))
                .and_then(|l| l.label.as_ref())
            {
                node_tip.push_str(&format!("\n{label}"));
            }
            if !self.snapshot.active_proxy_addresses.is_empty() {
                node_tip.push_str(&format!(
                    "\n{}: {}",
                    self.tr("代理地址", "Proxy"),
                    self.snapshot.active_proxy_addresses.join(", ")
                ));
            }
            if let Some(interface) = &self.snapshot.active_tun_interface {
                node_tip.push_str(&format!(
                    "\n{}: {interface}",
                    self.tr("TUN 网卡", "TUN interface")
                ));
            }
        }
        let drag_region = |id| {
            div()
                .id(id)
                .flex_1()
                .min_w(px(theme::HEADER_DRAG_GAP))
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
                })
        };
        div()
            .id("header-bar")
            .h(px(theme::HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(p.border)
            .bg(if p.is_dark { p.bg } else { p.panel })
            .child(drag_region("header-drag-left"))
            .child(
                div()
                    .id("header-status")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .h(px(theme::STATUS_HEIGHT))
                    .rounded_full()
                    .overflow_hidden()
                    .bg(p.panel)
                    .border_1()
                    .border_color(p.border)
                    .child(
                        Button::new("header-node-selector")
                            .compact()
                            .ghost()
                            // The dropdown wrapper sizes to its trigger, so give
                            // the button the capsule's full inner height explicitly.
                            .h(px(theme::STATUS_HEIGHT - 2.))
                            .w(px(theme::STATUS_NODE_WIDTH))
                            .rounded(gpui_kit::component::button::ButtonRounded::None)
                            .rounded_tl(px(theme::STATUS_END_RADIUS))
                            .rounded_bl(px(theme::STATUS_END_RADIUS))
                            .disabled(core_busy)
                            .accessibility_label(format!(
                                "{}: {node_name}, {node_delay}",
                                self.tr("切换节点", "Switch node")
                            ))
                            .tooltip(node_tip)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .w_full()
                                    .min_w(px(0.))
                                    .child(if core_busy {
                                        Spinner::new().into_any_element()
                                    } else {
                                        dot(if failed {
                                            p.danger_text
                                        } else if running {
                                            p.ok_text
                                        } else {
                                            p.muted_fg
                                        })
                                        .into_any_element()
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_xs()
                                            .child(node_name),
                                    )
                                    .when(node.is_some(), |d| {
                                        d.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_xs()
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_color(if testing {
                                                    p.muted_fg
                                                } else {
                                                    theme::latency_color(&p, latency)
                                                })
                                                .when(
                                                    latency.is_some_and(|entry| {
                                                        theme::latency_age_secs(entry)
                                                            > theme::LATENCY_STALE_SECS
                                                    }),
                                                    |d| d.opacity(theme::DISABLED_OPACITY),
                                                )
                                                .child(node_delay),
                                        )
                                    })
                                    .child(
                                        Icon::new(IconName::ChevronDown)
                                            .size(px(theme::ICON_SIZE))
                                            .flex_shrink_0(),
                                    ),
                            )
                            .dropdown_menu(menus::builder(
                                cx.entity().downgrade(),
                                menus::MenuTarget::NodeSelector,
                            )),
                    )
                    .child(div().w(px(1.)).h_full().flex_shrink_0().bg(p.border))
                    .child(
                        Button::new("toggle-core")
                            .h_full()
                            .w(px(theme::CONTROL_HEIGHT))
                            .p_0()
                            .flex_shrink_0()
                            .rounded(gpui_kit::component::button::ButtonRounded::None)
                            .rounded_tr(px(theme::STATUS_END_RADIUS))
                            .rounded_br(px(theme::STATUS_END_RADIUS))
                            .custom(if running {
                                danger_button_style(&p, cx)
                            } else {
                                ButtonCustomVariant::new(cx)
                                    .foreground(p.primary)
                                    .hover(p.accent)
                                    .active(p.accent)
                            })
                            .icon(action_icon)
                            .tooltip(action_label.clone())
                            .accessibility_label(action_label)
                            .disabled(core_busy)
                            .on_click(cx.listener(|this, _, _, _| this.service.toggle_core())),
                    ),
            )
            .child(
                // Keep both halves equal so the pill stays centered. Windows
                // uses the first matching control hitbox, so the drag region
                // and caption buttons must occupy separate layout space.
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .child(drag_region("header-drag-right"))
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
                            .child(if self.snapshot.process.phase.is_alive() {
                                self.tr(
                                    "有未应用的配置变更，需重载 Core 生效",
                                    "Unapplied config changes. Reload Core to apply.",
                                )
                            } else {
                                self.tr(
                                    "配置变更已保存，启动 Core 时生效",
                                    "Changes saved. They take effect when Core starts.",
                                )
                            }),
                    ),
            )
            .child(
                Button::new("apply-dirty-config")
                    .compact()
                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                    .primary()
                    .label(if self.snapshot.process.phase.is_alive() {
                        self.tr("立即应用并重载", "Apply & Reload")
                    } else {
                        self.tr("校验配置", "Validate configuration")
                    })
                    .loading(self.snapshot.busy.iter().any(|key| key == "core"))
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
            Page::Settings => self.page_settings(cx).into_any_element(),
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
                            xrose_core::service::ToastLevel::Info => p.info,
                            xrose_core::service::ToastLevel::Success => p.success,
                            xrose_core::service::ToastLevel::Warning => p.warning,
                            xrose_core::service::ToastLevel::Error => p.danger,
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
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
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

    fn traffic_chart(&self, p: &Palette) -> impl IntoElement {
        let now = chrono::Utc::now().timestamp_millis();
        let samples: Vec<_> = self
            .snapshot
            .runtime
            .history
            .iter()
            .filter(|s| (0..=theme::TRAFFIC_WINDOW_MS).contains(&now.saturating_sub(s.at_ms)))
            .cloned()
            .collect();
        let peak = samples
            .iter()
            .map(|s| s.up_speed.max(s.down_speed))
            .max()
            .unwrap_or(0);
        let down_color = p.data_down;
        let up_color = p.data_up;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(self.tr("近 60 秒", "Last 60 seconds"))
                    .child(
                        div()
                            .text_color(p.data_down)
                            .child(self.tr("↓ 下行 · 实线", "↓ Download · solid")),
                    )
                    .child(
                        div()
                            .text_color(p.data_up)
                            .child(self.tr("↑ 上行 · 虚线", "↑ Upload · dashed")),
                    )
                    .child(format!(
                        "{} {}",
                        self.tr("单方向峰值", "Peak per direction"),
                        xrose_core::util::format_speed(peak)
                    )),
            )
            .child(
                div()
                    .h(px(theme::TRAFFIC_HEIGHT))
                    .w_full()
                    .border_b_1()
                    .border_color(p.muted_fg.opacity(theme::DISABLED_OPACITY))
                    .child(
                        canvas(
                            move |_, _, _| (),
                            move |bounds, _, window, _| {
                                if peak == 0 {
                                    return;
                                }
                                for (upload, color) in [(false, down_color), (true, up_color)] {
                                    let mut path = PathBuilder::stroke(px(theme::TRAFFIC_STROKE));
                                    if upload {
                                        path = path.dash_array(&[
                                            px(theme::TRAFFIC_DASH),
                                            px(theme::TRAFFIC_DASH),
                                        ]);
                                    }
                                    let inset = px(theme::TRAFFIC_STROKE / 2.);
                                    let width = (bounds.size.width - inset * 2.).max(px(0.));
                                    let height = (bounds.size.height - inset * 2.).max(px(0.));
                                    for (index, sample) in samples.iter().enumerate() {
                                        let value = if upload {
                                            sample.up_speed
                                        } else {
                                            sample.down_speed
                                        };
                                        let Some((x, y)) =
                                            traffic_position(sample.at_ms, value, now, peak)
                                        else {
                                            continue;
                                        };
                                        let point = point(
                                            bounds.left() + inset + width * x,
                                            bounds.top() + inset + height * y,
                                        );
                                        if index == 0 {
                                            path.move_to(point);
                                        } else {
                                            path.line_to(point);
                                        }
                                    }
                                    if let Ok(path) = path.build() {
                                        window.paint_path(path, color);
                                    }
                                }
                            },
                        )
                        .w_full()
                        .h_full(),
                    ),
            )
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let mods = event.keystroke.modifiers;
                let key = event.keystroke.key.as_str();
                if key == "escape" && cx.stop_active_drag(window) {
                    this.active_route_drag = None;
                    this.route_drag_epoch += 1;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if mods.platform
                    && matches!(key, "," | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8")
                {
                    this.route_drag_epoch += 1;
                    this.dismiss_action_menu(cx);
                }
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
                                .test_latency(xrose_core::service::LatencyScope::All);
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
                    .min_w(px(0.))
                    .bg(p.bg)
                    .overflow_hidden()
                    .child(self.header(cx))
                    .when(
                        self.snapshot.config_dirty
                            && !(self.snapshot.settings.hot_reload
                                && (self.snapshot.auto_reload_pending
                                    || self.snapshot.busy.iter().any(|key| key == "core"))),
                        |d| d.child(self.config_dirty_banner(cx)),
                    )
                    .child(
                        div()
                            .id("content-scroll")
                            .track_scroll(&self.content_scroll)
                            .on_drag_move(cx.listener(
                                |this, event: &DragMoveEvent<route_drag::RouteDrag>, window, cx| {
                                    if this.page != Page::Routing
                                        || !event.bounds.contains(&event.event.position)
                                    {
                                        return;
                                    }
                                    cx.notify();
                                    let y = event.event.position.y;
                                    let delta = if y < event.bounds.top() + px(theme::DRAG_EDGE) {
                                        theme::DRAG_SCROLL_STEP
                                    } else if y > event.bounds.bottom() - px(theme::DRAG_EDGE) {
                                        -theme::DRAG_SCROLL_STEP
                                    } else {
                                        0.
                                    };
                                    if delta != 0. && !this.route_scroll_running {
                                        this.route_scroll_running = true;
                                        route_drag::scroll_next_frame(
                                            cx.entity().downgrade(),
                                            window,
                                            cx,
                                        );
                                    }
                                    if delta != 0. {
                                        let mut offset = this.content_scroll.offset();
                                        offset.y += px(delta);
                                        this.content_scroll.set_offset(offset);
                                        cx.notify();
                                    }
                                },
                            ))
                            .flex_1()
                            .min_h(px(0.))
                            .w_full()
                            .when(matches!(self.page, Page::Logs | Page::Settings), |d| {
                                d.flex().flex_col().overflow_hidden()
                            })
                            .when(!matches!(self.page, Page::Logs | Page::Settings), |d| {
                                d.overflow_y_scroll()
                            })
                            .p(px(theme::PAGE_PAD))
                            .child(self.page_content(window, cx)),
                    ),
            )
            .child(self.toasts(cx))
    }
}

/// Map real timestamps into the fixed time window; zero traffic remains on the baseline.
fn traffic_position(at_ms: i64, value: u64, now: i64, peak: u64) -> Option<(f32, f32)> {
    let age = now.saturating_sub(at_ms);
    if !(0..=theme::TRAFFIC_WINDOW_MS).contains(&age) {
        return None;
    }
    let x = 1. - age as f32 / theme::TRAFFIC_WINDOW_MS as f32;
    let ratio = if peak == 0 {
        0.
    } else {
        (value as f32 / peak as f32).clamp(0., 1.)
    };
    Some((x, 1. - ratio))
}

#[cfg(test)]
mod traffic_tests {
    #[test]
    fn line_uses_real_time_and_preserves_small_values() {
        let now = 100_000;
        assert_eq!(
            super::traffic_position(40_000, 0, now, 1000),
            Some((0., 1.))
        );
        assert_eq!(
            super::traffic_position(100_000, 1000, now, 1000),
            Some((1., 0.))
        );
        let (x, y) = super::traffic_position(70_000, 1, now, 1000).unwrap();
        assert_eq!(x, 0.5);
        assert!(y < 1. && y > 0.99);
        assert_eq!(super::traffic_position(39_999, 1, now, 1000), None);
        assert_eq!(super::traffic_position(100_001, 1, now, 1000), None);
    }

    #[test]
    fn empty_and_zero_traffic_have_a_finite_baseline() {
        assert_eq!(super::traffic_position(1000, 0, 1000, 0), Some((1., 1.)));
        assert_eq!(
            super::traffic_position(1000, 200, 1000, 100),
            Some((1., 0.))
        );
    }
}
