mod about;
mod chain_editor;
mod config_import;
mod connections;
mod dialogs;
mod dns;
mod logs;
mod manual_node;
mod menus;
mod mesh;
mod mesh_sync;
mod network_diagnostics;
mod node_selector;
mod nodes;
mod process_icons;
mod qr;
mod route_drag;
mod route_transfer;
mod routing;
mod settings;
mod stun_diagnostics;
mod subscriptions;
pub(crate) mod theme;
mod title_controls;
mod traffic_popover;
mod traffic_statistics;
#[cfg(target_os = "linux")]
mod tray_state;
#[cfg(target_os = "linux")]
pub(crate) use tray_state::TrayWindowState;
pub(crate) mod widgets;

use std::time::Duration;

use gpui_base::{Disableable, Selectable};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::AppService;
use xrose_core::core::logs::LogFilter;
use xrose_core::diagnostics::DiagnosticRuleKind;
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
            Page::Mesh,
            Page::Subscriptions,
            Page::Routing,
            Page::Dns,
            Page::Connections,
            Page::Diagnostics,
            Page::Logs,
            Page::Settings,
        ]
    }

    fn from_shortcut(key: &str) -> Option<Self> {
        if key == "," {
            return Some(Self::Settings);
        }
        let [b @ b'1'..=b'9'] = key.as_bytes() else {
            return None;
        };
        Self::all().get((b - b'1') as usize).copied()
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
            Page::Diagnostics => "诊断",
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
    pub(crate) const fn all() -> [MatcherKind; 7] {
        [
            MatcherKind::Domain,
            MatcherKind::Suffix,
            MatcherKind::Geo,
            MatcherKind::RuleSet,
            MatcherKind::Ip,
            MatcherKind::Process,
            MatcherKind::Port,
        ]
    }

    pub(crate) fn label(&self, lang: Language) -> SharedString {
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

    pub(crate) const fn prefix(&self) -> &'static str {
        match self {
            MatcherKind::Domain => "domain:",
            MatcherKind::Suffix => "suffix:",
            MatcherKind::Geo => "geosite:",
            MatcherKind::RuleSet => "ruleset:",
            MatcherKind::Ip => "ip:",
            MatcherKind::Process => "process:",
            MatcherKind::Port => "port:",
        }
    }

    pub(crate) fn placeholder(&self, lang: Language) -> SharedString {
        let (zh, en) = match self {
            MatcherKind::Domain => (
                "example.com，多个用逗号分隔",
                "example.com, comma-separated",
            ),
            MatcherKind::Suffix => ("openai.com, google.com", "openai.com, google.com"),
            MatcherKind::Geo => ("cn, gfw, geolocation-!cn", "cn, gfw, geolocation-!cn"),
            MatcherKind::RuleSet => ("geosite-gfw, geoip-cn", "geosite-gfw, geoip-cn"),
            MatcherKind::Ip => ("10.0.0.0/8, 192.168.1.0/24", "10.0.0.0/8, 192.168.1.0/24"),
            MatcherKind::Process => ("chrome, telegram", "chrome, telegram"),
            MatcherKind::Port => ("443, 8000-8080", "443, 8000-8080"),
        };
        tr(lang, zh, en)
    }
}

fn tr(lang: Language, zh: &'static str, en: &'static str) -> SharedString {
    crate::i18n::tr(lang, zh, en)
}

pub(crate) fn traffic_label(icon: IconName, label: SharedString) -> Div {
    let slot = px(12.);
    // In the 24-unit SVGs, the arrows span 16 units including the stroke, and sigma spans 18.
    let size = if matches!(icon, IconName::Sigma) {
        slot * (16. / 18.)
    } else {
        slot
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .size(slot)
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(size)),
        )
        .child(label)
}

fn text_input(
    window: &mut Window,
    cx: &mut Context<AppView>,
    placeholder: &str,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()))
}

/// Scroll invalidation belongs to the page. Fixed navigation and window controls
/// can reuse their painted subtrees until application state or appearance changes.
#[derive(Clone, Copy)]
enum Region {
    Sidebar,
    Header,
    Content,
}

struct WindowRegion {
    app: WeakEntity<AppView>,
    region: Region,
}

impl WindowRegion {
    fn new(region: Region, cx: &mut Context<AppView>) -> Entity<Self> {
        let app = cx.entity();
        cx.new(|cx| {
            cx.observe(&app, |_, _, cx| cx.notify()).detach();
            Self {
                app: app.downgrade(),
                region,
            }
        })
    }
}

impl Render for WindowRegion {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.app
            .update(cx, |app, cx| match self.region {
                Region::Sidebar => app.sidebar(cx).into_any_element(),
                Region::Header => app.header(cx).into_any_element(),
                Region::Content => app.scroll_content(window, cx).into_any_element(),
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeSort {
    #[default]
    Order,
    Health,
    Latency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NodeView {
    #[default]
    Flat,
    Source,
    OutboundGroups,
}

pub struct AppView {
    scan_pending: bool,
    import_editor_epoch: u64,
    import_busy: bool,
    traffic_ui: traffic_statistics::TrafficUi,
    mesh_form: mesh::MeshForm,
    forward_editor: Option<mesh::ForwardEditor>,
    mesh_sync_ui: mesh_sync::SyncUi,
    config_import_ui: config_import::ConfigImportUi,
    mesh_probe_feedback: mesh::ProbeFeedback,
    mesh_expanded: std::collections::HashSet<String>,
    mesh_card_tabs: std::collections::HashMap<String, mesh::MeshCardTab>,
    collapsed_mesh_sections: std::collections::HashSet<String>,
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
    pub(crate) show_chain_form: bool,
    pub(crate) manual_form: ManualNodeForm,
    pub(crate) chain_form: chain_editor::ChainForm,
    pub(crate) manual_error: Option<String>,
    pub(crate) editing_node: Option<String>,
    pub(crate) hide_ips: bool,
    pub(crate) hide_traffic_chart: bool,
    /// Nodes of the latency run started from this window; they show a spinner
    /// instead of a stale value while the run is busy.
    pub(crate) latency_pending: std::collections::HashSet<String>,
    pub(crate) node_sort: NodeSort,
    pub(crate) node_view: NodeView,
    pub(crate) expanded_node: Option<String>,
    pub(crate) node_click_seq: u64,
    pub(crate) pre_click_expanded: Option<(String, Option<String>)>,
    pub(crate) collapsed_subs: std::collections::HashSet<String>,
    pub(crate) batch_mode: bool,
    pub(crate) batch_selected: std::collections::HashSet<String>,
    pub(crate) sub_name: Entity<InputState>,
    pub(crate) sub_url: Entity<InputState>,
    /// Tracks the last domain auto-filled into `sub_name` so repeated scans update it cleanly.
    pub(crate) sub_name_auto: Option<String>,
    /// URL associated with an inline subscription error so programmatic prefill does not clear it.
    pub(crate) sub_error_url: Option<String>,
    pub(crate) import_input: Entity<TextareaState>,
    pub(crate) log_search: Entity<InputState>,
    log_list: logs::LogListState,
    log_revision: u64,
    connection_list: ListState,
    log_filter: LogFilter,
    pub(crate) rule_name: Entity<InputState>,
    pub(crate) rule_search: Entity<InputState>,
    pub(crate) rule_group_inline_open: bool,
    pub(crate) rule_group_inline_error: Option<SharedString>,
    pub(crate) rule_group_name_input: Entity<InputState>,
    pub(crate) rule_group_id: Option<String>,
    pub(crate) rule_show_all: bool,
    route_drag_epoch: u64,
    route_scroll_running: bool,
    node_selector: Option<Entity<node_selector::NodeSelector>>,
    pub(crate) traffic_popover_open: bool,
    pub(crate) nodes_policy_popover_open: bool,
    action_menu: Option<WeakEntity<gpui_kit::component::menu::PopupMenu>>,
    active_route_drag: Option<route_drag::RouteDrag>,
    sidebar_view: Entity<WindowRegion>,
    header_view: Entity<WindowRegion>,
    content_view: Entity<WindowRegion>,
    content_scroll: ScrollHandle,
    trace_anchor: ScrollAnchor,
    inspector_anchor: ScrollAnchor,
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
    domestic_urltest_input: Entity<InputState>,
    overseas_urltest_input: Entity<InputState>,
    update_proxy_input: Entity<InputState>,
    update_agent_input: Entity<InputState>,
    pub(crate) trace_input: Entity<InputState>,
    pub(crate) connection_search: Entity<InputState>,
    pub(crate) connections_tab: connections::ConnectionsTab,
    connections_stats_scroll: ScrollHandle,
    connections_tab_scroll: ScrollHandle,
    connections_tab_focus: [FocusHandle; 5],
    process_icons: Option<process_icons::ProcessIcons>,
    connection_sort: xrose_core::flow::ConnectionSort,
    exact_process_filter: Option<String>,
    pub(crate) failed_filter: connections::FailedFilter,
    pub(crate) failed_selected: std::collections::HashSet<u64>,
    pub(crate) failed_granularity: xrose_core::failed_connections::DomainMatchGranularity,
    pub(crate) failed_batch_action: RouteActionKind,
    pub(crate) failed_batch_group: Option<String>,
    pub(crate) connections_limit: usize,
    pub(crate) selected_connection: Option<xrose_core::runtime::ClashConnection>,
    pub(crate) diagnostic_rule_kind: DiagnosticRuleKind,
    pub(crate) diagnostic_rule_action: RouteActionKind,
    pub(crate) diagnostic_rule_group: Option<String>,
    diagnostic_rule_trace_id: Option<u64>,
    pub(crate) expanded_trace_step: Option<TraceStepKind>,
    pub(crate) auto_trace_started: bool,
    last_refresh: std::time::Instant,
    snapshot_task: Option<Task<()>>,

    pub(crate) show_subscription_form: bool,
    pub(crate) show_rule_form: bool,
    pub(crate) show_dns_form: bool,
    pub(crate) show_host_form: bool,
    pub(crate) matcher_kind: MatcherKind,
    pub(crate) rule_action: RouteActionKind,
    pub(crate) rule_outbound_group: Option<String>,
    pub(crate) about_note_hovered: bool,
    hovered_toast: Option<u64>,
}

impl AppView {
    pub fn new(service: AppService, window: &mut Window, cx: &mut Context<Self>) -> Self {
        #[cfg(target_os = "linux")]
        crate::wayland::main_window::install(window, cx);
        let snapshot = service.snapshot();
        let log_revision = service.logs().revision();
        let mixed_port = snapshot.settings.mixed_port.to_string();
        let sub_name = text_input(window, cx, "留空自动使用订阅标题或域名");
        let sub_url = text_input(
            window,
            cx,
            "https://example.com/api/v1/client/subscribe?token=...",
        );
        cx.subscribe_in(&sub_name, window, |this, _, event, window, cx| {
            if !this.show_subscription_form {
                return;
            }
            match event {
                InputEvent::Change => {
                    let current_url = this.sub_url.read(cx).value();
                    if this.sub_error_url.as_deref() != Some(current_url.as_ref()) {
                        this.manual_error = None;
                        this.sub_error_url = None;
                    }
                    window.refresh();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit_subscription_form(window, cx);
                }
                _ => {}
            }
        })
        .detach();
        cx.subscribe_in(&sub_url, window, |this, _, event, window, cx| {
            if !this.show_subscription_form {
                return;
            }
            match event {
                InputEvent::Change => {
                    let current = this.sub_url.read(cx).value();
                    if this.sub_error_url.as_deref() != Some(current.as_ref()) {
                        this.manual_error = None;
                        this.sub_error_url = None;
                    }
                    window.refresh();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    this.submit_subscription_form(window, cx);
                }
                _ => {}
            }
        })
        .detach();
        let import_input = cx.new(|cx| TextareaState::new(window, cx).rows(4));
        cx.subscribe(&import_input, |this, _, event, cx| {
            if this.show_import_box && matches!(event, InputEvent::Change) {
                this.manual_error = None;
                cx.notify();
            }
        })
        .detach();
        let node_search = text_input(window, cx, "按名称或地址过滤节点…");
        let log_search = text_input(window, cx, "搜索日志…");
        let rule_name = text_input(window, cx, "规则名称");
        let rule_search = text_input(window, cx, "过滤规则…");
        cx.subscribe(&rule_search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let rule_group_name_input = text_input(window, cx, "新分组名称");
        cx.subscribe_in(
            &rule_group_name_input,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::Change => {
                    if this.rule_group_inline_error.is_some() {
                        this.rule_group_inline_error = None;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => {
                    this.submit_inline_rule_group(window, cx);
                }
                _ => {}
            },
        )
        .detach();
        let initial_matcher_kind = MatcherKind::Suffix;
        let rule_value = text_input(
            window,
            cx,
            &initial_matcher_kind.placeholder(snapshot.settings.language),
        );
        let dns_tag = text_input(window, cx, "标识，例如 remote");
        let dns_address = text_input(window, cx, "1.1.1.1 或 https://1.1.1.1/dns-query");
        let dns_domains = text_input(
            window,
            cx,
            "域名条件，例如 geosite:geolocation-!cn（逗号分隔）",
        );
        let host_domain = text_input(window, cx, "例如 example.com");
        let host_ips = text_input(window, cx, "1.2.3.4, 2001:db8::1");
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
        let update_proxy_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("socks5://host:port")
                .default_value(snapshot.settings.update_proxy.as_str().to_owned())
        });
        let update_agent_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(snapshot.settings.update_user_agent.clone())
        });
        let trace_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(
                    "https://example.com 或 tls://[ip]:443?sni=example.com / tcp://1.1.1.1:443 / udp://8.8.8.8:53",
                )
                .default_value("https://www.gstatic.com/generate_204")
        });
        let domestic_urltest_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(xrose_core::settings::DEFAULT_DOMESTIC_URLTEST_URL)
                .default_value(snapshot.settings.domestic_urltest_url.clone())
        });
        let overseas_urltest_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(xrose_core::settings::DEFAULT_OVERSEAS_URLTEST_URL)
                .default_value(snapshot.settings.urltest_url.clone())
        });
        for input in [&domestic_urltest_input, &overseas_urltest_input] {
            cx.subscribe(input, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }
        cx.subscribe(&trace_input, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let manual_form = ManualNodeForm::new(window, cx);
        let chain_form = chain_editor::ChainForm::new(window, cx);

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
        let content_scroll = ScrollHandle::new();
        let view = Self {
            traffic_ui,
            mesh_form: mesh::MeshForm::new(window, cx),
            forward_editor: None,
            mesh_sync_ui: mesh_sync::SyncUi::new(window, cx),
            config_import_ui: config_import::ConfigImportUi::default(),
            mesh_probe_feedback: mesh::ProbeFeedback::default(),
            mesh_expanded: std::collections::HashSet::new(),
            mesh_card_tabs: std::collections::HashMap::new(),
            collapsed_mesh_sections: std::collections::HashSet::new(),
            mesh_overflow: std::collections::HashMap::new(),
            snapshot,
            logo_badge: crate::brand_assets::logo_badge(),
            logo_hero: crate::brand_assets::logo_hero(),
            page: initial_page,
            service,
            focus_handle: cx.focus_handle(),
            scan_pending: false,
            import_editor_epoch: 0,
            import_busy: false,
            node_search,
            show_import_box: false,
            show_manual_form,
            show_chain_form: false,
            manual_form,
            chain_form,
            manual_error: None,
            editing_node: None,
            hide_ips,
            hide_traffic_chart: false,
            latency_pending: std::collections::HashSet::new(),
            node_sort: NodeSort::Order,
            node_view: NodeView::Flat,
            expanded_node: None,
            node_click_seq: 0,
            pre_click_expanded: None,
            collapsed_subs: std::collections::HashSet::new(),
            batch_mode: false,
            batch_selected: std::collections::HashSet::new(),
            sub_name,
            sub_url,
            sub_name_auto: None,
            sub_error_url: None,
            import_input,
            log_search,
            log_list: logs::LogListState::new(),
            log_revision,
            connection_list: ListState::new(0, ListAlignment::Top, px(80.))
                .with_uniform_item_height(px(100.)),
            log_filter: LogFilter::default(),
            rule_name,
            rule_search,
            rule_group_inline_open: false,
            rule_group_inline_error: None,
            rule_group_name_input,
            rule_group_id: None,
            rule_show_all: true,
            route_drag_epoch: 0,
            route_scroll_running: false,
            action_menu: None,
            node_selector: None,
            traffic_popover_open: false,
            nodes_policy_popover_open: false,
            about_note_hovered: false,
            active_route_drag: None,
            trace_anchor: ScrollAnchor::for_handle(content_scroll.clone()),
            inspector_anchor: ScrollAnchor::for_handle(content_scroll.clone()),
            sidebar_view: WindowRegion::new(Region::Sidebar, cx),
            header_view: WindowRegion::new(Region::Header, cx),
            content_view: WindowRegion::new(Region::Content, cx),
            content_scroll,
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
            domestic_urltest_input,
            overseas_urltest_input,
            update_proxy_input,
            update_agent_input,
            trace_input,
            connection_search: text_input(window, cx, "搜索域名、IP、端口、进程或规则…"),
            connections_tab: connections::ConnectionsTab::default(),
            connections_stats_scroll: ScrollHandle::new(),
            connections_tab_scroll: ScrollHandle::new(),
            connections_tab_focus: std::array::from_fn(|_| cx.focus_handle()),
            process_icons: None,
            connection_sort: Default::default(),
            exact_process_filter: None,
            failed_filter: connections::FailedFilter::default(),
            failed_selected: std::collections::HashSet::new(),
            failed_granularity:
                xrose_core::failed_connections::DomainMatchGranularity::RootDomainSuffix,
            failed_batch_action: RouteActionKind::Proxy,
            failed_batch_group: None,
            connections_limit: 50,
            selected_connection: None,
            diagnostic_rule_kind: DiagnosticRuleKind::Domain,
            diagnostic_rule_action: RouteActionKind::Proxy,
            diagnostic_rule_group: None,
            diagnostic_rule_trace_id: None,
            expanded_trace_step: None,
            auto_trace_started: false,
            last_refresh: std::time::Instant::now(),
            snapshot_task: None,
            show_subscription_form: false,
            show_rule_form: false,
            show_dns_form: false,
            show_host_form: false,
            matcher_kind: initial_matcher_kind,
            rule_action: RouteActionKind::Proxy,
            rule_outbound_group: None,
            hovered_toast: None,
        };
        view.init_import(window, cx);
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

    fn toggle_privacy_mode(&mut self, cx: &mut Context<Self>) {
        self.hide_ips = !self.hide_ips;
        let mut settings = self.snapshot.settings.clone();
        settings.privacy_mode = self.hide_ips;
        self.service.update_settings(settings);
        self.sync(cx);
    }

    pub(super) fn copy_proxy_env_commands(&self, cx: &mut Context<Self>) {
        match self.service.proxy_env_command() {
            Ok(command) => {
                cx.write_to_clipboard(ClipboardItem::new_string(command.text));
                let message = if command.requires_credentials {
                    self.tr(
                        "命令已复制，请将 USER 和 PASSWORD 替换为 URL 编码后的用户名和密码",
                        "Commands copied. Replace USER and PASSWORD with your URL-encoded credentials.",
                    )
                } else {
                    self.tr(
                        "代理环境变量命令已复制",
                        "Proxy environment commands copied",
                    )
                };
                self.service
                    .toast(xrose_core::service::ToastLevel::Success, message);
            }
            Err(error) => self
                .service
                .toast(xrose_core::service::ToastLevel::Error, error.to_string()),
        }
    }

    fn header_privacy_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let hide_ips = self.hide_ips;
        let tip = if hide_ips {
            self.tr(
                "隐私模式已开启：隐藏服务器地址、IP 与位置（点击关闭）",
                "Privacy mode on: hiding addresses, IPs, and locations (click to reveal)",
            )
        } else {
            self.tr(
                "隐私模式已关闭：当前显示真实地址与位置（点击开启）",
                "Privacy mode off: showing real addresses and locations (click to hide)",
            )
        };
        Button::new("header-privacy-toggle")
            .compact()
            .flex_shrink_0()
            .mr(px(theme::HEADER_PRIVACY_EDGE_GAP))
            .w(px(theme::ICON_BUTTON_SIZE))
            .h(px(theme::ICON_BUTTON_SIZE))
            .custom(
                ButtonCustomVariant::new(cx)
                    .foreground(if hide_ips { p.primary } else { p.muted_fg })
                    .hover(p.neutral_hover())
                    .active(p.neutral_active()),
            )
            .icon(if hide_ips {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .toggled(hide_ips)
            .accessibility_label(self.tr("隐私模式", "Privacy mode"))
            .tooltip(tip)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_privacy_mode(cx)))
    }

    fn lang(&self) -> Language {
        self.snapshot.settings.language
    }

    fn localize_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.tr("粘贴节点链接，每行一条", "Paste node links, one per line");
        self.import_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        for (input, placeholder) in [
            (
                &self.node_search,
                self.tr("按名称或地址过滤节点…", "Filter nodes by name or address…"),
            ),
            (&self.log_search, self.tr("搜索日志…", "Search logs…")),
            (
                &self.connection_search,
                self.connection_search_placeholder(),
            ),
            (
                &self.sub_name,
                self.tr("留空自动使用订阅标题或域名", "Optional display name"),
            ),
            (&self.rule_name, self.tr("规则名称", "Rule name")),
            (&self.rule_search, self.tr("过滤规则…", "Filter rules…")),
            (
                &self.rule_group_name_input,
                self.tr("新分组名称", "New group name"),
            ),
            (&self.rule_value, self.matcher_kind.placeholder(self.lang())),
            (
                &self.dns_tag,
                self.tr("标识，例如 remote", "Tag, e.g. remote"),
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
                &self.host_domain,
                self.tr("例如 example.com", "e.g. example.com"),
            ),
            (
                &self.host_ips,
                self.tr("1.2.3.4, 2001:db8::1", "1.2.3.4, 2001:db8::1"),
            ),
            (
                &self.trace_input,
                self.tr(
                    "https://example.com 或 tls://[ip]:443?sni=example.com / tcp://1.1.1.1:443 / udp://8.8.8.8:53",
                    "https://example.com or tls://[ip]:443?sni=example.com / tcp://1.1.1.1:443 / udp://8.8.8.8:53",
                ),
            ),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
        self.manual_form.localize_inputs(self.lang(), window, cx);
        self.chain_form.localize_inputs(self.lang(), window, cx);
    }

    fn sync_diagnostic_rule_defaults(&mut self) {
        let Some(trace) = self.snapshot.trace.as_ref() else {
            return;
        };
        if !matches!(
            trace.phase,
            xrose_core::trace::TracePhase::Done | xrose_core::trace::TracePhase::Failed
        ) || self.diagnostic_rule_trace_id == Some(trace.id)
        {
            return;
        }
        self.diagnostic_rule_trace_id = Some(trace.id);
        if trace.host.parse::<std::net::IpAddr>().is_ok() {
            self.diagnostic_rule_kind = DiagnosticRuleKind::Ip;
        } else {
            self.diagnostic_rule_kind = DiagnosticRuleKind::Domain;
        }
        if trace.success == Some(false) {
            self.diagnostic_rule_action = if trace.actual_direct == Some(false) {
                RouteActionKind::Direct
            } else {
                RouteActionKind::Proxy
            };
        }
    }

    fn refresh_snapshot(&mut self) {
        // An earlier poll must not overwrite the result of an explicit action.
        self.snapshot_task = None;
        if self.hovered_toast.is_some() {
            self.service.touch_toasts();
        }
        self.snapshot = self.service.snapshot();
        self.sync_diagnostic_rule_defaults();
        if let Some(id) = self.hovered_toast
            && !self
                .snapshot
                .toasts
                .iter()
                .any(|t| t.id == id && t.visible_in_app())
        {
            self.hovered_toast = None;
        }
        self.log_revision = self.service.logs().revision();
        self.last_refresh = std::time::Instant::now();
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        self.refresh_snapshot();
        self.refresh_header_location();
        cx.notify();
    }

    fn refresh_header_location(&self) {
        if let Some(node) = self.snapshot.selected_node(None) {
            self.service.request_node_location(&node.id);
        }
    }

    pub(crate) fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        self.route_drag_epoch += 1;
        self.dismiss_action_menu(cx);
        self.page = page;
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn open_updates_settings(&mut self, cx: &mut Context<Self>) {
        self.navigate(Page::Settings, cx);
        self.settings_tab = settings::SettingsTab::Updates;
        self.settings_scroll.set_offset(point(px(0.), px(0.)));
        self.settings_tab_scroll
            .scroll_to_item(settings::SettingsTab::Updates as usize);
        cx.notify();
    }

    pub(crate) fn open_failed_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Page::Connections, cx);
        self.select_connections_tab(connections::ConnectionsTab::Failed, window, cx);
    }

    /// Poll state without starving live connections while a trace is active.
    pub(crate) fn tick(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Connections
            && let Some(icons) = &mut self.process_icons
        {
            icons.load_visible(cx);
        }
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
        self.sync_diagnostic_rule_defaults();
        let refresh_due = now.duration_since(self.last_refresh) >= Duration::from_millis(400);
        if refresh_due && self.snapshot_task.is_none() {
            self.last_refresh = now;
            let service = self.service.clone();
            let toast_hovered = self.hovered_toast.is_some();
            let read = cx.background_executor().spawn(async move {
                if toast_hovered {
                    service.touch_toasts();
                }
                service.snapshot()
            });
            self.snapshot_task = Some(cx.spawn(async move |view, cx| {
                let snapshot = read.await;
                let _ = view.update(cx, |view, cx| {
                    view.snapshot_task = None;
                    view.snapshot = snapshot;
                    view.sync_diagnostic_rule_defaults();
                    if let Some(id) = view.hovered_toast
                        && !view
                            .snapshot
                            .toasts
                            .iter()
                            .any(|t| t.id == id && t.visible_in_app())
                    {
                        view.hovered_toast = None;
                    }
                    view.log_revision = view.service.logs().revision();
                    if !view.snapshot.busy.iter().any(|key| key == "mesh") {
                        view.mesh_probe_feedback
                            .observe(&view.snapshot.mesh, std::time::Instant::now());
                    }
                    if !view.snapshot.busy.iter().any(|key| key == "latency") {
                        view.latency_pending.clear();
                    }
                    view.refresh_header_location();
                    cx.notify();
                });
            }));
        }
        if changed {
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
            .w(px(theme::SIDEBAR_WIDTH))
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
                    let failed_count = if page == Page::Connections {
                        self.snapshot.failed_connections.unfixed_count
                    } else {
                        0
                    };
                    let failed_only_stalled = self.snapshot.failed_connections.unfixed_only_stalled;
                    div().child(
                        Button::new(SharedString::from(format!("nav-{page:?}")))
                            .ghost()
                            .selected(active)
                            .w_full()
                            .h(px(40.))
                            .justify_start()
                            .accessibility_label(if has_update {
                                tr(lang, "设置，有更新", "Settings, update available")
                            } else if failed_count > 0 {
                                SharedString::from(match lang {
                                    Language::Zh => {
                                        format!("连接，{failed_count} 条近期失败")
                                    }
                                    Language::En => {
                                        format!("Connections, {failed_count} recent failed")
                                    }
                                })
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
                                    })
                                    .when(failed_count > 0, |d| {
                                        d.child(div().flex_1()).child(dot(if failed_only_stalled {
                                            p.warning
                                        } else {
                                            p.danger
                                        }))
                                    }),
                            )
                            .text_color(if active { p.primary } else { p.muted_fg })
                            .when(active, |b| b.bg(p.accent))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate(page, cx);
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
                            this.navigate(Page::About, cx);
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
                        IconName::ArrowDown,
                        self.tr("下行", "Download"),
                        self.snapshot.runtime.down_speed,
                        p.data_down,
                    ),
                    (
                        IconName::ArrowUp,
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
                    .child(traffic_label(IconName::Sigma, self.tr("累计", "Total")))
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
        let direct = self.snapshot.model.work_mode == xrose_core::model::WorkMode::Direct;
        let mesh = self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh;
        let suffixes = self.snapshot.mesh.magic_dns_suffixes();
        let node_name = if mesh {
            self.snapshot
                .model
                .mesh_mode
                .target
                .as_ref()
                .map(|target| {
                    let name = if self.hide_ips {
                        xrose_core::privacy::mask_server_in_label(
                            &target.name,
                            &target.address,
                            "•••",
                            &suffixes,
                        )
                    } else {
                        target.name.clone()
                    };
                    format!("{} · {name}", self.tr("组网", "Mesh"))
                })
                .unwrap_or_else(|| self.tr("选择组网出口", "Select mesh exit").to_string())
        } else {
            node.map(|n| nodes::display_node_name(n, self.hide_ips, &suffixes))
                .unwrap_or_else(|| {
                    if direct {
                        self.tr("本机直连", "Direct")
                    } else {
                        self.tr("选择节点", "Select node")
                    }
                    .to_string()
                })
        };
        let state_label = match self.snapshot.process.phase {
            xrose_core::core::ProcessPhase::Stopped => self.tr("已停止", "Stopped"),
            xrose_core::core::ProcessPhase::Starting => self.tr("启动中", "Starting"),
            xrose_core::core::ProcessPhase::Running => self.tr("运行中", "Running"),
            xrose_core::core::ProcessPhase::Stopping => self.tr("停止中", "Stopping"),
            xrose_core::core::ProcessPhase::Failed => self.tr("启动失败", "Failed"),
        };
        let mut node_tip = format!("{node_name} · {state_label}");
        let latency_key = if mesh {
            self.snapshot.model.mesh_mode.target.as_ref().map(|target| {
                xrose_core::service::HttpTestTarget::MeshExit {
                    profile: target.profile_id.clone(),
                    peer: target.peer_id.clone(),
                }
                .key()
            })
        } else if direct {
            Some(xrose_core::model::DIRECT_VIRTUAL_NODE_ID.to_owned())
        } else {
            node.map(|node| node.id.clone())
        };
        let latency = latency_key
            .as_ref()
            .and_then(|key| self.snapshot.latency.get(key))
            .filter(|entry| entry.is_http());
        let testing = latency_key.as_ref().is_some_and(|key| {
            self.snapshot
                .busy
                .iter()
                .any(|busy| busy == &format!("latency:{key}"))
        });
        let downloading = latency_key.as_ref().is_some_and(|key| {
            self.snapshot
                .busy
                .iter()
                .any(|busy| busy == &format!("download:{key}"))
        });
        let node_delay = theme::latency_label(latency, testing, self.lang());
        if let Some(ms) = latency.and_then(|entry| entry.delay_ms) {
            node_tip.push_str(&format!(" · {ms} ms"));
        }
        if running
            && !self.hide_ips
            && let Some(label) = node
                .and_then(|n| self.snapshot.node_location(n))
                .and_then(|l| l.label.as_ref())
        {
            node_tip.push_str(&format!("\n{label}"));
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
            .w_full()
            .h(px(theme::HEADER_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(p.hairline_border())
            .bg(p.header_bg())
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
                    .bg(p.header_pill_bg())
                    .border_1()
                    .border_color(p.control_border())
                    .child(
                        self.node_selector_popover(
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
                                .accessibility_label(if direct {
                                    node_name.clone()
                                } else {
                                    format!(
                                        "{}: {node_name}, {node_delay}",
                                        self.tr("切换节点", "Switch node")
                                    )
                                })
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
                                        .when(
                                            testing || downloading || node.is_some() || mesh,
                                            |d| {
                                                d.child(if testing || downloading {
                                                    div()
                                                        .flex_shrink_0()
                                                        .flex()
                                                        .items_center()
                                                        .child(
                                                            Spinner::new()
                                                                .small()
                                                                .color(p.muted_fg),
                                                        )
                                                        .into_any_element()
                                                } else {
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_xs()
                                                        .font_family(
                                                            cx.theme().mono_font_family.clone(),
                                                        )
                                                        .text_color(theme::latency_color(
                                                            &p, latency,
                                                        ))
                                                        .when(
                                                            latency.is_some_and(|entry| {
                                                                theme::latency_age_secs(entry)
                                                                    > theme::LATENCY_STALE_SECS
                                                            }),
                                                            |d| d.opacity(theme::DISABLED_OPACITY),
                                                        )
                                                        .child(node_delay)
                                                        .into_any_element()
                                                })
                                            },
                                        )
                                        .child(
                                            Icon::new(IconName::ChevronDown)
                                                .size(px(theme::ICON_SIZE))
                                                .flex_shrink_0(),
                                        ),
                                ),
                            cx,
                        ),
                    )
                    .child(
                        div()
                            .w(px(1.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(p.control_border()),
                    )
                    .child(self.traffic_mode_popover(cx))
                    .child(
                        div()
                            .w(px(1.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(p.control_border()),
                    )
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
                                start_ghost_button_style(&p, cx)
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
                    .items_center()
                    .child(drag_region("header-drag-right"))
                    .child(self.header_privacy_toggle(cx))
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
                                    "有未应用的配置变更，需重载内核生效",
                                    "Unapplied config changes. Reload Core to apply.",
                                )
                            } else {
                                self.tr(
                                    "配置变更已保存，启动内核时生效",
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

    fn scroll_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page_scrolls = !matches!(self.page, Page::Connections | Page::Logs | Page::Settings);
        let viewport = div()
            .id("content-scroll")
            .track_scroll(&self.content_scroll)
            .on_drag_move(cx.listener(
                |this, event: &DragMoveEvent<route_drag::RouteDrag>, window, cx| {
                    if this.page != Page::Routing || !event.bounds.contains(&event.event.position) {
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
                        route_drag::scroll_next_frame(cx.entity().downgrade(), window, cx);
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
            .when(!page_scrolls, |d| d.flex().flex_col().overflow_hidden())
            .when(page_scrolls, |d| d.overflow_y_scroll())
            .p(px(theme::PAGE_PAD))
            .child(self.page_content(window, cx));
        match self.page {
            Page::Connections | Page::Logs | Page::Settings => viewport.into_any_element(),
            _ => scroll_frame("page-scrollbar", &self.content_scroll, viewport).into_any_element(),
        }
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
        let visible: Vec<_> = self
            .snapshot
            .toasts
            .iter()
            .rev()
            .filter(|toast| toast.visible_in_app())
            .take(3)
            .collect();
        if visible.is_empty() {
            return div().into_any_element();
        }
        div()
            .absolute()
            .right(px(18.))
            .bottom(px(18.))
            .w(px(420.))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(visible.into_iter().map(|toast| {
                let color = match toast.level {
                    xrose_core::service::ToastLevel::Info => p.info,
                    xrose_core::service::ToastLevel::Success => p.success,
                    xrose_core::service::ToastLevel::Warning => p.warning,
                    xrose_core::service::ToastLevel::Error => p.danger,
                };
                let id = toast.id;
                div()
                    .id(SharedString::from(format!("toast-card-{id}")))
                    .block_mouse_except_scroll()
                    .on_hover(cx.listener(move |this, hovered, _, _| {
                        if *hovered {
                            this.hovered_toast = Some(id);
                            this.service.touch_toasts();
                        } else if this.hovered_toast == Some(id) {
                            this.hovered_toast = None;
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                            .custom(icon_button_style(&p, false, cx))
                            .icon(IconName::X)
                            .flex_shrink_0()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if this.hovered_toast == Some(id) {
                                    this.hovered_toast = None;
                                }
                                this.service.dismiss_toast(id);
                                this.sync(cx);
                            })),
                    )
            }))
            .into_any_element()
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
                        traffic_label(
                            IconName::ArrowDown,
                            self.tr("下行 · 实线", "Download · solid"),
                        )
                        .text_color(p.data_down),
                    )
                    .child(
                        traffic_label(IconName::ArrowUp, self.tr("上行 · 虚线", "Upload · dashed"))
                            .text_color(p.data_up),
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

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        #[cfg(target_os = "macos")]
        qr::enable_drop(self.accepts_import(window, cx));
        // GPUI replays paint and mouse listeners from cached views, but native
        // caption hitboxes and the active accessibility tree are built per frame.
        let chrome = |view: &Entity<WindowRegion>, style| {
            if cfg!(target_os = "linux") && !window.is_a11y_active() {
                view.clone().cached(style)
            } else {
                view.clone().into_element()
            }
        };
        let import_page = matches!(self.page, Page::Nodes | Page::Subscriptions);
        div()
            .track_focus(&self.focus_handle)
            .when(import_page, |el| {
                el.on_drop(cx.listener(Self::drop_files))
                    .on_drop(cx.listener(Self::drop_content))
            })
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let mods = event.keystroke.modifiers;
                let key = event.keystroke.key.as_str();
                if key == "v"
                    && !mods.alt
                    && !mods.shift
                    && (mods.platform || mods.control)
                    && this.paste_import(window, cx)
                {
                    cx.stop_propagation();
                    return;
                }
                if key == "escape" && cx.stop_active_drag(window) {
                    this.active_route_drag = None;
                    this.route_drag_epoch += 1;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                // Development shortcut for repeated flyout lifecycle checks.
                #[cfg(debug_assertions)]
                if (mods.control || mods.platform) && mods.shift && key == "t" {
                    crate::tray::queue_command(crate::tray::TrayCommand::TogglePopup);
                }
                if mods.platform {
                    if let Some(page) = Page::from_shortcut(key) {
                        this.navigate(page, cx);
                    } else if key == "r" {
                        this.service.test_all_http_latency();
                        cx.notify();
                    } else if key == "q" {
                        crate::tray::queue_command(crate::tray::TrayCommand::Quit);
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
            .child(chrome(
                &self.sidebar_view,
                StyleRefinement::default()
                    .w(px(theme::SIDEBAR_WIDTH))
                    .h_full(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.))
                    .bg(p.content_bg())
                    .overflow_hidden()
                    .child(chrome(
                        &self.header_view,
                        StyleRefinement::default()
                            .h(px(theme::HEADER_HEIGHT))
                            .flex_shrink_0()
                            .w_full(),
                    ))
                    .when(
                        self.snapshot.config_dirty
                            && !(self.snapshot.settings.hot_reload
                                && (self.snapshot.auto_reload_pending
                                    || self.snapshot.busy.iter().any(|key| key == "core"))),
                        |d| d.child(self.config_dirty_banner(cx)),
                    )
                    .child(self.content_view.clone()),
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

#[cfg(test)]
mod shortcut_tests {
    use super::Page;

    #[test]
    fn numeric_shortcuts_match_sidebar_order() {
        for (index, page) in Page::all().into_iter().enumerate() {
            let key = (index + 1).to_string();
            assert_eq!(Page::from_shortcut(&key), Some(page));
        }
        assert_eq!(Page::from_shortcut("2"), Some(Page::Mesh));
        assert_eq!(Page::from_shortcut("9"), Some(Page::Settings));
        assert_eq!(Page::from_shortcut(","), Some(Page::Settings));
        assert_eq!(Page::from_shortcut("0"), None);
        assert_eq!(Page::from_shortcut("a"), None);
    }
}
