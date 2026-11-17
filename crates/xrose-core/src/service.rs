//! The single facade the UI talks to. All heavy work is dispatched to an
//! internal Tokio runtime; the UI polls [`UiSnapshot`] and issues plain calls.

mod http_tests;
mod mesh_discovery;
mod mesh_exit;
pub use http_tests::HttpTestTarget;
mod mesh_gateway;
mod mesh_mode;
#[cfg(test)]
mod mesh_mode_tests;
mod mesh_reconnect;
pub mod mesh_sync;
mod network_diagnostics;
mod node_switch;
mod outbound_groups;
mod reload;
mod routing_transfer;
mod system_proxy;
mod traffic_statistics;
mod tray;
pub use tray::{TrayIndicator, TrayStatus};
mod update_check;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Once};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::adapters::{CompiledConfig, ValidationReport};
use crate::core::logs::{LogBus, LogEntry, LogFilter, LogStream};
use crate::core::logs::{
    LogPrivacy,
    redact::{LogRedactor, RedactionRules},
};
use crate::core::{BinaryRecord, CoreManager, ProcessPhase, ProcessState, StartOutcome};
use crate::diagnostics::{DiagnosticRecording, DiagnosticRuleKind, DiagnosticTarget};
use crate::dns::{self, DnsPreset};
use crate::error::{Error, Result};
use crate::model::{
    AppModel, CoreKind, InboundKind, Node, NodeDraft, Placement, RoutePreset, RouteRule, WorkMode,
};
use crate::paths::AppPaths;
use crate::runtime::location::{self, NodeLocation};
use crate::runtime::{ClashClient, ClashConnection, ClashProxy, TrafficHistory, TrafficSample};
use crate::settings::LogLevel;
use crate::settings::Settings;
use crate::storage::{LatencyEntry, ProfileMeta, Store};
use crate::subscription::{Subscription, SubscriptionUsage, parse_subscription, qr_ascii};
use crate::system::privilege::{self, HelperRequest, HelperStatus};
use crate::system::{self, DoctorCheck, PacServer, ProxyManager};
use crate::trace::{self, TraceSession};
use crate::traffic_statistics::CoreEpoch;
use crate::updates::{self, AppUpdate, RuleSetUpdate, UpdateCandidate};
use crate::util::now_unix;

// Pasted links have historically shared this synthetic source, not a node subscription.
const PASTED_SOURCE: &str = "import";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Summary of a direct import, including successful parts of a mixed batch.
#[derive(Default)]
pub struct SharedImport {
    pub nodes: usize,
    pub subscriptions: usize,
    pub duplicates: usize,
    pub unreadable: bool,
    pub errors: Vec<String>,
}

pub enum SharePreview {
    Node {
        name: String,
        protocol: &'static str,
        endpoint: String,
        count: usize,
    },
    Subscription {
        host: String,
    },
    Unsupported,
}

#[derive(Clone)]
pub enum ShareTarget {
    Node(String),
    Subscription(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Toast {
    pub id: u64,
    pub level: ToastLevel,
    pub message: String,
    pub at: i64,
    /// Retained in history for CLI errors, but excluded from the in-app overlay.
    #[serde(default)]
    pub system_notification: bool,
}

impl Toast {
    pub fn visible_in_app(&self, system_notifications: bool) -> bool {
        !system_notifications && !self.system_notification
    }
}

/// Native notification controls are independent of the UI's toast polling.
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    Configure { enabled: bool, user_initiated: bool },
    Show(Toast),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub proxy_backend: String,
    pub elevated: bool,
    pub helper_available: bool,
    pub helper_installed: bool,
    #[serde(default)]
    pub helper_needs_update: bool,
    pub helper_supported: bool,
    pub helper_detail: String,
    pub helper_path: Option<String>,
    #[serde(default)]
    pub helper_tun_protocol: u32,
    #[serde(default)]
    pub helper_running_tun_protocol: Option<u32>,
    #[serde(default)]
    pub mesh_protocol: u32,
    #[serde(default)]
    pub helper_running_mesh_protocol: Option<u32>,
    pub log_dir: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    #[serde(default)]
    pub cores: BTreeMap<String, CoreUpdateStatus>,
    pub app: Option<AppUpdate>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoreUpdateStatus {
    pub candidate: Option<UpdateCandidate>,
    pub error: Option<String>,
    pub checked_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeView {
    /// Default proxy node actually in use, including live selector changes.
    #[serde(default)]
    pub running_proxy_node: Option<String>,
    #[serde(default)]
    pub managed_nodes: BTreeMap<String, String>,
    pub connections: Arc<Vec<ClashConnection>>,
    #[serde(default)]
    pub live_traffic: Arc<crate::flow::LiveTraffic>,
    pub proxies: BTreeMap<String, ClashProxy>,
    pub up_total: u64,
    pub down_total: u64,
    pub up_speed: u64,
    pub down_speed: u64,
    pub history: Vec<TrafficSample>,
    pub error: Option<String>,
}

/// Everything the UI needs for one frame. Cloned at ~3 Hz by the view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSnapshot {
    #[serde(default)]
    pub mesh: crate::mesh::MeshSnapshot,
    #[serde(default)]
    pub mesh_reconnecting: BTreeSet<String>,
    #[serde(default)]
    pub mesh_connection_errors: BTreeMap<String, String>,
    #[serde(default)]
    pub mesh_sync: mesh_sync::SyncView,
    pub app_version: String,
    pub settings: Settings,
    pub model: AppModel,
    pub process: ProcessState,
    pub logs: Vec<LogEntry>,
    pub subscriptions: Vec<Subscription>,
    pub profiles: Vec<ProfileMeta>,
    pub active_profile: Option<String>,
    pub runtime: RuntimeView,
    #[serde(default)]
    pub traffic_statistics: Arc<crate::traffic_statistics::TrafficStatisticsView>,
    pub latency: BTreeMap<String, LatencyEntry>,
    #[serde(default)]
    pub downloads: BTreeMap<String, crate::runtime::probe::DownloadResult>,
    #[serde(default)]
    pub node_locations: BTreeMap<String, NodeLocation>,
    pub validation: Option<ValidationReport>,
    pub compiled_warnings: Vec<String>,
    pub compiled_json: Option<String>,
    pub binaries: Vec<BinaryRecord>,
    #[serde(default)]
    pub local_binaries: BTreeMap<String, crate::core::binary::LocalBinaryStatus>,
    pub toasts: Vec<Toast>,
    pub busy: Vec<String>,
    pub platform: PlatformInfo,
    pub doctor: Vec<DoctorCheck>,
    #[serde(skip)]
    pub network_report: Option<Arc<system::net_diag::NetworkReport>>,
    #[serde(skip)]
    pub dns_comparison: Option<Arc<dns::probe::Comparison>>,
    #[serde(default)]
    pub nat_probe: Option<system::nat::NatProbe>,
    pub updates: UpdateStatus,
    pub system_proxy_active: bool,
    #[serde(default)]
    pub system_proxy_status: Option<system::proxy::SystemProxyStatus>,
    pub pac_url: Option<String>,
    #[serde(default)]
    pub active_proxy_addresses: Vec<String>,
    #[serde(default)]
    pub active_tun_interface: Option<String>,
    pub config_dirty: bool,
    #[serde(default)]
    pub auto_reload_pending: bool,
    pub recovery_note: Option<String>,
    pub rule_set_updates: Vec<RuleSetUpdate>,
    #[serde(default)]
    pub route_import: Option<(String, crate::route::transfer::ImportSummary)>,
    pub trace: Option<TraceSession>,
    #[serde(default)]
    pub recording: DiagnosticRecording,
}

impl UiSnapshot {
    /// Resolve the display name and exact link together when a share action runs.
    pub fn share_link(&self, target: &ShareTarget) -> Option<(&str, String)> {
        match target {
            ShareTarget::Node(id) => {
                let node = self.model.node(id)?;
                Some((&node.name, crate::subscription::node_to_share_link(node)?))
            }
            ShareTarget::Subscription(id) => {
                let sub = self.subscriptions.iter().find(|sub| &sub.id == id)?;
                Some((&sub.name, sub.url.clone()))
            }
        }
    }

    pub fn core_binary_available(&self, core: CoreKind) -> bool {
        let local = self.local_binaries.get(core.as_str());
        if let Some(version) = self.settings.active_version(core) {
            return self.binaries.iter().any(|record| {
                record.core == core && record.matches_platform() && &record.version == version
            });
        }
        local.is_some_and(|status| status.available)
    }

    pub fn settings_update_available(&self) -> bool {
        self.platform.helper_needs_update
            || self.updates.app.is_some()
            || self
                .updates
                .cores
                .values()
                .any(|check| check.candidate.is_some())
    }

    pub fn dns_preset(&self) -> Option<DnsPreset> {
        dns::detect_preset(&self.model.dns)
    }

    pub fn dns_deletion_reason(&self, tag: &str) -> Option<&'static str> {
        dns::deletion_reason(&self.model.dns, tag)
    }

    pub fn rule_sets(&self) -> Vec<crate::model::RuleSet> {
        crate::adapters::downloadable_rule_sets(&self.model)
    }

    /// Resolve the same group as the node list, including live automatic choices.
    pub fn selected_node(&self, group_id: Option<&str>) -> Option<&Node> {
        if self.model.work_mode == WorkMode::Direct {
            return None;
        }
        if self.model.work_mode == WorkMode::Mesh {
            return self
                .model
                .mesh_mode
                .target
                .as_ref()
                .and_then(|target| self.model.node(&target.gateway_id));
        }
        let group = group_id.and_then(|id| self.model.group(id)).or_else(|| {
            self.model
                .default_group_id()
                .and_then(|id| self.model.group(&id))
        })?;
        if self.process.phase.is_alive() {
            if let Some(node) = self
                .runtime
                .managed_nodes
                .get(&group.id)
                .and_then(|id| self.model.node(id))
            {
                return Some(node);
            }
        }
        if self.process.phase.is_alive()
            && self.settings.core == CoreKind::SingBox
            && !group.managed()
            && !group.manually_selectable()
        {
            if let Some(tag) = self
                .runtime
                .proxies
                .get(&group.tag())
                .and_then(|p| p.now.as_ref())
            {
                if let Some(node) = self
                    .model
                    .nodes
                    .iter()
                    .find(|n| group.members.contains(&n.id) && n.outbound_tag() == *tag)
                {
                    return Some(node);
                }
            }
        }
        group.active_node().and_then(|id| self.model.node(id))
    }

    pub fn node_location(&self, node: &Node) -> Option<&NodeLocation> {
        self.node_locations
            .get(&location::cache_key(&node.server, self.settings.language))
    }
}

#[derive(Default)]
struct ConnectionState {
    rows: Arc<Vec<ClashConnection>>,
    live: Arc<crate::flow::LiveTraffic>,
}

impl ConnectionState {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

struct Inner {
    local_binaries: Arc<RwLock<BTreeMap<String, crate::core::binary::LocalBinaryStatus>>>,
    mesh_sync: mesh_sync::SyncState,
    mesh: crate::mesh::controller::MeshController,
    mesh_reconnect: Mutex<mesh_reconnect::ReconnectState>,
    mesh_connection_errors: RwLock<BTreeMap<String, String>>,
    paths: AppPaths,
    settings: RwLock<Settings>,
    model: RwLock<AppModel>,
    store: Arc<Store>,
    core: Arc<CoreManager>,
    mesh_client_profile: Mutex<Option<crate::mesh::MeshProfile>>,
    mesh_gateways: tokio::sync::Mutex<BTreeMap<String, mesh_gateway::ExitGateway>>,
    mesh_proxy_suspended: AtomicBool,
    mesh_takeover_requested: AtomicBool,
    exit_policy_revision: AtomicU64,
    rt: Runtime,
    shutdown: Once,
    subs: RwLock<Vec<Subscription>>,
    profiles: RwLock<Vec<ProfileMeta>>,
    active_profile: RwLock<Option<String>>,
    traffic: Mutex<TrafficHistory>,
    statistics: traffic_statistics::StatisticsState,
    connections: RwLock<ConnectionState>,
    rate_tracker: Mutex<(
        Option<crate::traffic_statistics::CoreEpoch>,
        crate::flow::FlowRateTracker,
    )>,
    proxies: RwLock<Arc<BTreeMap<String, ClashProxy>>>,
    latency: RwLock<BTreeMap<String, LatencyEntry>>,
    downloads: RwLock<BTreeMap<String, crate::runtime::probe::DownloadResult>>,
    node_locations: RwLock<BTreeMap<String, NodeLocation>>,
    busy: RwLock<BTreeSet<String>>,
    toasts: Mutex<Vec<Toast>>,
    proxy: Mutex<ProxyManager>,
    proxy_backend: system::ProxyBackend,
    #[cfg(target_os = "macos")]
    proxy_observation: Arc<RwLock<system::proxy::observer::Observation>>,
    proxy_requests: Mutex<system_proxy::Requests>,
    pac: Mutex<Option<PacServer>>,
    validation: RwLock<Option<ValidationReport>>,
    compiled: RwLock<Option<CompiledConfig>>,
    compiled_json: RwLock<Option<String>>,
    reload: Mutex<reload::ReloadState>,
    reload_wake: tokio::sync::watch::Sender<()>,
    helper_update_attempted: AtomicBool,
    updates: Mutex<UpdateStatus>,
    doctor: RwLock<Vec<DoctorCheck>>,
    network_report: RwLock<Option<Arc<system::net_diag::NetworkReport>>>,
    dns_comparison: RwLock<Option<Arc<dns::probe::Comparison>>>,
    nat_probe: RwLock<Option<system::nat::NatProbe>>,
    doctor_generation: AtomicU64,
    recovery_note: RwLock<Option<String>>,
    helper: RwLock<HelperStatus>,
    rule_set_updates: RwLock<BTreeMap<String, RuleSetUpdate>>,
    rule_set_lock: tokio::sync::Mutex<()>,
    route_import: Mutex<Option<routing_transfer::PendingImport>>,
    trace: RwLock<Option<TraceSession>>,
    trace_generation: AtomicU64,
    recording: Mutex<DiagnosticRecording>,
    toast_seq: AtomicU64,
    startup_started: AtomicBool,
    last_start_failed: AtomicBool,
    failed_proxy_probe: Mutex<Option<tray::ProbeTarget>>,
    notification_sender: RwLock<Option<std::sync::mpsc::Sender<NotificationEvent>>>,
    install_lock: tokio::sync::Mutex<()>,
    helper_lock: tokio::sync::Mutex<()>,
}

/// Cheap to clone; all state lives behind Arc.
#[derive(Clone)]
pub struct AppService {
    inner: Arc<Inner>,
}

impl AppService {
    pub fn new() -> Result<Self> {
        Self::with_paths(AppPaths::discover())
    }

    pub fn with_paths(paths: AppPaths) -> Result<Self> {
        paths.ensure()?;
        let store = Store::open(&paths)?;
        let settings = Settings::load(&paths.settings_file())?;
        if !paths.settings_file().exists() {
            let _ = settings.save(&paths.settings_file());
        }
        if std::env::var_os("XROSE_DEBUG").is_some() {
            eprintln!("[debug] settings loaded");
        }
        let active_profile = store.active_profile_id();
        let model = active_profile
            .as_deref()
            .and_then(|id| store.load_profile(id).ok().flatten())
            .map(|p| p.model)
            .unwrap_or_default();
        let logs = LogBus::with_file(
            paths.logs_dir(),
            "xrose",
            settings.log_max_bytes,
            settings.log_keep,
        );
        logs.load_tail(&paths.logs_dir().join("xrose.log"), 300);
        let core = Arc::new(CoreManager::new(paths.clone(), logs));
        if std::env::var_os("XROSE_DEBUG").is_some() {
            eprintln!(
                "[debug] profile {} model loaded",
                active_profile.as_deref().unwrap_or("-")
            );
        }
        let subs = store.subscriptions().unwrap_or_default();
        let profiles = store.profiles().unwrap_or_default();
        let latency = store
            .latency_cache()
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.key.clone(), e))
            .collect();
        let proxy = ProxyManager::new(paths.proxy_snapshot());
        if std::env::var_os("XROSE_DEBUG").is_some() {
            eprintln!("[debug] proxy backend detected");
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("xrose-worker")
            .build()
            .map_err(|e| Error::other(format!("无法创建异步运行时：{e}")))?;

        let mesh = crate::mesh::controller::MeshController::new(paths.clone(), store.clone())?;
        let mut model = model;
        settings.sync_inbounds(&mut model);
        mesh.sync_outbounds(&mut model);
        let statistics =
            traffic_statistics::StatisticsState::new(settings.traffic_statistics_enabled);
        let rule_set_updates = updates::cached_rule_set_updates(&paths.rule_sets_dir());
        let inner = Arc::new(Inner {
            local_binaries: Default::default(),
            statistics,
            mesh_sync: Default::default(),
            mesh_reconnect: Default::default(),
            mesh_connection_errors: Default::default(),
            mesh,
            paths,
            settings: RwLock::new(settings),
            model: RwLock::new(model),
            store,
            core,
            mesh_client_profile: Mutex::new(None),
            mesh_gateways: Default::default(),
            mesh_proxy_suspended: AtomicBool::new(false),
            mesh_takeover_requested: AtomicBool::new(false),
            exit_policy_revision: AtomicU64::new(0),
            rt,
            shutdown: Once::new(),
            subs: RwLock::new(subs),
            profiles: RwLock::new(profiles),
            active_profile: RwLock::new(active_profile),
            traffic: Mutex::new(TrafficHistory::new(180)),
            connections: Default::default(),
            rate_tracker: Default::default(),
            proxies: Default::default(),
            latency: RwLock::new(latency),
            downloads: RwLock::new(BTreeMap::new()),
            node_locations: RwLock::new(BTreeMap::new()),
            busy: RwLock::new(BTreeSet::new()),
            toasts: Mutex::new(Vec::new()),
            proxy_backend: proxy.backend(),
            #[cfg(target_os = "macos")]
            proxy_observation: Default::default(),
            proxy: Mutex::new(proxy),
            proxy_requests: Default::default(),
            pac: Mutex::new(None),
            validation: RwLock::new(None),
            compiled: RwLock::new(None),
            compiled_json: RwLock::new(None),
            reload: Mutex::default(),
            reload_wake: tokio::sync::watch::channel(()).0,
            helper_update_attempted: AtomicBool::new(false),
            updates: Mutex::new(UpdateStatus::default()),
            doctor: RwLock::new(Vec::new()),
            network_report: RwLock::new(None),
            dns_comparison: RwLock::new(None),
            nat_probe: RwLock::new(None),
            doctor_generation: AtomicU64::new(0),
            recovery_note: RwLock::new(None),
            helper: RwLock::new(privilege::initial_status()),
            rule_set_updates: RwLock::new(rule_set_updates),
            rule_set_lock: Default::default(),
            route_import: Mutex::new(None),
            trace: RwLock::new(None),
            trace_generation: AtomicU64::new(0),
            recording: Mutex::new(DiagnosticRecording::default()),
            toast_seq: AtomicU64::new(1),
            startup_started: AtomicBool::new(false),
            last_start_failed: AtomicBool::new(false),
            failed_proxy_probe: Mutex::new(None),
            notification_sender: RwLock::new(None),
            install_lock: tokio::sync::Mutex::new(()),
            helper_lock: tokio::sync::Mutex::new(()),
        });
        let service = Self {
            inner: inner.clone(),
        };

        if std::env::var_os("XROSE_DEBUG").is_some() {
            eprintln!("[debug] runtime ready");
        }
        service.refresh_doctor();
        if std::env::var_os("XROSE_DEBUG").is_some() {
            eprintln!("[debug] doctor done");
        }
        service.start_mesh_discovery();
        service.refresh_local_binaries();
        service.start_monitor();
        service.start_reload_worker();
        service.start_node_switch_scheduler();
        service.start_subscription_scheduler();
        Ok(service)
    }

    pub fn paths(&self) -> &AppPaths {
        &self.inner.paths
    }

    pub fn store(&self) -> Arc<Store> {
        self.inner.store.clone()
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.read().clone()
    }

    pub fn model(&self) -> AppModel {
        self.inner.model.read().clone()
    }

    pub fn process_state(&self) -> ProcessState {
        self.inner.core.state()
    }

    pub fn logs(&self) -> &LogBus {
        self.inner.core.logs()
    }

    pub fn is_running(&self) -> bool {
        self.inner.core.state().phase.is_alive()
    }

    pub fn core(&self) -> Arc<CoreManager> {
        self.inner.core.clone()
    }

    pub fn snapshot(&self) -> UiSnapshot {
        let (config_dirty, auto_reload_pending) = {
            let reload = self.inner.reload.lock();
            (reload.dirty, reload.due.is_some())
        };
        let settings = self.inner.settings.read().clone();
        let mut model = self.inner.model.read().clone();
        let mut mesh = self.inner.mesh.snapshot();
        self.annotate_peers(&mut mesh);
        let suffixes = mesh.magic_dns_suffixes();
        // Use the connected peer's name throughout the UI while leaving stored gateway IDs and names stable.
        for (profile, peer, _) in mesh.exits().filter(|(_, peer, _)| peer.exit_selected) {
            if let Some(node) = model.node_mut(&format!("mesh-exit-{}", profile.id))
                && !peer.name.is_empty()
            {
                let peer_name = if settings.privacy_mode {
                    crate::privacy::mask_magic_dns_name(&peer.name, &suffixes)
                } else {
                    peer.name.clone()
                };
                node.name = format!(
                    "{} · {} · {}",
                    peer_name,
                    profile.provider.label(),
                    profile.name
                );
            }
        }
        let process = self.inner.core.state();
        let (up_total, down_total, up_speed, down_speed) = self.inner.traffic.lock().snapshot();
        let history = self.inner.traffic.lock().samples();
        let pac_url = self.inner.pac.lock().as_ref().map(|p| p.info.url.clone());
        let proxy_backend = self.inner.proxy_backend.label().to_string();
        let doctor = self.inner.doctor.read().clone();
        let validation = self.inner.validation.read().clone();
        let helper = self.inner.helper.read().clone();
        let (connections, live_traffic) = {
            let state = self.inner.connections.read();
            (state.rows.clone(), state.live.clone())
        };
        let runtime = RuntimeView {
            running_proxy_node: self.running_proxy_node(process.phase),
            managed_nodes: if process.phase.is_alive() {
                self.inner.core.managed_selections()
            } else {
                Default::default()
            },
            connections,
            live_traffic,
            proxies: self.inner.proxies.read().as_ref().clone(),
            up_total,
            down_total,
            up_speed,
            down_speed,
            history,
            error: None,
        };
        let updates = self.inner.updates.lock().clone();
        let system_proxy_status = self.system_proxy_status();
        let system_proxy_active = if model.work_mode == WorkMode::Mesh {
            false
        } else {
            match system_proxy_status {
                Some(
                    system::proxy::SystemProxyStatus::Off
                    | system::proxy::SystemProxyStatus::External,
                ) => false,
                Some(
                    system::proxy::SystemProxyStatus::Xrose
                    | system::proxy::SystemProxyStatus::Partial,
                ) => true,
                _ => settings.system_proxy,
            }
        };
        UiSnapshot {
            nat_probe: self
                .inner
                .nat_probe
                .read()
                .as_ref()
                .map(|result| result.visible(settings.privacy_mode)),
            mesh_sync: self.sync_view(),
            mesh_reconnecting: self.inner.mesh_reconnect.lock().pending(&mesh),
            mesh_connection_errors: self.inner.mesh_connection_errors.read().clone(),
            mesh,
            app_version: APP_VERSION.into(),
            settings,
            model,
            process,
            logs: self.inner.core.logs().recent(800, LogLevel::Trace, ""),
            subscriptions: self.inner.subs.read().clone(),
            profiles: self.inner.profiles.read().clone(),
            active_profile: self.inner.active_profile.read().clone(),
            runtime,
            traffic_statistics: self.inner.statistics.view.read().clone(),
            latency: self.inner.latency.read().clone(),
            downloads: self.inner.downloads.read().clone(),
            node_locations: self.inner.node_locations.read().clone(),
            validation,
            compiled_warnings: self
                .inner
                .compiled
                .read()
                .as_ref()
                .map(|c| c.warnings.clone())
                .unwrap_or_default(),
            compiled_json: self.inner.compiled_json.read().clone(),
            binaries: self.inner.core.binaries().list(),
            local_binaries: self.inner.local_binaries.read().clone(),
            toasts: self.active_toasts(),
            busy: self.inner.busy.read().iter().cloned().collect(),
            platform: PlatformInfo {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                proxy_backend,
                elevated: helper.elevated,
                helper_available: helper.running,
                helper_installed: helper.installed,
                helper_needs_update: helper.needs_update,
                helper_supported: helper.supported,
                helper_detail: helper.detail.clone(),
                helper_path: helper.executable.clone(),
                helper_tun_protocol: system::tun_process::VERSION,
                helper_running_tun_protocol: helper.running_tun_protocol,
                mesh_protocol: crate::mesh::PROTOCOL_VERSION,
                helper_running_mesh_protocol: helper.running_mesh_protocol,
                log_dir: self.inner.paths.logs_dir().display().to_string(),
                data_dir: self.inner.paths.data_dir.display().to_string(),
            },
            doctor,
            network_report: self.inner.network_report.read().clone(),
            dns_comparison: self.inner.dns_comparison.read().clone(),
            updates,
            system_proxy_active,
            system_proxy_status,
            pac_url,
            active_proxy_addresses: self.inner.core.active_proxy_addresses(),
            active_tun_interface: self.inner.core.active_tun_interface(),
            config_dirty,
            auto_reload_pending,
            recovery_note: self.inner.recovery_note.read().clone(),
            rule_set_updates: self
                .inner
                .rule_set_updates
                .read()
                .values()
                .cloned()
                .collect(),
            route_import: self
                .inner
                .route_import
                .lock()
                .as_ref()
                .map(|p| (p.id.clone(), p.plan.summary.clone())),
            trace: self.inner.trace.read().clone(),
            recording: self.inner.recording.lock().clone(),
        }
    }

    /// Schedule a cached lookup; safe to call while refreshing a UI snapshot.
    pub fn request_node_location(&self, node_id: &str) {
        let Some(server) = self
            .inner
            .model
            .read()
            .node(node_id)
            .map(|n| n.server.clone())
        else {
            return;
        };
        let language = self.inner.settings.read().language;
        let key = location::cache_key(&server, language);
        {
            let mut locations = self.inner.node_locations.write();
            if locations
                .get(&key)
                .is_some_and(|entry| !entry.needs_refresh(now_unix()))
            {
                return;
            }
            locations.entry(key.clone()).or_default().pending = true;
        }
        let service = self.clone();
        self.spawn(async move {
            let location = location::lookup(&server, language).await;
            service.inner.node_locations.write().insert(key, location);
        });
    }

    /// Refresh enabled servers in a bounded batch, sharing the per-server cache.
    pub fn request_all_node_locations(&self) {
        self.request_all_node_locations_with(|server, language| async move {
            location::lookup(&server, language).await
        });
    }

    fn request_all_node_locations_with<F, Fut>(&self, lookup: F)
    where
        F: Fn(String, crate::settings::Language) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = NodeLocation> + Send,
    {
        if !self.inner.busy.write().insert("node-locations".into()) {
            return;
        }
        let language = self.settings().language;
        let targets: BTreeMap<_, _> = self
            .inner
            .model
            .read()
            .nodes
            .iter()
            .filter(|node| node.enabled && !node.server.trim().is_empty())
            .map(|node| {
                (
                    location::cache_key(&node.server, language),
                    node.server.clone(),
                )
            })
            .collect();
        let targets: Vec<_> = {
            let mut locations = self.inner.node_locations.write();
            targets
                .into_iter()
                .filter(|(key, _)| {
                    let entry = locations.entry(key.clone()).or_default();
                    if entry.pending {
                        false
                    } else {
                        entry.pending = true;
                        true
                    }
                })
                .collect()
        };
        if targets.is_empty() {
            self.set_busy("node-locations", false);
            self.toast(
                ToastLevel::Info,
                "没有可探测的服务器，节点可能已禁用或正在探测",
            );
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            use futures_util::{StreamExt, stream};
            let total = targets.len();
            let mut results = stream::iter(targets)
                .map(|(key, server)| {
                    let lookup = &lookup;
                    async move { (key, lookup(server, language).await) }
                })
                .buffer_unordered(4);
            let mut found = 0;
            while let Some((key, location)) = results.next().await {
                found += usize::from(location.label.is_some());
                service.inner.node_locations.write().insert(key, location);
            }
            service.set_busy("node-locations", false);
            service.toast(
                if found == total {
                    ToastLevel::Success
                } else {
                    ToastLevel::Warning
                },
                if found == total {
                    format!("归属地探测完成：{found}/{total} 个服务器")
                } else {
                    format!("归属地探测完成：{found}/{total} 个服务器，未知项可稍后重新探测")
                },
            );
        });
    }

    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.inner.rt.spawn(future);
    }

    fn set_busy(&self, key: &str, busy: bool) {
        let mut set = self.inner.busy.write();
        if busy {
            set.insert(key.to_string());
        } else {
            set.remove(key);
            if key == "core" {
                self.inner.reload_wake.send_replace(());
            }
        }
    }

    pub fn is_busy(&self, key: &str) -> bool {
        self.inner.busy.read().contains(key)
    }

    pub fn subscribe_notifications(&self) -> std::sync::mpsc::Receiver<NotificationEvent> {
        // Control events must not be dropped when delivery is waiting for permission.
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(NotificationEvent::Configure {
            enabled: self.settings().system_notifications,
            user_initiated: false,
        });
        *self.inner.notification_sender.write() = Some(sender);
        receiver
    }

    pub fn report_notification_error(&self, message: &str) {
        // Do not toast delivery failures back into the notification queue.
        self.inner.core.logs().push(
            LogStream::App,
            LogLevel::Warn,
            format!("系统通知不可用：{message}"),
        );
        self.local_toast(ToastLevel::Warning, message);
    }

    pub fn toast(&self, level: ToastLevel, message: impl Into<String>) {
        self.push_toast(level, message.into(), true);
    }

    /// Permission/delivery feedback must never recursively request a native notification.
    pub fn local_toast(&self, level: ToastLevel, message: impl Into<String>) {
        self.push_toast(level, message.into(), false);
    }

    fn push_toast(&self, level: ToastLevel, message: String, native: bool) {
        let id = self.inner.toast_seq.fetch_add(1, Ordering::SeqCst);
        let system_notification = self.inner.settings.read().system_notifications;
        let toast = Toast {
            id,
            level,
            message: message.clone(),
            at: now_unix(),
            system_notification,
        };
        if native && system_notification {
            if let Some(sender) = self.inner.notification_sender.read().as_ref() {
                let _ = sender.send(NotificationEvent::Show(toast.clone()));
            }
        }
        let mut toasts = self.inner.toasts.lock();
        toasts.push(toast);
        while toasts.len() > 20 {
            toasts.remove(0);
        }
        let log_level = match level {
            ToastLevel::Info | ToastLevel::Success => LogLevel::Info,
            ToastLevel::Warning => LogLevel::Warn,
            ToastLevel::Error => LogLevel::Error,
        };
        self.inner
            .core
            .logs()
            .push(LogStream::App, log_level, format!("[UI] {message}"));
    }

    pub fn dismiss_toast(&self, id: u64) {
        self.inner.toasts.lock().retain(|t| t.id != id);
    }

    fn active_toasts(&self) -> Vec<Toast> {
        let cutoff = now_unix() - 12;
        let mut toasts = self.inner.toasts.lock();
        toasts.retain(|t| t.at >= cutoff);
        toasts.clone()
    }

    fn persist_model(&self) -> Result<()> {
        let model = self.inner.model.read().clone();
        match self.inner.store.save_active_profile(&model) {
            Ok(id) => {
                *self.inner.active_profile.write() = Some(id);
                Ok(())
            }
            Err(err) => {
                self.toast(ToastLevel::Error, format!("保存配置失败：{err}"));
                Err(err)
            }
        }
    }

    /// Publish only after durable storage succeeds; failed edits keep the old snapshot.
    fn commit_model_change(
        &self,
        change: impl FnOnce(&mut AppModel) -> Result<bool>,
    ) -> Result<bool> {
        let changed = self.save_model_change(change)?;
        if changed {
            self.mark_dirty();
        }
        Ok(changed)
    }

    fn save_model_change(
        &self,
        change: impl FnOnce(&mut AppModel) -> Result<bool>,
    ) -> Result<bool> {
        let mut current = self.inner.model.write();
        let mut next = current.clone();
        if !change(&mut next)? {
            return Ok(false);
        }
        let profile = self.inner.store.save_active_profile(&next)?;
        *current = next;
        drop(current);
        *self.inner.active_profile.write() = Some(profile);
        Ok(true)
    }

    pub fn move_rule_to(
        &self,
        id: &str,
        group: Option<&str>,
        target: &str,
        placement: Placement,
    ) -> Result<bool> {
        let changed = self.commit_model_change(|model| {
            model.routing.move_rule_to(id, group, target, placement)
        })?;
        if changed {
            self.inner.reload.lock().dirty = true;
        }
        Ok(changed)
    }

    pub fn move_rule_group_to(&self, id: &str, target: &str, placement: Placement) -> Result<bool> {
        let changed =
            self.commit_model_change(|model| model.routing.move_group_to(id, target, placement))?;
        if changed {
            self.inner.reload.lock().dirty = true;
        }
        Ok(changed)
    }

    fn sync_model_settings(&self) {
        let settings = self.inner.settings.read().clone();
        let mut model = self.inner.model.write();
        settings.sync_inbounds(&mut model);
    }

    fn save_settings(&self, settings: &Settings) -> Result<()> {
        let old = self.settings();
        if settings == &old {
            return Ok(());
        }
        let restart_required = settings.requires_core_restart(&old, &self.inner.model.read());
        {
            let mut current = self.inner.settings.write();
            let mut saved = settings.clone();
            // Only the proxy worker commits the state of the OS operation.
            saved.system_proxy = current.system_proxy;
            saved.save(&self.inner.paths.settings_file())?;
            let mut updates = self.inner.updates.lock();
            for core in CoreKind::all() {
                if !Self::same_binary(core, &current, settings) {
                    updates.cores.remove(core.as_str());
                }
            }
            *current = saved;
        }
        if settings.hot_reload != old.hot_reload {
            self.reload_preference_changed();
            self.inner
                .helper_update_attempted
                .store(false, Ordering::SeqCst);
            if settings.hot_reload && self.inner.startup_started.load(Ordering::SeqCst) {
                let service = self.clone();
                self.spawn(async move { service.check_helper_update().await });
            }
        }
        if settings.traffic_statistics_enabled != old.traffic_statistics_enabled {
            self.statistics_setting_changed(settings.traffic_statistics_enabled);
        }
        if settings.watchdog != old.watchdog
            || settings.watchdog_max_retries != old.watchdog_max_retries
        {
            self.inner.core.set_watchdog(crate::core::WatchdogConfig {
                enabled: settings.watchdog,
                max_retries: settings.watchdog_max_retries,
            });
        }
        if restart_required {
            self.mark_dirty();
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Profiles
    // ------------------------------------------------------------------

    pub fn switch_profile(&self, id: &str) -> Result<()> {
        if self.is_busy("core") || self.is_busy("mesh-exit") {
            return Err(Error::other("正在应用配置，请完成后再管理配置"));
        }
        if self.is_running() {
            self.toast(ToastLevel::Warning, "切换配置前请先停止 core");
            return Err(Error::other("core 正在运行"));
        }
        let current_model = self.inner.model.read().clone();
        if let Some(current_id) = self.inner.active_profile.read().clone() {
            let _ = self
                .inner
                .store
                .save_profile_with_id(&current_id, &current_model);
        }
        let Some(profile) = self.inner.store.load_profile(id)? else {
            return Err(Error::other("配置不存在"));
        };
        let mut model = profile.model;
        self.inner.mesh.sync_outbounds(&mut model);
        *self.inner.model.write() = model;
        *self.inner.active_profile.write() = Some(id.to_string());
        self.inner.store.set_active_profile(id)?;
        self.inner.validation.write().take();
        self.inner.compiled.write().take();
        self.inner.compiled_json.write().take();
        *self.inner.reload.lock() = Default::default();
        self.sync_model_settings();
        self.refresh_doctor();
        self.refresh_profiles();
        self.finish_idle_mode_switch();
        self.schedule_exit_gateway_refresh();
        self.toast(
            ToastLevel::Success,
            format!("已切换到配置“{}”", profile.name),
        );
        Ok(())
    }

    pub fn create_profile(&self, name: impl Into<String>) -> Result<()> {
        if self.is_busy("core") || self.is_busy("mesh-exit") {
            return Err(Error::other("正在应用配置，请完成后再管理配置"));
        }
        if self.is_running() {
            return Err(Error::other("请先停止 core 再新建配置"));
        }
        let name = name.into();
        let current_model = self.inner.model.read().clone();
        if let Some(current_id) = self.inner.active_profile.read().clone() {
            let _ = self
                .inner
                .store
                .save_profile_with_id(&current_id, &current_model);
        }
        let model = AppModel::new(if name.trim().is_empty() {
            "新配置".into()
        } else {
            name
        });
        let mut model = model;
        self.inner.mesh.sync_outbounds(&mut model);
        let id = self.inner.store.save_profile(&model)?;
        self.inner.store.set_active_profile(&id)?;
        *self.inner.model.write() = model;
        *self.inner.active_profile.write() = Some(id);
        self.sync_model_settings();
        self.refresh_profiles();
        self.finish_idle_mode_switch();
        self.schedule_exit_gateway_refresh();
        self.toast(ToastLevel::Success, "已创建新配置");
        Ok(())
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        if self.is_busy("core") || self.is_busy("mesh-exit") {
            return Err(Error::other("正在应用配置，请完成后再管理配置"));
        }
        if self.is_running() {
            return Err(Error::other("请先停止 core 再删除配置"));
        }
        if self.inner.profiles.read().len() <= 1 {
            self.toast(ToastLevel::Warning, "至少保留一个配置");
            return Err(Error::other("至少保留一个配置"));
        }
        self.inner.store.delete_profile(id)?;
        self.refresh_profiles();
        if self.inner.store.active_profile_id().is_none() {
            if let Some(first) = self.inner.profiles.read().first().cloned() {
                self.switch_profile(&first.id)?;
            }
        }
        self.toast(ToastLevel::Info, "配置已删除");
        Ok(())
    }

    pub fn rename_profile(&self, name: impl Into<String>) {
        let name = name.into();
        {
            let mut model = self.inner.model.write();
            model.name = name.clone();
            model.updated_at = now_unix();
        }
        let _ = self.persist_model();
        self.refresh_profiles();
        self.toast(ToastLevel::Success, format!("配置已重命名为“{name}”"));
    }

    fn refresh_profiles(&self) {
        if let Ok(profiles) = self.inner.store.profiles() {
            *self.inner.profiles.write() = profiles;
        }
    }

    // ------------------------------------------------------------------
    // Subscriptions
    // ------------------------------------------------------------------

    pub fn add_subscription(
        &self,
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<String> {
        let (name, url) = subscription_fields(&name.into(), &url.into())?;
        let mut subscription = Subscription::new(name, url);
        {
            let mut subs = self.inner.subs.write();
            if subs.iter().any(|item| item.url == subscription.url) {
                return Err(Error::other("此订阅地址已添加"));
            }
            subscription.sort = subs.len() as i64;
            self.inner.store.save_subscription(&subscription)?;
            subs.push(subscription.clone());
        }
        let id = subscription.id.clone();
        self.refresh_subscription(&id);
        Ok(id)
    }

    pub fn update_subscription_settings(&self, subscription: Subscription) -> Result<()> {
        self.inner.store.save_subscription(&subscription)?;
        let mut subs = self.inner.subs.write();
        if let Some(existing) = subs.iter_mut().find(|s| s.id == subscription.id) {
            *existing = subscription;
        } else {
            subs.push(subscription);
        }
        Ok(())
    }

    fn change_subscription(&self, id: &str, change: impl FnOnce(&mut Subscription)) -> Result<()> {
        let mut subs = self.inner.subs.write();
        if self.is_busy(&format!("sub:{id}")) {
            return Err(Error::other("订阅正在更新，请稍后再试"));
        }
        let index = subs
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| Error::other("订阅不存在"))?;
        let mut updated = subs[index].clone();
        change(&mut updated);
        if subs.iter().any(|s| s.id != id && s.url == updated.url) {
            return Err(Error::other("此订阅地址已添加"));
        }
        self.inner.store.save_subscription(&updated)?;
        subs[index] = updated;
        Ok(())
    }

    pub fn edit_subscription(&self, id: &str, name: &str, url: &str) -> Result<()> {
        let (name, url) = subscription_fields(name, url)?;
        self.change_subscription(id, |sub| {
            sub.name = name;
            if sub.url != url {
                // Validators, usage and credentials belong to the original feed.
                if url::Url::parse(&sub.url).ok().map(|u| u.origin())
                    != url::Url::parse(&url).ok().map(|u| u.origin())
                {
                    sub.headers.clear();
                }
                sub.url = url;
                sub.last_updated = None;
                sub.last_user_agent = None;
                sub.last_modified = None;
                sub.etag = None;
                sub.usage = None;
                sub.last_error = None;
            }
        })
    }

    pub fn set_subscription_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.change_subscription(id, |sub| sub.enabled = enabled)
    }

    pub fn delete_subscription(&self, id: &str) -> Result<()> {
        {
            let mut subs = self.inner.subs.write();
            if self.is_busy(&format!("sub:{id}")) {
                return Err(Error::other("订阅正在更新，请稍后再试"));
            }
            self.inner.store.delete_subscription(id)?;
            subs.retain(|s| s.id != id);
        }
        self.commit_model_change(|model| {
            let removed = model.remove_subscription_nodes(id);
            for group in &mut model.groups {
                group.repair();
            }
            Ok(removed > 0)
        })?;
        self.refresh_doctor();
        self.toast(ToastLevel::Info, "订阅及对应节点已删除");
        Ok(())
    }

    pub fn resource_network(&self) -> updates::UpdateNetwork {
        let settings = self.settings();
        self.inner
            .core
            .update_network(&settings, settings.proxy_resource_updates)
    }

    pub fn refresh_subscription(&self, id: &str) {
        let key = format!("sub:{id}");
        let subscription = {
            let subs = self.inner.subs.read();
            let Some(subscription) = subs.iter().find(|s| s.id == id) else {
                self.toast(ToastLevel::Error, "订阅不存在");
                return;
            };
            if !self.inner.busy.write().insert(key.clone()) {
                return;
            }
            subscription.clone()
        };
        let service = self.clone();
        self.spawn(async move {
            let settings = service.settings();
            let network = service
                .inner
                .core
                .update_network(&settings, settings.proxy_subscription_updates);
            let user_agent = if !subscription.user_agent.is_empty() {
                subscription.user_agent.clone()
            } else if !settings.update_user_agent.is_empty() {
                settings.update_user_agent
            } else {
                crate::subscription::DEFAULT_USER_AGENT.into()
            };
            let same_agent = subscription.last_user_agent.as_ref() == Some(&user_agent);
            let request = crate::subscription::FetchRequest {
                network,
                url: subscription.url.clone(),
                user_agent: user_agent.clone(),
                headers: subscription.headers.clone(),
                etag: subscription.etag.clone().filter(|_| same_agent),
                last_modified: subscription.last_modified.clone().filter(|_| same_agent),
            };
            let result = crate::subscription::fetch_subscription(request).await;
            match result {
                Ok(response)
                    if response.status == crate::subscription::FetchStatus::NotModified =>
                {
                    let mut sub = subscription.clone();
                    sub.last_updated = Some(now_unix());
                    sub.last_error = None;
                    let _ = service.update_subscription_settings(sub);
                    service.toast(ToastLevel::Info, format!("“{}”已是最新", subscription.name));
                }
                Ok(response) => {
                    let mut parsed = parse_subscription(&response.body);
                    if let Some(title) = response.title.clone() {
                        if !title.trim().is_empty() {
                            parsed.warnings.push(format!("订阅标题：{title}"));
                        }
                    }
                    let added = match service.merge_parsed(&subscription.id, &parsed) {
                        Ok(count) => count,
                        Err(error) => {
                            service.toast(ToastLevel::Error, format!("保存订阅节点失败：{error}"));
                            service.set_busy(&key, false);
                            return;
                        }
                    };
                    let mut updated = subscription.clone();
                    updated.last_user_agent = Some(user_agent);
                    updated.etag = response.etag;
                    updated.last_modified = response.last_modified;
                    updated.usage = response
                        .userinfo
                        .as_ref()
                        .map(SubscriptionUsage::from_userinfo);
                    updated.last_updated = Some(now_unix());
                    updated.last_error = None;
                    updated.node_count = added;
                    if let Some(hours) = response.update_interval_hours {
                        if hours > 0 {
                            updated.update_interval_secs = hours * 3600;
                        }
                    }
                    let _ = service.update_subscription_settings(updated);
                    if parsed.is_empty() {
                        service.toast(
                            ToastLevel::Warning,
                            format!(
                                "“{}”未解析出节点：{}",
                                subscription.name,
                                parsed.warnings.join("；")
                            ),
                        );
                    } else {
                        service.toast(
                            ToastLevel::Success,
                            format!(
                                "“{}”更新完成：{} 个新节点（{}）",
                                subscription.name,
                                added,
                                parsed.format.label()
                            ),
                        );
                    }
                    for warning in parsed.warnings.iter().take(3) {
                        service.toast(ToastLevel::Warning, warning.clone());
                    }
                }
                Err(err) => {
                    let mut sub = subscription.clone();
                    sub.last_error = Some(err.to_string());
                    let _ = service.update_subscription_settings(sub);
                    service.toast(
                        ToastLevel::Error,
                        format!("更新订阅“{}”失败：{err}", subscription.name),
                    );
                }
            }
            service.set_busy(&key, false);
        });
    }

    pub fn refresh_all_subscriptions(&self) {
        let ids: Vec<String> = self
            .inner
            .subs
            .read()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        if ids.is_empty() {
            self.toast(ToastLevel::Info, "还没有订阅");
            return;
        }
        for id in ids {
            self.refresh_subscription(&id);
        }
    }

    /// Adds parsed nodes + groups to the model. Returns how many nodes were new.
    fn merge_parsed(
        &self,
        subscription_id: &str,
        parsed: &crate::subscription::ParsedSubscription,
    ) -> Result<usize> {
        let mut added = 0;
        self.commit_model_change(|model| {
            let before: BTreeSet<String> = model.nodes.iter().map(|n| n.id.clone()).collect();
            let mut incoming: Vec<Node> = parsed.nodes.clone();
            for node in &mut incoming {
                node.subscription_id = Some(subscription_id.to_string());
                node.source = crate::model::NodeSource::Subscription;
            }
            added = model.add_nodes(incoming);
            if added == 0 {
                return Ok(false);
            }
            let new_nodes: Vec<(String, String)> = model
                .nodes
                .iter()
                .filter(|n| !before.contains(&n.id))
                .filter(|n| n.subscription_id.as_deref() == Some(subscription_id))
                .map(|n| (n.name.clone(), n.id.clone()))
                .collect();
            for group in &parsed.groups {
                let members: Vec<String> = group
                    .members
                    .iter()
                    .filter_map(|name| {
                        new_nodes
                            .iter()
                            .find(|(node_name, _)| node_name == name)
                            .map(|(_, id)| id.clone())
                    })
                    .collect();
                if members.is_empty() {
                    continue;
                }
                if let Some(existing) = model.groups.iter_mut().find(|g| g.name == group.name) {
                    existing.kind = group.kind;
                    existing.enabled = true;
                    for member in members {
                        if !existing.members.contains(&member) {
                            existing.members.push(member);
                        }
                    }
                    existing.repair();
                } else {
                    let mut new_group =
                        crate::model::OutboundGroup::new(group.name.clone(), group.kind);
                    new_group.members = members;
                    new_group.selected = new_group.members.first().cloned();
                    model.groups.push(new_group);
                }
            }
            model.updated_at = now_unix();
            Ok(true)
        })?;
        if added > 0 {
            self.refresh_doctor();
        }
        Ok(added)
    }

    /// Inspect the same formats as direct import without persisting anything or resolving a hostname.
    pub fn preview_shared_content(content: &str) -> SharePreview {
        let (urls, nodes) = split_shared_content(content);
        let parsed = parse_subscription(&nodes);
        if let Some(node) = parsed.nodes.first() {
            return SharePreview::Node {
                name: node.name.clone(),
                protocol: node.protocol.display_name(),
                endpoint: node.endpoint(),
                count: parsed.nodes.len(),
            };
        }
        match urls.first() {
            Some(url) => SharePreview::Subscription {
                host: pretty_host(url),
            },
            None => SharePreview::Unsupported,
        }
    }

    /// Routes shared text from a clipboard, dropped file, or QR code.
    pub fn import_shared_content(&self, content: &str) -> SharedImport {
        let (urls, nodes) = split_shared_content(content);
        let mut result = SharedImport::default();
        for url in urls {
            if self.inner.subs.read().iter().any(|sub| sub.url == url) {
                result.duplicates += 1;
            } else {
                match self.add_subscription("", url) {
                    Ok(_) => result.subscriptions += 1,
                    Err(err) => result.errors.push(err.to_string()),
                }
            }
        }
        if !nodes.trim().is_empty() {
            let parsed = parse_subscription(&nodes);
            result.unreadable = parsed.is_empty() || !parsed.warnings.is_empty();
            if !parsed.is_empty() {
                match self.merge_parsed(PASTED_SOURCE, &parsed) {
                    Ok(count) => {
                        result.nodes = count;
                        result.duplicates += parsed.nodes.len() - count;
                    }
                    Err(err) => result.errors.push(err.to_string()),
                }
            }
        } else if result.subscriptions + result.duplicates == 0 && result.errors.is_empty() {
            result.unreadable = true;
        }
        result
    }

    /// Imports share links / Clash YAML / SIP008 / sing-box JSON pasted by the user.
    pub fn import_content(&self, content: &str) -> usize {
        let parsed = parse_subscription(content);
        if parsed.is_empty() {
            self.toast(
                ToastLevel::Error,
                format!("导入失败：{}", parsed.warnings.join("；")),
            );
            return 0;
        }
        let added = match self.merge_parsed(PASTED_SOURCE, &parsed) {
            Ok(count) => count,
            Err(error) => {
                self.toast(ToastLevel::Error, format!("保存节点失败：{error}"));
                return 0;
            }
        };
        self.toast(
            ToastLevel::Success,
            format!("已导入 {} 个节点（{}）", added, parsed.format.label()),
        );
        if !parsed.warnings.is_empty() {
            self.toast(ToastLevel::Warning, parsed.warnings.join("；"));
        }
        added
    }

    /// Adds a node entered in the manual form. Validation lives in
    /// [`crate::model::NodeDraft::into_node`]; the returned id can be used to
    /// test or select the node right away.
    pub fn add_manual_node(&self, draft: NodeDraft) -> Result<String> {
        let core = self.inner.settings.read().core;
        let node = draft.into_node(core)?;
        let fingerprint = node.fingerprint();
        self.commit_model_change(|model| {
            if model.nodes.iter().any(|n| n.fingerprint() == fingerprint) {
                return Err(Error::other(
                    "已存在相同节点（协议 / 地址 / 端口 / 凭据一致）",
                ));
            }
            if model.add_nodes([node.clone()]) == 0 {
                return Err(Error::other("节点未加入配置（可能已存在）"));
            }
            Ok(true)
        })?;
        self.refresh_doctor();
        self.toast(
            ToastLevel::Success,
            format!("已添加节点“{}”（{}:{}）", node.name, node.server, node.port),
        );
        Ok(node.id)
    }

    // ------------------------------------------------------------------
    // Nodes and groups
    // ------------------------------------------------------------------

    /// Replaces editable node parameters without changing group references or subscription ownership.
    pub fn update_node(&self, node_id: &str, draft: NodeDraft) -> Result<()> {
        if node_id.starts_with("mesh-exit-") {
            return Err(Error::config("请在组网配置中编辑出口节点"));
        }
        // An existing node remains editable when the selected core cannot run its protocol.
        let mut model = self.inner.model.write();
        let original = model
            .node(node_id)
            .ok_or_else(|| Error::other("节点不存在，请刷新后重试"))?;
        let node = draft.apply_to(original, CoreKind::SingBox)?;
        let fingerprint = node.fingerprint();
        if model
            .nodes
            .iter()
            .any(|n| n.id != node_id && n.fingerprint() == fingerprint)
        {
            return Err(Error::other(
                "已存在相同节点（协议 / 地址 / 端口 / 凭据一致）",
            ));
        }
        let mut next = model.clone();
        *next.node_mut(node_id).expect("node was found above") = node;
        next.updated_at = now_unix();
        let profile_id = self.inner.store.save_active_profile(&next)?;
        *model = next;
        drop(model);
        *self.inner.active_profile.write() = Some(profile_id);
        self.inner.latency.write().remove(node_id);
        if let Err(err) = self.inner.store.remove_latency(node_id) {
            self.toast(ToastLevel::Warning, format!("清除节点测速缓存失败：{err}"));
        }
        self.refresh_doctor();
        self.mark_dirty();
        self.toast(ToastLevel::Success, "节点已保存");
        Ok(())
    }

    pub fn move_node_to(&self, node_id: &str, target_id: &str) {
        let moved = self.inner.model.write().move_node_to(node_id, target_id);
        if moved {
            if let Err(err) = self.persist_model() {
                self.toast(ToastLevel::Error, format!("保存节点顺序失败：{err}"));
                return;
            }
            self.mark_dirty();
        }
    }

    pub fn select_direct_mode(&self) -> Result<()> {
        self.select_work_mode(WorkMode::Direct)
    }

    pub fn select_proxy_mode(&self) -> Result<()> {
        self.select_work_mode(WorkMode::Proxy)
    }

    fn select_work_mode(&self, mode: WorkMode) -> Result<()> {
        if self.is_busy("core") || self.is_busy("mesh-exit") {
            return Err(Error::other("内核忙，请稍后重试"));
        }
        let core = self.settings().core;
        let changed = self.commit_model_change(|model| {
            if mode == WorkMode::Proxy && !model.has_usable_default_group(core) {
                return Err(Error::config(
                    "请先在默认出站组添加或启用当前内核支持的节点",
                ));
            }
            if mode == model.work_mode {
                return Ok(false);
            }
            model.work_mode = mode;
            model.updated_at = now_unix();
            Ok(true)
        })?;
        if changed {
            if self.is_running() {
                self.apply_config();
            } else {
                self.finish_idle_mode_switch();
                self.notify_work_mode(mode);
            }
        }
        Ok(())
    }

    fn notify_work_mode(&self, mode: WorkMode) {
        use crate::settings::Language;
        let message = match (mode, self.inner.settings.read().language) {
            (WorkMode::Direct, Language::Zh) => "已切换至本机直连",
            (WorkMode::Direct, Language::En) => "Switched to Direct",
            (WorkMode::Proxy, Language::Zh) => "已切换至代理模式",
            (WorkMode::Proxy, Language::En) => "Switched to proxy mode",
            (WorkMode::Mesh, Language::Zh) => "组网模式已启动，系统流量由所选出口接管",
            (WorkMode::Mesh, Language::En) => {
                "Mesh mode started; the selected exit handles system traffic"
            }
        };
        self.toast(ToastLevel::Success, message);
    }

    pub fn select_node(&self, group_id: &str, node_id: &str) {
        if group_id.is_empty() && self.select_saved_mesh_gateway(node_id) {
            return;
        }
        if self.is_busy("core") || self.is_busy("mesh-exit") {
            return;
        }
        let settings = self.settings();
        let (node, group_tag, mode_changed) = {
            let mut current = self.inner.model.write();
            let Some(group) = current.group(group_id).filter(|g| {
                g.enabled && g.manually_selectable() && g.members.iter().any(|id| id == node_id)
            }) else {
                return;
            };
            let Some(node) = current
                .node(node_id)
                .filter(|n| n.usable_with(settings.core))
            else {
                return;
            };
            let mode_changed = current.work_mode != WorkMode::Proxy;
            let selection = (node.clone(), group.tag(), mode_changed);
            let mut next = current.clone();
            if !next.select_node(group_id, node_id) && !mode_changed {
                return;
            }
            next.work_mode = WorkMode::Proxy;
            next.updated_at = now_unix();
            match self.inner.store.save_active_profile(&next) {
                Ok(profile) => {
                    *current = next;
                    *self.inner.active_profile.write() = Some(profile);
                }
                Err(error) => {
                    self.toast(ToastLevel::Error, format!("保存节点选择失败：{error}"));
                    return;
                }
            }
            selection
        };

        if !self.is_running() {
            if mode_changed {
                self.finish_idle_mode_switch();
            }
            self.schedule_exit_gateway_refresh();
            return;
        }
        self.schedule_exit_gateway_refresh();
        if mode_changed {
            self.mark_dirty();
            self.apply_config();
        } else if settings.core == CoreKind::SingBox {
            // sing-box's selector supports instant switching through Clash API.
            let client = ClashClient::new(settings.api_port, "");
            let tag = node.outbound_tag();
            let epoch = CoreEpoch::from_state(&self.process_state());
            let service = self.clone();
            self.spawn(async move {
                match client.select(&group_tag, &tag).await {
                    Ok(()) => {
                        if epoch.is_some()
                            && epoch == CoreEpoch::from_state(&service.process_state())
                        {
                            let mut proxies = service.inner.proxies.write();
                            Arc::make_mut(&mut proxies)
                                .entry(group_tag)
                                .or_default()
                                .now = Some(tag);
                        }
                        service.toast(ToastLevel::Success, format!("已切换到“{}”", node.name))
                    }
                    Err(err) => {
                        // Fall back to a config reapply if the group is not live.
                        service.toast(
                            ToastLevel::Warning,
                            format!("实时切换失败，将重载内核：{err}"),
                        );
                        service.apply_config();
                    }
                }
            });
        } else {
            self.toast(
                ToastLevel::Info,
                "Xray 不支持热切换节点，正在重载内核以应用新节点",
            );
            self.apply_config();
        }
    }

    fn running_proxy_node(&self, phase: ProcessPhase) -> Option<String> {
        if !phase.is_alive() {
            return None;
        }
        self.inner.core.applied_proxy_node(|tag| {
            let proxies = self.inner.proxies.read();
            Some(
                proxies
                    .get(tag)?
                    .now
                    .as_deref()?
                    .strip_prefix("node-")?
                    .to_owned(),
            )
        })
    }

    pub fn set_node_enabled(&self, node_id: &str, enabled: bool) {
        self.set_nodes_enabled([node_id], enabled);
    }

    pub fn set_nodes_enabled<'a>(&self, ids: impl IntoIterator<Item = &'a str>, enabled: bool) {
        let ids: BTreeSet<_> = ids.into_iter().collect();
        let result = self.commit_model_change(|model| {
            let running = self.running_proxy_node(self.process_state().phase);
            let mut changed = false;
            for node in &mut model.nodes {
                if ids.contains(node.id.as_str())
                    && node.enabled != enabled
                    && running.as_deref() != Some(node.id.as_str())
                {
                    node.enabled = enabled;
                    changed = true;
                }
            }
            Ok(changed)
        });
        match result {
            Ok(true) => self.refresh_doctor(),
            Ok(false) => {}
            Err(error) => self.toast(ToastLevel::Error, format!("保存节点启用状态失败：{error}")),
        }
    }

    pub fn delete_node(&self, node_id: &str) {
        {
            let mut model = self.inner.model.write();
            model.remove_node(node_id);
        }
        let _ = self.persist_model();
        self.inner.latency.write().remove(node_id);
        self.refresh_doctor();
        self.mark_dirty();
        self.toast(ToastLevel::Info, "节点已删除");
    }

    pub fn delete_nodes(&self, node_ids: &[String]) {
        if node_ids.is_empty() {
            return;
        }
        let count = node_ids.len();
        {
            let mut model = self.inner.model.write();
            for id in node_ids {
                model.remove_node(id);
            }
        }
        let _ = self.persist_model();
        {
            let mut lat = self.inner.latency.write();
            for id in node_ids {
                lat.remove(id);
            }
        }
        self.refresh_doctor();
        self.mark_dirty();
        self.toast(ToastLevel::Info, format!("已批量删除 {count} 个节点"));
    }

    pub fn select_group_kind(&self, group_id: &str, kind: crate::model::GroupKind) {
        {
            let mut model = self.inner.model.write();
            let Some(group) = model.group_mut(group_id) else {
                return;
            };
            group.kind = kind;
            if group.switch_policy.mode == crate::model::SwitchMode::Off {
                group.switch_policy.mode = crate::model::SwitchMode::GroupDefault;
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn export_links(&self, node_ids: &[String]) -> String {
        let model = self.inner.model.read();
        let by_id: std::collections::HashMap<_, _> =
            model.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        node_ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .filter_map(crate::subscription::node_to_share_link)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn node_qr(&self, node_id: &str) -> Result<String> {
        qr_ascii(&self.node_share_link(node_id)?)
    }

    /// Returns QR modules for graphical sharing, without writing credentials to disk.
    pub fn node_qr_matrix(&self, node_id: &str) -> Result<Vec<Vec<bool>>> {
        crate::subscription::qr_matrix(&self.node_share_link(node_id)?)
    }

    fn node_share_link(&self, node_id: &str) -> Result<String> {
        let node = self
            .inner
            .model
            .read()
            .node(node_id)
            .cloned()
            .ok_or_else(|| Error::other("节点不存在"))?;
        crate::subscription::node_to_share_link(&node)
            .ok_or_else(|| Error::other("该协议暂不支持导出分享链接"))
    }

    // ------------------------------------------------------------------
    // Latency
    // ------------------------------------------------------------------

    fn try_begin_probe(&self, key: &str) -> bool {
        let mut busy = self.inner.busy.write();
        if ["latency", "download", "auto-probe"]
            .iter()
            .any(|k| busy.contains(*k))
        {
            return false;
        }
        busy.insert(key.to_owned())
    }

    pub fn test_latency(&self, scope: LatencyScope) {
        self.test_latency_scope(scope, false);
    }

    pub fn test_http_latency(&self, ids: Vec<String>) {
        self.test_http_targets(ids.into_iter().map(HttpTestTarget::Node).collect());
    }

    fn test_latency_scope(&self, scope: LatencyScope, force_http: bool) {
        if self.is_busy("latency") || self.is_busy("download") || self.is_busy("auto-probe") {
            return;
        }
        let model = self.inner.model.read().clone();
        let node_ids: Vec<String> = match &scope {
            LatencyScope::All => model.nodes.iter().map(|n| n.id.clone()).collect(),
            LatencyScope::Node(id) => vec![id.clone()],
            LatencyScope::Nodes(ids) => ids.clone(),
            LatencyScope::Group(id) => model
                .group(id)
                .map(|g| g.members.clone())
                .unwrap_or_default(),
        };
        let targets: Vec<Node> = node_ids
            .iter()
            .filter_map(|id| model.node(id))
            .filter(|n| n.enabled)
            .cloned()
            .collect();
        // Use the applied configuration, not a pending settings edit. A TUN TCP
        // handshake can terminate locally before any remote connection exists.
        let bypass_tun = self.inner.core.native_tun_active();
        let use_http = force_http || bypass_tun;
        let settings = self.settings();
        let core = if bypass_tun {
            self.inner.core.state().core.unwrap_or(settings.core)
        } else {
            settings.core
        };
        let binary = if use_http {
            match self.inner.core.resolve_binary(core, &settings) {
                Ok(binary) => Some(binary),
                Err(err) => {
                    self.toast(ToastLevel::Error, err.to_string());
                    return;
                }
            }
        } else {
            None
        };
        if targets.is_empty() {
            self.toast(ToastLevel::Warning, "没有可测试的节点");
            return;
        }
        if !self.try_begin_probe("latency") {
            return;
        }
        let target = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            use futures_util::{StreamExt, stream};
            let results: Vec<_> = stream::iter(targets)
                .map(|node| {
                    let binary = binary.clone();
                    let url = settings.urltest_url.clone();
                    async move {
                        let result = if let Some(binary) = binary {
                            match crate::runtime::probe::http_latency(
                                &binary, core, &node, &url, bypass_tun,
                            )
                            .await
                            {
                                Ok(ms) => crate::runtime::LatencyResult::ok(ms),
                                Err(err) => crate::runtime::LatencyResult::failed(err.to_string()),
                            }
                        } else {
                            crate::runtime::tcping(
                                &node.server,
                                node.port,
                                std::time::Duration::from_secs(4),
                            )
                            .await
                        };
                        (node.id, result)
                    }
                })
                .buffer_unordered(4)
                .collect()
                .await;
            if use_http {
                for (id, result) in &results {
                    service.record_proxy_probe(target.as_ref(), id, result.ok);
                }
            }
            let mut ok_count = 0;
            {
                let mut cache = service.inner.latency.write();
                for (key, result) in &results {
                    if result.ok {
                        ok_count += 1;
                    }
                    let entry = LatencyEntry {
                        key: key.clone(),
                        delay_ms: result.delay_ms,
                        checked_at: now_unix(),
                        detail: result.error.clone().or_else(|| {
                            Some(
                                if use_http {
                                    "HTTP 延迟"
                                } else {
                                    "TCP 延迟"
                                }
                                .into(),
                            )
                        }),
                    };
                    let _ = service.inner.store.set_latency(&entry);
                    cache.insert(key.clone(), entry);
                }
            }
            service.set_busy("latency", false);
            service.toast(
                ToastLevel::Info,
                format!("测速完成：{ok_count}/{} 个节点可用", results.len()),
            );
        });
    }

    /// HTTP latency through a private core configured with exactly one node.
    pub fn test_real_latency(&self, node_id: &str) {
        self.test_http_target(HttpTestTarget::Node(node_id.into()));
    }

    pub fn test_download(&self, node_id: &str) {
        self.test_downloads(&[node_id.to_owned()]);
    }

    pub fn test_downloads(&self, ids: &[String]) {
        if self.is_busy("download") || self.is_busy("latency") || self.is_busy("auto-probe") {
            return;
        }
        let settings = self.settings();
        let bypass_tun = self.inner.core.native_tun_active();
        let core = if bypass_tun {
            self.inner.core.state().core.unwrap_or(settings.core)
        } else {
            settings.core
        };
        let model = self.model();
        let nodes: Vec<_> = ids
            .iter()
            .filter_map(|id| model.node(id))
            .filter(|node| node.enabled && node.protocol.supported_by(core))
            .cloned()
            .collect();
        if nodes.is_empty() {
            self.toast(ToastLevel::Warning, "没有已启用且当前内核支持的测速节点");
            return;
        }
        let binary = match self.inner.core.resolve_binary(core, &settings) {
            Ok(binary) => binary,
            Err(err) => {
                self.toast(ToastLevel::Error, err.to_string());
                return;
            }
        };
        if !self.try_begin_probe("download") {
            return;
        }
        let target = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            // Measure downloads sequentially so probes do not compete for bandwidth.
            let count = nodes.len();
            let mut successful = 0;
            for node in nodes {
                service.set_busy(&format!("download:{}", node.id), true);
                service.inner.downloads.write().remove(&node.id);
                let result =
                    crate::runtime::probe::download(&binary, core, &node, bypass_tun).await;
                service.record_proxy_probe(target.as_ref(), &node.id, result.is_ok());
                let entry = match result {
                    Ok(entry) => {
                        successful += 1;
                        if count == 1 {
                            service.toast(
                                ToastLevel::Success,
                                format!(
                                    "“{}”下载速度 {:.2} MiB/s",
                                    node.name,
                                    entry.bytes_per_second as f64 / 1048576.0
                                ),
                            );
                        }
                        entry
                    }
                    Err(err) => {
                        if count == 1 {
                            service.toast(ToastLevel::Warning, err.to_string());
                        }
                        crate::runtime::probe::DownloadResult {
                            bytes: 0,
                            elapsed_ms: 0,
                            bytes_per_second: 0,
                            checked_at: now_unix(),
                            error: Some(err.to_string()),
                        }
                    }
                };
                service
                    .inner
                    .downloads
                    .write()
                    .insert(node.id.clone(), entry);
                service.set_busy(&format!("download:{}", node.id), false);
            }
            service.set_busy("download", false);
            if count > 1 {
                service.toast(
                    ToastLevel::Info,
                    format!("下载测速完成：{successful}/{count} 个节点成功"),
                );
            }
        });
    }

    pub fn clear_latency(&self) {
        self.inner.latency.write().clear();
        let _ = self.inner.store.clear_latency();
        self.toast(ToastLevel::Info, "已清除测速结果");
    }

    // ------------------------------------------------------------------
    // Core lifecycle
    // ------------------------------------------------------------------

    /// Downloads any missing rule-sets before starting sing-box. The core will
    /// otherwise fetch them itself at startup, which fails behind a broken DNS
    /// path and makes the start non-deterministic.
    async fn ensure_rule_sets(
        &self,
        model: &AppModel,
        settings: &Settings,
    ) -> std::result::Result<(), String> {
        if settings.core == CoreKind::Xray {
            let binary = self
                .inner
                .core
                .resolve_binary(settings.core, settings)
                .map_err(|e| e.to_string())?;
            let dir = binary.parent().ok_or_else(|| "内核目录无效".to_string())?;
            let mut ctx = crate::adapters::CompileContext::new(
                settings,
                self.inner.paths.data_dir.clone(),
                self.inner.paths.runtime_dir.clone(),
            );
            ctx.mesh_exit = self.inner.core.mesh_exit_runtime();
            let compiled = crate::adapters::adapter_for(settings.core)
                .compile(model, settings, &ctx)
                .map_err(|e| e.to_string())?;
            return updates::ensure_xray_geo(&compiled.json, dir, &self.resource_network())
                .await
                .map_err(|e| e.to_string());
        }
        let sets = crate::adapters::required_rule_sets(model);
        let dir = self.inner.paths.rule_sets_dir();
        let missing_file = |set: &crate::model::RuleSet| {
            !std::fs::metadata(updates::rule_set_file(&dir, &set.tag))
                .is_ok_and(|m| m.is_file() && m.len() > 0)
        };
        if !sets.iter().any(missing_file) {
            return Ok(());
        }
        let _lock = self.inner.rule_set_lock.lock().await;
        let missing: Vec<_> = sets.into_iter().filter(missing_file).collect();
        if missing.is_empty() {
            return Ok(());
        }
        self.toast(
            ToastLevel::Info,
            format!("正在预下载 {} 个规则集…", missing.len()),
        );
        let results = self.download_rule_sets(&missing).await;
        let failed: Vec<_> = results.iter().filter(|r| r.status == "failed").collect();
        for failure in failed {
            self.inner.core.logs().push(
                LogStream::App,
                LogLevel::Warn,
                format!(
                    "规则集 {} 下载失败：{}",
                    failure.tag,
                    failure.error.clone().unwrap_or_default()
                ),
            );
        }
        let still_missing: Vec<String> = missing
            .iter()
            .filter(|set| missing_file(set))
            .map(|set| set.tag.clone())
            .collect();
        if still_missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "规则集下载失败：{}。请检查网络及资源更新代理设置，或在“路由 → 规则集”中更换镜像。",
                still_missing.join(", ")
            ))
        }
    }

    async fn ensure_core_binary(&self, settings: &mut Settings) -> Result<()> {
        let _install = self.inner.install_lock.lock().await;
        match self.inner.core.resolve_binary(settings.core, settings) {
            Ok(binary) => {
                if settings.active_version(settings.core).is_none() {
                    self.inner
                        .core
                        .binaries()
                        .register_local(settings.core, &binary)?;
                }
                return Ok(());
            }
            Err(error)
                if settings.binary_override(settings.core).is_some()
                    || settings.active_version(settings.core).is_none() =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
        self.toast(
            ToastLevel::Info,
            format!("正在下载 {} 内核…", settings.core.display_name()),
        );
        self.set_busy("install", true);
        let result: Result<_> = async {
            let network = self.resource_network();
            let candidate = updates::check_core_update(settings.core, None, &network)
                .await?
                .ok_or_else(|| Error::binary("未找到可下载的内核版本"))?;
            let checksum = match candidate.checksum_url {
                Some(url) => updates::download_checksum(&url, &network).await?,
                None => None,
            };
            self.inner
                .core
                .binaries()
                .install_from_url(
                    settings.core,
                    &candidate.version,
                    &candidate.asset.url,
                    checksum,
                    &network,
                )
                .await
        }
        .await;
        self.set_busy("install", false);
        let record = result?;
        settings
            .active_versions
            .insert(settings.core.as_str().into(), record.version.clone());
        // Merge only the installed version so edits made during the download survive.
        let mut current = self.settings();
        current
            .active_versions
            .insert(settings.core.as_str().into(), record.version);
        self.save_settings(&current)?;
        Ok(())
    }

    /// Called once on every GUI launch, never by CLI/test construction.
    pub fn run_startup_actions(&self) {
        if self.inner.startup_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        // Begin crash recovery after the GUI has acquired its single-instance lock.
        let recovering = self.inner.paths.proxy_snapshot().exists();
        if recovering {
            self.set_system_proxy(false);
        }
        self.spawn(async move {
            while service.is_busy("proxy") {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if recovering && !service.inner.paths.proxy_snapshot().exists() {
                *service.inner.recovery_note.write() =
                    Some("已恢复上次异常退出时的系统代理设置".into());
            }
            let settings = service.settings();
            if settings.hot_reload {
                service.check_helper_update().await;
            }
            if settings.startup_proxy {
                service.start_core();
            }
            if settings.check_app_updates {
                if settings.startup_proxy
                    && settings.proxy_resource_updates
                    && settings.update_proxy.as_str().is_empty()
                {
                    while service.is_busy("core") {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
                service.check_core_release(settings.core, false);
                service.check_app_release(false);
            }
            let (ids, missing) = resolve_startup_mesh(&settings, &service.inner.mesh.snapshot());
            for provider in missing {
                service.toast(
                    ToastLevel::Warning,
                    format!("{} 应用启动连接配置不存在，请重新选择", provider.label()),
                );
            }
            service.connect_mesh_sequence(&ids).await;
        });
    }

    pub fn start_core(&self) {
        if self.model().work_mode == WorkMode::Mesh {
            self.inner
                .mesh_takeover_requested
                .store(true, Ordering::Release);
        }
        if self.is_running() {
            self.toast(ToastLevel::Info, "core 已在运行");
            return;
        }
        if !self.inner.busy.write().insert("core".into()) {
            return;
        }
        self.inner.last_start_failed.store(false, Ordering::Relaxed);
        self.sync_model_settings();
        if self.persist_model().is_err() {
            self.inner.last_start_failed.store(true, Ordering::Relaxed);
            self.set_busy("core", false);
            return;
        }
        self.refresh_doctor();
        let service = self.clone();
        self.spawn(async move {
            let success = match service.start_core_task().await {
                Ok(started) => started,
                Err(error) => {
                    service.toast(ToastLevel::Error, error);
                    false
                }
            };
            service
                .inner
                .last_start_failed
                .store(!success, Ordering::Relaxed);
            service.set_busy("core", false);
        });
    }

    async fn start_core_task(&self) -> std::result::Result<bool, String> {
        let mut settings = self.settings();
        let model = self.model();
        self.prepare_mesh_mode(&model)
            .await
            .map_err(|e| e.to_string())?;
        let mut ctx = crate::adapters::CompileContext::new(
            &settings,
            self.inner.paths.data_dir.clone(),
            self.inner.paths.runtime_dir.clone(),
        );
        ctx.mesh_exit = self.inner.core.mesh_exit_runtime();
        ctx.mesh = self.inner.core.mesh_plan();
        crate::adapters::adapter_for(settings.core)
            .compile(&model, &settings, &ctx)
            .map_err(|err| err.to_string())?;
        self.ensure_core_binary(&mut settings)
            .await
            .map_err(|err| format!("准备内核失败：{err}"))?;
        self.ensure_rule_sets(&model, &settings).await?;
        self.ensure_tun_helper().await?;
        if let Some(warning) = self.prepare_tun_via_helper().await {
            self.toast(
                ToastLevel::Warning,
                format!("提权助手未能准备 TUN：{warning}"),
            );
        }
        let outcome = self
            .inner
            .core
            .start(&model, &settings, settings.core)
            .await
            .map_err(|err| format!("启动失败：{err}"))?;
        let started = self.record_outcome(&outcome);
        if started {
            self.inner.traffic.lock().reset();
            self.inner.connections.write().clear();
            *self.inner.proxies.write() = Default::default();
            self.finish_config_apply(&model, &settings);
            self.update_mode_system_proxy()
                .await
                .map_err(|e| e.to_string())?;
            if model.work_mode == WorkMode::Mesh {
                self.notify_work_mode(WorkMode::Mesh);
            } else {
                self.toast(
                    ToastLevel::Success,
                    format!(
                        "{} 已启动，代理端口 {}",
                        settings.core.display_name(),
                        settings.mixed_port
                    ),
                );
            }
        }
        Ok(started)
    }

    pub fn stop_core(&self) {
        self.inner
            .mesh_takeover_requested
            .store(false, Ordering::Release);
        if !self.is_running() {
            self.toast(ToastLevel::Info, "core 未运行");
            return;
        }
        if !self.inner.busy.write().insert("core".into()) {
            return;
        }
        self.inner.reload.lock().due = None;
        self.stop_managed_proxy();
        let service = self.clone();
        self.spawn(async move {
            service.poll_traffic_statistics().await;
            service.flush_traffic_statistics().await;
            let result = service.inner.core.stop().await;
            service.cleanup_tun_via_helper().await;
            match result {
                Ok(()) => {
                    service.inner.traffic.lock().reset();
                    service.inner.connections.write().clear();
                    *service.inner.proxies.write() = Default::default();
                    service.toast(ToastLevel::Success, "core 已停止");
                }
                Err(err) => service.toast(ToastLevel::Error, format!("停止失败：{err}")),
            }
            service.finish_core_stop();
            service.set_busy("core", false);
        });
    }

    pub fn toggle_core(&self) {
        if self.is_running() {
            self.stop_core();
        } else {
            self.start_core();
        }
    }

    /// Start or stop both proxy core and mesh networks from a single control.
    pub fn toggle_proxy_and_mesh(&self) {
        if self.is_busy("core")
            || self.is_busy("mesh")
            || self.is_busy("mesh-exit")
            || self.is_busy("power")
        {
            return;
        }
        let core_running = self.is_running();
        let mesh = self.inner.mesh.snapshot();
        let mut disconnect_providers = Vec::new();
        let mut any_mesh_active = false;
        for state in &mesh.networks {
            if matches!(
                state.phase,
                crate::mesh::MeshPhase::Starting
                    | crate::mesh::MeshPhase::NeedsLogin
                    | crate::mesh::MeshPhase::NeedsApproval
                    | crate::mesh::MeshPhase::Connected
            ) {
                any_mesh_active = true;
                if !disconnect_providers.contains(&state.provider) {
                    disconnect_providers.push(state.provider);
                }
            } else if state.phase == crate::mesh::MeshPhase::Stopping {
                any_mesh_active = true;
            }
        }
        let has_reconnecting = !self.inner.mesh_reconnect.lock().pending(&mesh).is_empty();
        if core_running || any_mesh_active || has_reconnecting {
            for provider in [
                crate::mesh::MeshProvider::Tailscale,
                crate::mesh::MeshProvider::EasyTier,
            ] {
                self.inner.mesh_reconnect.lock().forget_provider(provider);
            }
            if has_reconnecting && !core_running && disconnect_providers.is_empty() {
                self.toast(ToastLevel::Info, "已取消组网重连");
                return;
            }
            if !core_running && disconnect_providers.is_empty() {
                return;
            }
            if !self.inner.busy.write().insert("power".into()) {
                return;
            }
            if core_running {
                self.stop_core();
            }
            let service = self.clone();
            self.spawn(async move {
                service
                    .disconnect_mesh_sequence(&disconnect_providers)
                    .await;
                while service.is_busy("core") || service.is_busy("mesh") {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                service.set_busy("power", false);
            });
            return;
        }

        let settings = self.settings();
        let plan = plan_unified_start(&settings, &mesh);
        for provider in plan.missing_startup {
            self.toast(
                ToastLevel::Warning,
                format!("{} 应用启动连接配置不存在，请重新选择", provider.label()),
            );
        }
        if !plan.start_core && plan.mesh_profile_ids.is_empty() {
            return;
        }
        if !self.inner.busy.write().insert("power".into()) {
            return;
        }
        if plan.start_core {
            self.start_core();
        }
        let service = self.clone();
        let mesh_ids = plan.mesh_profile_ids;
        self.spawn(async move {
            service.connect_mesh_sequence(&mesh_ids).await;
            while service.is_busy("core") || service.is_busy("mesh") {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            service.set_busy("power", false);
        });
    }

    async fn connect_mesh_sequence(&self, profile_ids: &[String]) {
        self.run_mesh_sequence(profile_ids, |id| self.mesh_connect(id))
            .await;
    }

    async fn disconnect_mesh_sequence(&self, providers: &[crate::mesh::MeshProvider]) {
        self.run_mesh_sequence(providers, |&provider| self.mesh_disconnect(provider))
            .await;
    }

    async fn run_mesh_sequence<T>(&self, items: &[T], mut step: impl FnMut(&T)) {
        for item in items {
            // mesh_action has a shared operation gate; await each item rather than dropping the next request.
            while self.is_busy("mesh") || self.is_busy("core") {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            step(item);
            while self.is_busy("mesh") {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }

    /// Apply the saved intent now, validating before replacing a running core.
    pub fn apply_config(&self) {
        self.refresh_exit_gateway_policies(true);
        self.apply_config_with_tun_cleanup(false, false);
    }

    fn apply_config_with_tun_cleanup(&self, cleanup_old_tun: bool, automatic: bool) {
        if !self.claim_config_apply(automatic) {
            return;
        }
        self.sync_model_settings();
        if self.persist_model().is_err() {
            self.set_busy("core", false);
            return;
        }
        self.refresh_doctor();
        let was_running = self.is_running();
        let service = self.clone();
        self.spawn(async move {
            let mut settings = service.inner.settings.read().clone();
            let model = service.inner.model.read().clone();
            if let Err(error) = service.prepare_mesh_mode(&model).await {
                service.set_busy("core", false);
                service.toast(ToastLevel::Error, error.to_string());
                return;
            }
            let mut ctx = crate::adapters::CompileContext::new(
                &settings,
                service.inner.paths.data_dir.clone(),
                service.inner.paths.runtime_dir.clone(),
            );
            ctx.mesh_exit = service.inner.core.mesh_exit_runtime();
            ctx.mesh = service.inner.core.mesh_plan();
            if let Err(err) =
                crate::adapters::adapter_for(settings.core).compile(&model, &settings, &ctx)
            {
                service.set_busy("core", false);
                service.toast(ToastLevel::Error, err.to_string());
                return;
            }
            if let Err(err) = service.ensure_core_binary(&mut settings).await {
                service.set_busy("core", false);
                service.toast(ToastLevel::Error, format!("准备内核失败：{err}"));
                return;
            }
            // Prepare dependencies even when applying a stopped configuration.
            if let Err(err) = service.ensure_rule_sets(&model, &settings).await {
                service.set_busy("core", false);
                service.toast(ToastLevel::Error, err);
                return;
            }
            if was_running {
                if let Err(err) = service.ensure_tun_helper().await {
                    service.set_busy("core", false);
                    service.toast(ToastLevel::Error, err);
                    return;
                }
                if settings.traffic_statistics_enabled {
                    service.poll_traffic_statistics().await;
                }
                service.flush_traffic_statistics().await;
            }
            if automatic && !service.inner.settings.read().hot_reload {
                service.set_busy("core", false);
                return;
            }
            let previous_mode = service.inner.core.applied_work_mode();
            let result = if was_running {
                service
                    .inner
                    .core
                    .reload(&model, &settings, async {
                        if cleanup_old_tun {
                            service.cleanup_tun_interface_via_helper().await;
                        }
                        if let Some(warning) = service.prepare_tun_via_helper().await {
                            service.toast(
                                ToastLevel::Warning,
                                format!("提权助手未能准备 TUN：{warning}"),
                            );
                        }
                    })
                    .await
            } else {
                service
                    .inner
                    .core
                    .prepare(&model, &settings, settings.core)
                    .await
            };
            match result {
                Ok(outcome) => {
                    let ok = service.record_outcome(&outcome);
                    if was_running && ok {
                        service.inner.traffic.lock().reset();
                        service.inner.connections.write().clear();
                        *service.inner.proxies.write() = Default::default();
                        service.finish_config_apply(&model, &settings);
                        if let Err(error) = service.update_mode_system_proxy().await {
                            service.toast(ToastLevel::Error, error.to_string());
                        }
                        if previous_mode.is_some_and(|mode| mode != model.work_mode) {
                            service.notify_work_mode(model.work_mode);
                        } else {
                            service.toast(ToastLevel::Success, "配置已应用，内核已重载");
                        }
                    } else if !was_running && ok {
                        if let Err(error) = service.update_mode_system_proxy().await {
                            service.toast(ToastLevel::Error, error.to_string());
                        }
                        service.finish_config_apply(&model, &settings);
                        service.toast(ToastLevel::Success, "配置校验通过（core 未运行）");
                    }
                }
                Err(err) => service.toast(ToastLevel::Error, format!("应用配置失败：{err}")),
            }
            service.set_busy("core", false);
        });
    }

    pub fn validate_config(&self) {
        if self.is_busy("validate") {
            return;
        }
        self.sync_model_settings();
        self.set_busy("validate", true);
        let service = self.clone();
        self.spawn(async move {
            let settings = service.inner.settings.read().clone();
            let model = service.inner.model.read().clone();
            let result = service
                .inner
                .core
                .prepare(&model, &settings, settings.core)
                .await;
            service.set_busy("validate", false);
            match result {
                Ok(outcome) => {
                    service.record_outcome(&outcome);
                    if outcome.report.ok {
                        service.toast(ToastLevel::Success, "配置校验通过");
                    }
                }
                Err(err) => service.toast(ToastLevel::Error, format!("校验失败：{err}")),
            }
        });
    }

    fn record_outcome(&self, outcome: &StartOutcome) -> bool {
        *self.inner.validation.write() = Some(outcome.report.clone());
        *self.inner.compiled.write() = Some(outcome.compiled.clone());
        *self.inner.compiled_json.write() = Some(
            outcome
                .compiled
                .to_pretty_string()
                .unwrap_or_else(|_| outcome.compiled.json.to_string()),
        );
        for warning in &outcome.compiled.warnings {
            self.inner
                .core
                .logs()
                .push(LogStream::App, LogLevel::Warn, warning.clone());
        }
        if !outcome.report.ok {
            let detail = outcome
                .report
                .issues
                .iter()
                .map(|issue| match issue.line {
                    Some(line) => format!("第 {line} 行：{}", issue.message),
                    None => issue.message.clone(),
                })
                .take(4)
                .collect::<Vec<_>>()
                .join("\n");
            self.toast(
                ToastLevel::Error,
                format!(
                    "配置校验失败：{}",
                    if detail.is_empty() {
                        outcome.report.summary()
                    } else {
                        detail
                    }
                ),
            );
        }
        outcome.report.ok
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    pub fn update_settings(&self, mut new: Settings) {
        let old = self.settings();
        if self.model().work_mode == WorkMode::Mesh {
            new.tun_enabled = old.tun_enabled;
            new.system_proxy = old.system_proxy;
        }
        if self.is_busy("autostart") && new.autostart != old.autostart {
            self.toast(ToastLevel::Info, "正在设置登录启动，请稍后重试");
            return;
        }
        if new.lan_auth && !new.lan_account.valid() {
            self.toast(ToastLevel::Warning, "请输入有效的局域网用户名和密码");
            return;
        }
        if reqwest::header::HeaderValue::from_str(&new.update_user_agent).is_err() {
            self.toast(ToastLevel::Warning, "User-Agent 含有无效字符，请重新输入");
            return;
        }
        if new.listeners_changed(&old) {
            let mut model = self.model();
            new.sync_inbounds(&mut model);
            let listeners = crate::model::inbound::effective_inbounds(&model, &new);
            if let Err(err) = crate::model::inbound::validate_listeners(&listeners, new.core, &new)
            {
                self.toast(ToastLevel::Warning, err.to_string());
                return;
            }
            let mesh = self.inner.mesh.snapshot();
            if listeners
                .iter()
                .filter_map(|i| i.listen_ports(new.core).ok())
                .flatten()
                .any(|port| {
                    mesh.profiles
                        .iter()
                        .any(|p| p.outbound_port == port && port != 0)
                })
            {
                self.toast(
                    ToastLevel::Warning,
                    "代理端口与组网出口端口冲突，请修改端口",
                );
                return;
            }
        }
        if let Err(err) = self.save_settings(&new) {
            self.toast(ToastLevel::Error, format!("保存设置失败：{err}"));
            return;
        }
        if new.system_notifications != old.system_notifications {
            if let Some(sender) = self.inner.notification_sender.read().as_ref() {
                let _ = sender.send(NotificationEvent::Configure {
                    enabled: new.system_notifications,
                    user_initiated: true,
                });
            }
        }
        if new.mixed_port != old.mixed_port
            || new.allow_lan != old.allow_lan
            || new.separate_lan_port != old.separate_lan_port
            || new.listen_host != old.listen_host
            || new.tun_enabled != old.tun_enabled
            || new.tun_strict_route != old.tun_strict_route
        {
            self.sync_model_settings();
            let _ = self.persist_model();
        }
        if new.autostart != old.autostart {
            let service = self.clone();
            let new = new.clone();
            let old = old.clone();
            self.set_busy("autostart", true);
            self.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    system::autostart::set_enabled(new.login_enabled())
                })
                .await;
                service.set_busy("autostart", false);
                if !matches!(result, Ok(Ok(()))) {
                    let mut current = service.settings();
                    current.autostart = old.autostart;
                    let _ = service.save_settings(&current);
                    service.toast(
                        ToastLevel::Error,
                        "设置登录启动失败，已恢复原设置，请检查系统权限",
                    );
                }
            });
        }
        if new.core != old.core && self.is_running() && !new.hot_reload {
            self.toast(
                ToastLevel::Warning,
                format!("已选择 {}，需重载内核生效", new.core.display_name()),
            );
        }
        if new.system_proxy != old.system_proxy {
            self.set_system_proxy(new.system_proxy);
        }
        if new.tun_enabled && !new.core.capabilities().native_tun {
            self.toast(
                ToastLevel::Warning,
                format!("{} 不支持 TUN，请更新内核", new.core.display_name()),
            );
        }
        if new.requires_core_restart(&old, &self.inner.model.read()) || new.language != old.language
        {
            self.refresh_doctor();
        }
    }

    pub fn set_language(&self, language: crate::settings::Language) {
        let mut settings = self.settings();
        let changed = settings.language != language;
        settings.language = language;
        let _ = self.save_settings(&settings);
        if changed {
            self.refresh_doctor();
        }
    }

    pub fn set_theme(&self, theme: crate::settings::ThemeMode) {
        let mut settings = self.settings();
        settings.theme = theme;
        let _ = self.save_settings(&settings);
    }

    pub fn set_backdrop(&self, backdrop: crate::settings::BackdropStyle) {
        let mut settings = self.settings();
        settings.backdrop = backdrop;
        if let Err(err) = self.save_settings(&settings) {
            self.toast(ToastLevel::Error, format!("保存窗口材质失败：{err}"));
        }
    }

    pub fn set_core(&self, core: CoreKind) {
        let mut settings = self.settings();
        settings.core = core;
        self.update_settings(settings);
    }

    /// Apply a TUN preference through the same validated restart path as the UI.
    pub fn set_tun_enabled(&self, enabled: bool) {
        let mut settings = self.settings();
        let old_tun = settings.effective_tun();
        if enabled && !settings.core.capabilities().native_tun {
            self.toast(ToastLevel::Warning, "当前内核不支持 TUN，请更新内核");
            return;
        }
        if self.is_busy("core") {
            self.toast(ToastLevel::Info, "内核正在处理，请稍后再切换 TUN");
            return;
        }
        if settings.tun_enabled == enabled {
            return;
        }
        let running = self.is_running();
        settings.tun_enabled = enabled;
        self.update_settings(settings);
        if self.settings().tun_enabled != enabled {
            return;
        }
        if running {
            self.toast(ToastLevel::Info, "正在重载内核以应用 TUN 设置");
            self.apply_config_with_tun_cleanup(old_tun, false);
        } else {
            self.toast(ToastLevel::Info, "TUN 设置已保存，下次启动内核时生效");
        }
    }

    // ------------------------------------------------------------------
    // Routing and DNS
    // ------------------------------------------------------------------

    pub fn set_route_preset(&self, preset: RoutePreset) {
        {
            let mut model = self.inner.model.write();
            model.apply_preset(preset);
            model.updated_at = now_unix();
        }
        let _ = self.persist_model();
        self.mark_dirty();
        self.toast(
            ToastLevel::Info,
            format!("路由模板已切换为“{}”，应用配置后生效", preset.label()),
        );
    }

    /// Quick routing switch used by the tray, applying through a validated restart.
    pub fn switch_route_preset(&self, preset: RoutePreset) {
        if self.is_busy("core") {
            self.toast(ToastLevel::Info, "内核正在处理，请稍后再切换路由");
            return;
        }
        if self.model().routing.preset == preset {
            return;
        }
        let running = self.is_running();
        self.set_route_preset(preset);
        if running {
            self.toast(ToastLevel::Info, "正在重载内核以应用路由设置");
            self.apply_config();
        } else {
            self.toast(ToastLevel::Info, "路由设置已保存，下次启动内核时生效");
        }
    }

    pub fn upsert_rule(&self, rule: RouteRule) {
        if let Err(error) = self.upsert_rule_in_group(rule, None) {
            self.toast(ToastLevel::Error, error.to_string());
        }
    }

    pub fn upsert_rule_in_group(&self, mut rule: RouteRule, group_id: Option<&str>) -> Result<()> {
        self.commit_model_change(|model| {
            if rule.action.kind == crate::model::RouteActionKind::Proxy {
                if let Some(id) = &rule.action.group {
                    if !model.group(id).is_some_and(|group| group.enabled) {
                        return Err(Error::config("出站组已停用或不存在，请重新选择"));
                    }
                }
            } else {
                rule.action.group = None;
            }
            if let Some(id) = group_id {
                if !model.routing.rule_groups.iter().any(|g| g.id == id) {
                    return Err(Error::config("规则分组不存在，请重新选择"));
                }
            }
            if let Some(existing) = model.routing.all_rules_mut().find(|r| r.id == rule.id) {
                *existing = rule;
            } else if let Some(id) = group_id {
                model
                    .routing
                    .rule_groups
                    .iter_mut()
                    .find(|g| g.id == id)
                    .unwrap()
                    .rules
                    .push(rule);
            } else {
                model.routing.rules.push(rule);
                model.routing.preset = RoutePreset::Custom;
            }
            Ok(true)
        })?;
        self.toast(ToastLevel::Info, "规则已保存，应用配置后生效");
        Ok(())
    }

    pub fn new_rule(&self) -> RouteRule {
        RouteRule::new(
            "新规则",
            crate::model::Matchers::default(),
            crate::model::RouteAction::proxy(),
        )
    }

    pub fn delete_rule(&self, rule_id: &str) {
        {
            let mut model = self.inner.model.write();
            model.routing.rules.retain(|r| r.id != rule_id);
            for group in &mut model.routing.rule_groups {
                group.rules.retain(|r| r.id != rule_id);
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_rule_enabled(&self, rule_id: &str, enabled: bool) {
        {
            let mut model = self.inner.model.write();
            if let Some(rule) = model.routing.all_rules_mut().find(|r| r.id == rule_id) {
                rule.enabled = enabled;
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn save_rule_group(&self, id: Option<&str>, name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::config("请填写规则分组名称"));
        }
        let mut model = self.inner.model.write();
        let group_id = if let Some(id) = id {
            let group = model
                .routing
                .rule_groups
                .iter_mut()
                .find(|g| g.id == id)
                .ok_or_else(|| Error::config("规则分组不存在"))?;
            group.name = name.into();
            id.to_string()
        } else {
            let group = crate::model::RouteRuleGroup {
                name: name.into(),
                ..Default::default()
            };
            let id = group.id.clone();
            model.routing.rule_groups.push(group);
            id
        };
        drop(model);
        self.persist_model()?;
        self.mark_dirty();
        Ok(group_id)
    }

    pub fn set_rule_group_enabled(&self, id: &str, enabled: bool) {
        if let Some(group) = self
            .inner
            .model
            .write()
            .routing
            .rule_groups
            .iter_mut()
            .find(|g| g.id == id)
        {
            group.enabled = enabled;
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn move_rule_group(&self, id: &str, offset: isize) {
        let model = self.model();
        let groups = &model.routing.rule_groups;
        if let Some(index) = groups.iter().position(|g| g.id == id) {
            let target = index.saturating_add_signed(offset).min(groups.len() - 1);
            let placement = if target < index {
                Placement::Before
            } else {
                Placement::After
            };
            if let Err(err) = self.move_rule_group_to(id, &groups[target].id, placement) {
                self.toast(
                    ToastLevel::Error,
                    format!("保存分组顺序失败，请重试：{err}"),
                );
            }
        }
    }

    /// Removing a group preserves its rules and individual enabled states.
    pub fn delete_rule_group(&self, id: &str) {
        let mut model = self.inner.model.write();
        if let Some(index) = model.routing.rule_groups.iter().position(|g| g.id == id) {
            let group = model.routing.rule_groups.remove(index);
            model
                .routing
                .rules
                .extend(group.rules.into_iter().map(|mut rule| {
                    rule.enabled &= group.enabled;
                    rule
                }));
            model.routing.preset = RoutePreset::Custom;
        }
        drop(model);
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn move_rule(&self, id: &str, group_id: Option<&str>, offset: isize) {
        let model = self.model();
        let rules = match group_id {
            Some(id) => match model.routing.rule_groups.iter().find(|g| g.id == id) {
                Some(group) => &group.rules,
                None => return,
            },
            None => &model.routing.rules,
        };
        if let Some(index) = rules.iter().position(|r| r.id == id) {
            let target = index.saturating_add_signed(offset).min(rules.len() - 1);
            let placement = if target < index {
                Placement::Before
            } else {
                Placement::After
            };
            if let Err(err) = self.move_rule_to(id, group_id, &rules[target].id, placement) {
                self.toast(
                    ToastLevel::Error,
                    format!("保存规则顺序失败，请重试：{err}"),
                );
            }
        }
    }

    pub fn assign_rule_group(&self, id: &str, group_id: Option<&str>) {
        let mut model = self.inner.model.write();
        if group_id.is_some_and(|id| !model.routing.rule_groups.iter().any(|g| g.id == id)) {
            return;
        }
        let mut rule = None;
        if let Some(index) = model.routing.rules.iter().position(|r| r.id == id) {
            rule = Some(model.routing.rules.remove(index));
        } else {
            for group in &mut model.routing.rule_groups {
                if let Some(index) = group.rules.iter().position(|r| r.id == id) {
                    rule = Some(group.rules.remove(index));
                    break;
                }
            }
        }
        if let Some(rule) = rule {
            if let Some(id) = group_id {
                model
                    .routing
                    .rule_groups
                    .iter_mut()
                    .find(|g| g.id == id)
                    .unwrap()
                    .rules
                    .push(rule);
            } else {
                model.routing.rules.push(rule);
            }
        }
        drop(model);
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_domain_strategy(&self, strategy: crate::model::DomainStrategy) {
        {
            let mut model = self.inner.model.write();
            model.routing.domain_strategy = strategy;
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_dns_preset(&self, preset: DnsPreset) {
        {
            let mut model = self.inner.model.write();
            dns::apply_preset(&mut model.dns, preset);
        }
        let _ = self.persist_model();
        self.mark_dirty();
        self.toast(
            ToastLevel::Info,
            format!("DNS 模板已切换：{}", preset.label()),
        );
    }

    pub fn set_dns_enabled(&self, enabled: bool) {
        {
            let mut model = self.inner.model.write();
            model.dns.enabled = enabled;
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_fake_ip(&self, enabled: bool) {
        {
            let mut model = self.inner.model.write();
            model.dns.fake_ip = enabled;
            if enabled && !model.dns.servers.iter().any(|s| s.is_fakeip()) {
                model.dns.servers.insert(
                    0,
                    crate::model::DnsServer {
                        tag: "fakeip".into(),
                        server_type: crate::model::DnsServerType::FakeIp,
                        address: "fakeip".into(),
                        domains: Vec::new(),
                        detour: None,
                        strategy: None,
                        expect_ips: Vec::new(),
                        client_subnet: None,
                    },
                );
            }
        }
        let _ = self.persist_model();
        self.refresh_doctor();
        self.mark_dirty();
    }

    pub fn upsert_dns_server(&self, server: crate::model::DnsServer) {
        {
            let mut model = self.inner.model.write();
            if let Some(existing) = model.dns.servers.iter_mut().find(|s| s.tag == server.tag) {
                *existing = server;
            } else {
                model.dns.servers.push(server);
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn delete_dns_server(&self, tag: &str) -> Result<()> {
        self.commit_model_change(|model| {
            if let Some(reason) = dns::deletion_reason(&model.dns, tag) {
                return Err(Error::config(reason));
            }
            let index = model
                .dns
                .servers
                .iter()
                .position(|s| s.tag == tag)
                .ok_or_else(|| Error::config("DNS 服务器已不存在，请刷新后重试"))?;
            model.dns.servers.remove(index);
            Ok(true)
        })?;
        Ok(())
    }

    pub fn set_final_dns(&self, tag: impl Into<String>) {
        {
            let mut model = self.inner.model.write();
            model.dns.final_server = Some(tag.into());
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_hosts(&self, hosts: BTreeMap<String, Vec<String>>) {
        {
            let mut model = self.inner.model.write();
            model.dns.hosts = hosts;
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn dns_issues(&self) -> Vec<String> {
        dns::validate(&self.inner.model.read().dns)
    }

    // ------------------------------------------------------------------
    // System proxy / PAC
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Privilege helper
    // ------------------------------------------------------------------

    /// Cached helper status. Never blocks: the monitor probes the pipe from a
    /// worker thread and stores the result here.
    pub fn helper_status(&self) -> HelperStatus {
        self.inner.helper.read().clone()
    }

    /// Re-probes the helper in the background and updates the snapshot.
    pub fn refresh_helper_status(&self) {
        privilege::invalidate_status();
        self.set_busy("doctor", true);
        let service = self.clone();
        self.spawn(async move {
            let status = tokio::task::spawn_blocking(privilege::status).await;
            if let Ok(status) = status {
                *service.inner.helper.write() = status;
            }
            service.refresh_doctor();
        });
    }

    /// Only GUI startup and enabling hot reload proactively check the installed helper.
    async fn check_helper_update(&self) {
        let _guard = self.inner.helper_lock.lock().await;
        if !self.inner.settings.read().hot_reload {
            return;
        }
        let service = self.clone();
        let status = tokio::task::spawn_blocking(move || {
            service.update_outdated_helper(privilege::status())
        })
        .await;
        if let Ok(status) = status {
            *self.inner.helper.write() = status;
        }
    }

    fn claim_helper_update(&self, status: &HelperStatus) -> bool {
        if !(status.supported
            && status.installed
            && status.needs_update
            && self.inner.settings.read().hot_reload)
        {
            return false;
        }
        let mut busy = self.inner.busy.write();
        if busy.contains("helper")
            || self
                .inner
                .helper_update_attempted
                .swap(true, Ordering::SeqCst)
        {
            return false;
        }
        busy.insert("helper".into());
        true
    }

    /// Called on a blocking worker with helper_lock held. A declined elevation
    /// prompt can be retried manually without prompting again on every use.
    fn update_outdated_helper(&self, status: HelperStatus) -> HelperStatus {
        if !self.claim_helper_update(&status) {
            return status;
        }
        self.toast(ToastLevel::Info, "正在更新提权助手，请完成系统授权");
        let result = privilege::update();
        privilege::invalidate_status();
        let status = privilege::status();
        self.set_busy("helper", false);
        match result {
            Ok(message) => self.toast(ToastLevel::Success, message),
            Err(err) => self.toast(
                ToastLevel::Error,
                format!("提权助手自动更新失败，可在设置中重试：{err}"),
            ),
        }
        status
    }

    pub fn install_helper(&self) {
        self.run_helper_action("安装提权助手", privilege::install);
    }

    pub fn update_helper(&self) {
        self.run_helper_action("更新提权助手", privilege::update);
    }

    pub fn uninstall_helper(&self) {
        self.run_helper_action("卸载提权助手", privilege::uninstall);
    }

    pub fn restart_helper(&self) {
        self.run_helper_action("重启提权助手", privilege::restart);
    }

    pub fn start_helper(&self) {
        self.run_helper_action("启动提权助手", privilege::start);
    }

    /// Runs install/uninstall/start in a blocking task: they may show an OS
    /// elevation prompt and must never block the render thread.
    fn run_helper_action(&self, label: &'static str, action: fn() -> Result<String>) {
        if !self.inner.busy.write().insert("helper".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.helper_lock.lock().await;
            let result = tokio::task::spawn_blocking(move || {
                let outcome = action();
                privilege::invalidate_status();
                // Refresh before returning so the settings card updates
                // immediately instead of waiting for the next monitor tick.
                (outcome, privilege::status())
            })
            .await;
            match result {
                Ok((Ok(message), status)) => {
                    *service.inner.helper.write() = status;
                    service.toast(ToastLevel::Success, format!("{label}完成：{message}"));
                }
                Ok((Err(err), status)) => {
                    *service.inner.helper.write() = status;
                    service.toast(ToastLevel::Error, format!("{label}失败：{err}"));
                }
                Err(err) => {
                    service.toast(ToastLevel::Error, format!("{label}任务异常：{err}"));
                }
            }
            service.set_busy("helper", false);
            if service.inner.network_report.read().is_some() {
                service.refresh_network_report();
            } else {
                service.refresh_doctor();
            }
        });
    }

    /// TUN interface name from the model, used for helper requests.
    fn tun_interface(&self) -> String {
        if self.model().work_mode == WorkMode::Mesh {
            return "xrose-tun".into();
        }
        self.inner
            .model
            .read()
            .inbounds
            .iter()
            .find(|inbound| inbound.kind == InboundKind::Tun && inbound.enabled)
            .and_then(|inbound| inbound.tun.interface_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "xrose-tun".to_string())
    }

    /// Ensures the privileged helper is running before spawning an elevated TUN core.
    fn runtime_uses_tun(&self) -> bool {
        self.model().work_mode == WorkMode::Mesh || self.inner.settings.read().effective_tun()
    }

    async fn ensure_tun_helper(&self) -> std::result::Result<(), String> {
        if !self.runtime_uses_tun() || privilege::is_elevated() {
            return Ok(());
        }
        self.ensure_helper().await
    }

    async fn ensure_helper(&self) -> std::result::Result<(), String> {
        let _guard = self.inner.helper_lock.lock().await;
        let service = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let current = privilege::status();
            if current.running {
                return (Ok(String::new()), service.update_outdated_helper(current));
            }
            privilege::invalidate_status();
            let current = privilege::status();
            if current.running {
                return (Ok(String::new()), service.update_outdated_helper(current));
            }
            let outcome = if current.installed {
                service.toast(ToastLevel::Info, "正在启动提权助手…");
                privilege::start()
            } else {
                service.toast(ToastLevel::Info, "正在安装提权助手，请完成系统授权");
                privilege::install()
            };
            (outcome, service.update_outdated_helper(privilege::status()))
        })
        .await;
        match result {
            Ok((_, status)) if status.running => {
                *self.inner.helper.write() = status;
                Ok(())
            }
            Ok((Err(err), status)) => {
                *self.inner.helper.write() = status;
                Err(format!("此操作需要提权助手运行：{err}"))
            }
            Ok((Ok(message), status)) => {
                *self.inner.helper.write() = status;
                Err(format!(
                    "此操作需要提权助手运行：{}",
                    if message.is_empty() {
                        "提权助手未能启动，请在设置 → 系统集成中检查助手状态"
                    } else {
                        &message
                    }
                ))
            }
            Err(err) => Err(format!("启动提权助手任务异常：{err}")),
        }
    }

    /// Asks the helper to prepare privileges for TUN. Returns a warning only
    /// when the helper is running but the request failed; a missing helper is
    /// not an error because the core still creates its own device.
    async fn prepare_tun_via_helper(&self) -> Option<String> {
        if !self.runtime_uses_tun() || !privilege::status().running {
            return None;
        }
        let request = HelperRequest::PrepareTun {
            interface: self.tun_interface(),
        };
        match tokio::task::spawn_blocking(move || privilege::call(&request)).await {
            Ok(Ok(response)) if response.is_ok() => {
                self.inner.core.logs().push(
                    LogStream::App,
                    LogLevel::Info,
                    format!("提权助手：{}", response.message()),
                );
                None
            }
            Ok(Ok(response)) => Some(response.message().to_string()),
            Ok(Err(err)) => Some(err.to_string()),
            Err(err) => Some(format!("调用提权助手失败：{err}")),
        }
    }

    async fn cleanup_tun_via_helper(&self) {
        if !self.runtime_uses_tun() && !self.inner.core.native_tun_active() {
            return;
        }
        self.cleanup_tun_interface_via_helper().await;
    }

    async fn cleanup_tun_interface_via_helper(&self) {
        if !privilege::status().running {
            return;
        }
        let request = HelperRequest::RemoveTun {
            interface: self.tun_interface(),
        };
        let _ = tokio::task::spawn_blocking(move || privilege::call(&request)).await;
    }

    pub fn refresh_doctor(&self) {
        let mut model = self.inner.model.read().clone();
        let mut settings = self.inner.settings.read().clone();
        if model.work_mode == WorkMode::Mesh {
            if let Some(runtime) = self.inner.core.mesh_exit_runtime() {
                if let Ok((effective, runtime_settings)) =
                    crate::adapters::mesh_runtime_model(&model, &settings, &runtime)
                {
                    model = effective;
                    settings = runtime_settings;
                }
            }
        }
        let state = self.inner.core.state();
        let pid = state.phase.is_alive().then_some(state.pid).flatten();
        let generation = self.inner.doctor_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.set_busy("doctor", true);
        let service = self.clone();
        self.spawn(async move {
            let checks = tokio::task::spawn_blocking(move || {
                system::tun::diagnose_for_process(
                    &model,
                    &settings,
                    pid,
                    state.helper_session.as_deref(),
                )
            })
            .await;
            if service.inner.doctor_generation.load(Ordering::SeqCst) == generation {
                if let Ok(checks) = checks {
                    *service.inner.doctor.write() = checks;
                }
                service.set_busy("doctor", false);
            }
        });
    }

    // ------------------------------------------------------------------
    // Binaries, updates, rule-sets
    // ------------------------------------------------------------------

    pub fn adopt_binary(&self, core: CoreKind, version: &str, path: &std::path::Path) {
        match self
            .inner
            .core
            .binaries()
            .adopt_existing(core, version, path)
        {
            Ok(record) => {
                let mut settings = self.inner.settings.read().clone();
                settings
                    .active_versions
                    .insert(core.as_str().into(), record.version.clone());
                let _ = self.save_settings(&settings);
                self.toast(
                    ToastLevel::Success,
                    format!(
                        "已登记 {} {}（{}）",
                        core.display_name(),
                        record.version,
                        path.display()
                    ),
                );
            }
            Err(err) => self.toast(ToastLevel::Error, format!("登记 core 失败：{err}")),
        }
    }

    pub fn remove_binary(&self, core: CoreKind, version: &str) {
        match self.inner.core.binaries().remove(core, version) {
            Ok(()) => {
                let mut settings = self.inner.settings.read().clone();
                if settings
                    .active_version(core)
                    .map(|v| v == version)
                    .unwrap_or(false)
                {
                    settings.active_versions.remove(core.as_str());
                    let _ = self.save_settings(&settings);
                }
                self.toast(
                    ToastLevel::Info,
                    format!("已删除 {} {}", core.display_name(), version),
                );
            }
            Err(err) => self.toast(ToastLevel::Error, format!("删除失败：{err}")),
        }
    }

    pub fn set_active_version(&self, core: CoreKind, version: &str) -> Result<()> {
        let version = version.trim().trim_start_matches('v');
        if self
            .inner
            .core
            .binaries()
            .resolve(core, Some(version))
            .is_none()
        {
            return Err(Error::binary("此内核版本尚未安装，请重新选择或下载"));
        }
        let mut settings = self.inner.settings.read().clone();
        settings
            .active_versions
            .insert(core.as_str().into(), version.to_string());
        self.save_settings(&settings)?;
        self.toast(
            ToastLevel::Info,
            format!("{} 将使用版本 {}", core.display_name(), version),
        );
        Ok(())
    }

    fn refresh_local_binaries(&self) {
        use crate::core::binary::{LocalBinaryStatus, probe_binary_info};
        let settings = self.inner.settings.read();
        for core in CoreKind::all() {
            let current = LocalBinaryStatus::discover(core, settings.local_binary_path(core));
            let cache = self.inner.local_binaries.clone();
            let mut rows = cache.write();
            let row = rows.entry(core.as_str().to_owned()).or_default();
            if !row.same_file(&current) {
                *row = current;
            }
            if !self.inner.startup_started.load(Ordering::Acquire) || !row.available {
                continue;
            }
            if settings.active_version(core).is_none()
                && let Some(path) = row.path.as_deref()
            {
                let _ = self.inner.core.binaries().register_local(core, path);
            }
            if row.probing || row.info.is_some() {
                continue;
            }
            let Some(path) = row.path.clone() else {
                continue;
            };
            row.probing = true;
            let pending = row.clone();
            drop(rows);
            self.spawn(async move {
                let info = probe_binary_info(core, &path).await;
                if let Some(row) = cache.write().get_mut(core.as_str()) {
                    if row.same_file(&pending) {
                        row.info = Some(info);
                        row.probing = false;
                    }
                }
            });
        }
    }

    pub fn select_local_binary(&self, core: CoreKind) -> Result<()> {
        let mut settings = self.settings();
        let local = crate::core::binary::LocalBinaryStatus::discover(
            core,
            settings.local_binary_path(core),
        );
        if !local.available {
            return Err(Error::binary(
                "找不到可执行文件，请先选择文件或将内核加入 PATH",
            ));
        }
        if let Some(path) = local.path.as_deref() {
            self.inner.core.binaries().register_local(core, path)?;
        }
        if settings.active_version(core).is_none() {
            return Ok(());
        }
        settings.active_versions.remove(core.as_str());
        self.save_settings(&settings)
    }

    /// Picking a file activates the local row. Clearing only changes that row's file.
    pub fn set_binary_override(&self, core: CoreKind, path: Option<String>) -> Result<()> {
        let mut settings = self.settings();
        if let Some(path) = path {
            let file = std::path::Path::new(&path);
            if !file.is_absolute() || !crate::core::binary::is_executable(file) {
                return Err(Error::binary("请选择存在且可执行的内核文件"));
            }
            self.inner.core.binaries().register_local(core, file)?;
            settings.binary_paths.insert(core.as_str().into(), path);
            settings.active_versions.remove(core.as_str());
        } else {
            settings.binary_paths.remove(core.as_str());
            if settings.active_version(core).is_none()
                && let Some(path) = crate::core::binary::path_binary(core)
            {
                let _ = self.inner.core.binaries().register_local(core, &path);
            } else {
                let _ = self.inner.core.binaries().unregister_local(core);
            }
        }
        self.save_settings(&settings)?;
        self.refresh_local_binaries();
        Ok(())
    }

    pub fn install_core_update(&self, core: CoreKind) {
        if !self.inner.busy.write().insert("install".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let _install = service.inner.install_lock.lock().await;
            let settings = service.settings();
            let network = service.resource_network();
            let candidate = match service
                .core_download_candidate(core, &settings, &network)
                .await
            {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
                    service.record_core_check(core, &settings, Ok(None));
                    service.set_busy("install", false);
                    service.toast(
                        ToastLevel::Info,
                        format!("{} 已是最新版本", core.display_name()),
                    );
                    return;
                }
                Err(err) => {
                    service.set_busy("install", false);
                    service.toast(ToastLevel::Error, format!("检查更新失败：{err}"));
                    return;
                }
            };
            let checksum = match &candidate.checksum_url {
                Some(url) => match updates::download_checksum(url, &network).await {
                    Ok(checksum) => checksum,
                    Err(err) => {
                        service.set_busy("install", false);
                        service.toast(ToastLevel::Error, format!("获取校验文件失败：{err}"));
                        return;
                    }
                },
                None => None,
            };
            service.toast(
                ToastLevel::Info,
                format!("正在下载 {} {}…", core.display_name(), candidate.version),
            );
            let result = service
                .inner
                .core
                .binaries()
                .install_from_url(
                    core,
                    &candidate.version,
                    &candidate.asset.url,
                    checksum,
                    &network,
                )
                .await;
            match result {
                Ok(record) => {
                    let mut settings = service.inner.settings.read().clone();
                    settings
                        .active_versions
                        .insert(core.as_str().into(), record.version.clone());
                    if let Err(err) = service.save_settings(&settings) {
                        service.set_busy("install", false);
                        service.toast(ToastLevel::Error, format!("内核已下载，但启用失败：{err}"));
                        return;
                    }
                    service.record_core_check(core, &settings, Ok(None));
                    service.toast(
                        ToastLevel::Success,
                        format!("{} {} 安装完成", core.display_name(), record.version),
                    );
                }
                Err(err) => service.toast(ToastLevel::Error, format!("安装失败：{err}")),
            }
            service.set_busy("install", false);
        });
    }

    // Callers serialize manual and startup downloads so cache files and row results stay in order.
    async fn download_rule_sets(&self, sets: &[crate::model::RuleSet]) -> Vec<RuleSetUpdate> {
        updates::sync_rule_sets_reporting(
            sets,
            &self.inner.paths.rule_sets_dir(),
            &self.resource_network(),
            |result| {
                self.inner
                    .rule_set_updates
                    .write()
                    .insert(result.tag.clone(), result.clone());
            },
        )
        .await
    }

    /// Changes future downloads without interrupting the running core or removing cached files.
    pub fn set_rule_set_source(
        &self,
        source: crate::model::RuleSetSource,
        custom: Option<&str>,
    ) -> Result<()> {
        let busy_error = || Error::config("规则集正在使用，请在更新或内核操作完成后重试");
        let _lock = self
            .inner
            .rule_set_lock
            .try_lock()
            .map_err(|_| busy_error())?;
        if self.is_busy("rulesets") || self.is_busy("core") {
            return Err(busy_error());
        }
        let mut changed_tags = Vec::new();
        if self.save_model_change(|model| {
            let template = if source == crate::model::RuleSetSource::Custom {
                crate::model::route::rule_set_source_template(
                    custom.unwrap_or(&model.routing.custom_rule_set_template),
                )?
            } else {
                model.routing.custom_rule_set_template.clone()
            };
            if model.routing.rule_set_source == source
                && template == model.routing.custom_rule_set_template
            {
                return Ok(false);
            }
            let previous = crate::adapters::downloadable_rule_sets(model);
            model.routing.rule_set_source = source;
            model.routing.custom_rule_set_template = template;
            model.updated_at = now_unix();
            changed_tags.extend(
                previous
                    .into_iter()
                    .filter(|set| {
                        crate::adapters::resolve_rule_set(model, &set.tag)
                            .is_some_and(|next| next.url != set.url)
                    })
                    .map(|set| set.tag),
            );
            Ok(true)
        })? {
            let mut updates = self.inner.rule_set_updates.write();
            for tag in changed_tags {
                if let Some(result) = updates.get_mut(&tag) {
                    result.status = "cached".into();
                    result.error = None;
                }
            }
        }
        Ok(())
    }

    pub fn sync_rule_sets(&self) {
        if !self.inner.busy.write().insert("rulesets".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let _lock = service.inner.rule_set_lock.lock().await;
            let sets = crate::adapters::downloadable_rule_sets(&service.inner.model.read());
            let results = service.download_rule_sets(&sets).await;
            let updated = results.iter().filter(|r| r.status == "updated").count();
            let failed = results.iter().filter(|r| r.status == "failed").count();
            let unchanged = results
                .iter()
                .filter(|r| r.status == "not-modified")
                .count();
            service.set_busy("rulesets", false);
            service.toast(
                if failed > 0 {
                    ToastLevel::Warning
                } else {
                    ToastLevel::Success
                },
                format!("规则集更新完成：更新 {updated}，无变化 {unchanged}，失败 {failed}"),
            );
        });
    }

    // ------------------------------------------------------------------
    // Logs
    // ------------------------------------------------------------------

    pub fn logs_filtered(&self, level: LogLevel, query: &str, limit: usize) -> Vec<LogEntry> {
        self.inner.core.logs().recent(limit, level, query)
    }

    pub fn logs_matching(&self, filter: &LogFilter, query: &str, limit: usize) -> Vec<LogEntry> {
        self.logs()
            .recent_matching(limit, query, |entry| filter.matches(entry))
    }

    pub fn clear_logs(&self) {
        self.inner.core.logs().clear();
        self.toast(ToastLevel::Info, "日志已清空（磁盘日志仍保留）");
    }

    fn log_redactor(&self, privacy: LogPrivacy) -> Result<Option<LogRedactor>> {
        if privacy == LogPrivacy::Original {
            return Ok(None);
        }
        let mut rules = RedactionRules::default();
        {
            let model = self.inner.model.read();
            if model.dns.enabled && model.dns.fake_ip {
                rules.fake_ip = model.dns.fake_ip_range.trim().parse().ok();
            }
            for node in &model.nodes {
                rules.node(node);
            }
        }
        for sub in self.inner.subs.read().iter() {
            rules.subscription(sub);
        }
        self.inner.mesh.add_log_redactions(&mut rules);
        rules.secret(&self.inner.settings.read().lan_account.password);
        let paths = &self.inner.paths;
        rules.path(&paths.data_dir, "$XROSE_HOME");
        rules.path(&paths.config_dir, "$XROSE_HOME/config");
        rules.path(&paths.cache_dir, "$XROSE_HOME/cache");
        rules.path(&paths.runtime_dir, "$XROSE_HOME/run");
        for var in ["HOME", "USERPROFILE"] {
            if let Some(home) = std::env::var_os(var) {
                rules.path(std::path::Path::new(&home), "~");
            }
        }
        rules.build().map(Some)
    }

    /// Formats the selected rows in display order, preserving their original timestamps.
    pub fn logs_for_clipboard(&self, entries: &[LogEntry], privacy: LogPrivacy) -> Result<String> {
        use std::fmt::Write as _;
        let redactor = self.log_redactor(privacy)?;
        let mut text = String::new();
        for entry in entries {
            if !text.is_empty() {
                text.push('\n');
            }
            let message = redactor.as_ref().map(|r| r.message(&entry.message));
            write!(
                text,
                "{} {} {} {}",
                entry.timestamp,
                entry.level.as_str().to_ascii_uppercase(),
                entry.stream.label(),
                message.as_deref().unwrap_or(&entry.message)
            )
            .expect("writing to a String cannot fail");
        }
        Ok(text)
    }

    /// Exports the in-memory log buffer to `logs/exports/`.
    pub fn export_logs(&self) -> Result<String> {
        self.export_logs_with_privacy(LogPrivacy::Original)
    }

    pub fn export_logs_with_privacy(&self, privacy: LogPrivacy) -> Result<String> {
        let redactor = self.log_redactor(privacy)?;
        let suffix = if redactor.is_some() { "-redacted" } else { "" };
        let dir = self.inner.paths.logs_dir().join("exports");
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::other(format!("创建日志目录失败：{e}")))?;
        let path = dir.join(format!(
            "xrose-log-{}{suffix}.log",
            chrono::Utc::now().format("%Y%m%d-%H%M%S-%f")
        ));
        self.logs().export_with(&path, redactor.as_ref())?;
        self.toast(
            ToastLevel::Success,
            format!("日志已导出：{}", path.display()),
        );
        Ok(path.display().to_string())
    }

    // ------------------------------------------------------------------
    // Runtime monitoring
    // ------------------------------------------------------------------

    pub fn start_recording(&self) {
        let connections = self.inner.connections.read();
        let mut recording = self.inner.recording.lock();
        if recording.active {
            return;
        }
        let now = now_unix();
        recording.start(now);
        recording.observe(&connections.rows, now);
        self.toast(
            ToastLevel::Info,
            "已开始录制连接元数据和测试结果；不采集请求正文",
        );
    }

    pub fn stop_recording(&self) {
        self.inner.recording.lock().stop(now_unix());
        self.toast(ToastLevel::Info, "录制已停止，记录保留到清空或退出应用");
    }

    pub fn clear_recording(&self) {
        *self.inner.recording.lock() = DiagnosticRecording::default();
    }

    pub fn export_recording(&self) {
        if self.is_busy("recording-export") {
            return;
        }
        self.set_busy("recording-export", true);
        let recording = self.inner.recording.lock().clone();
        let path = self.inner.paths.logs_dir().join(format!(
            "connections-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f")
        ));
        let service = self.clone();
        self.spawn(async move {
            let result = async {
                let json = serde_json::to_vec_pretty(&recording)?;
                tokio::fs::write(&path, json).await?;
                Ok::<_, Error>(())
            }
            .await;
            match result {
                Ok(()) => service.toast(
                    ToastLevel::Success,
                    format!("录制已导出：{}", path.display()),
                ),
                Err(err) => service.toast(ToastLevel::Error, format!("导出录制失败：{err}")),
            }
            service.set_busy("recording-export", false);
        });
    }

    fn update_connections(
        &self,
        epoch: Option<crate::traffic_statistics::CoreEpoch>,
        connections: Vec<ClashConnection>,
    ) {
        // Aggregate and rank off the snapshot lock: rendering only clones two Arcs.
        let live = {
            let mut tracking = self.inner.rate_tracker.lock();
            let (previous_epoch, tracker) = &mut *tracking;
            if *previous_epoch != epoch {
                *tracker = Default::default();
                *previous_epoch = epoch.clone();
            }
            tracker.observe(
                connections.iter().map(crate::flow::FlowObservation::from),
                std::time::Instant::now(),
                chrono::Utc::now(),
            )
        };
        if epoch != CoreEpoch::from_state(&self.process_state()) {
            return;
        }
        let mut current = self.inner.connections.write();
        self.inner
            .recording
            .lock()
            .observe(&connections, now_unix());
        *current = ConnectionState {
            rows: Arc::new(connections),
            live: Arc::new(live),
        };
    }

    /// Insert an exact diagnostic override before broader preset rules.
    pub fn add_diagnostic_rule(
        &self,
        target: &DiagnosticTarget,
        kind: DiagnosticRuleKind,
        action: crate::model::RouteAction,
    ) -> Result<()> {
        self.save_diagnostic_rule(target, kind, action)?;
        self.toast(
            ToastLevel::Success,
            if self.model().work_mode == WorkMode::Mesh {
                "规则已保存至本机；组网模式暂停本地代理分流，远端出口规则未修改"
            } else {
                "规则已置顶保存，点击应用配置后重新测试"
            },
        );
        Ok(())
    }

    /// Firewall shortcuts apply the saved rule immediately to the running core.
    pub fn block_connection_target(&self, target: DiagnosticTarget, kind: DiagnosticRuleKind) {
        if !self.inner.busy.write().insert("firewall-rule".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let writer = service.clone();
            let result = tokio::task::spawn_blocking(move || {
                writer.save_diagnostic_rule(&target, kind, crate::model::RouteAction::block())
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    service.toast(ToastLevel::Success, "阻止联网规则已保存");
                    if service.is_running() {
                        service.apply_config();
                    }
                }
                Ok(Err(err)) => service.toast(ToastLevel::Error, err.to_string()),
                Err(_) => service.toast(ToastLevel::Error, "未能保存规则，请重试"),
            }
            service.set_busy("firewall-rule", false);
        });
    }

    fn save_diagnostic_rule(
        &self,
        target: &DiagnosticTarget,
        kind: DiagnosticRuleKind,
        action: crate::model::RouteAction,
    ) -> Result<()> {
        let matchers = target.matchers(kind)?;
        let mut model = self.inner.model.write();
        if let Some(group) = &action.group {
            if !model.group(group).is_some_and(|group| group.enabled) {
                return Err(Error::config("所选出站组已不存在或已停用，请重新选择"));
            }
        }
        let mut next = model.clone();
        if let Some(index) = next
            .routing
            .rules
            .iter()
            .position(|rule| rule.matchers == matchers && rule.action == action)
        {
            let mut rule = next.routing.rules.remove(index);
            rule.enabled = true;
            next.routing.rules.insert(0, rule);
        } else {
            next.routing.rules.insert(
                0,
                RouteRule::new(format!("诊断 · {}", target.value(kind)), matchers, action),
            );
        }
        if !next.routing.rule_groups.is_empty() {
            let rule = next.routing.rules.remove(0);
            let index = next
                .routing
                .rule_groups
                .iter()
                .position(|g| g.id == "diagnostic-overrides");
            let mut group = index
                .map(|i| next.routing.rule_groups.remove(i))
                .unwrap_or_else(|| crate::model::RouteRuleGroup {
                    id: "diagnostic-overrides".into(),
                    name: "诊断规则".into(),
                    ..Default::default()
                });
            group.enabled = true;
            group
                .rules
                .retain(|r| r.matchers != rule.matchers || r.action != rule.action);
            group.rules.insert(0, rule);
            next.routing.rule_groups.insert(0, group);
        }
        next.routing.preset = RoutePreset::Custom;
        next.updated_at = now_unix();
        let profile = self.inner.store.save_active_profile(&next)?;
        *self.inner.active_profile.write() = Some(profile);
        *model = next;
        drop(model);
        self.mark_dirty();
        Ok(())
    }

    pub fn refresh_connections(&self) {
        if !self.is_running() || !self.inner.busy.write().insert("connections-refresh".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            service.poll_traffic_statistics().await;
            service.set_busy("connections-refresh", false);
        });
    }

    pub fn close_connection(&self, id: &str) {
        if !id.is_empty() {
            self.close_live_connections(Some(id.to_owned()));
        }
    }

    pub fn close_all_connections(&self) {
        self.close_live_connections(None);
    }

    pub fn kill_connection(&self, id: &str) {
        self.close_connection(id);
    }

    pub fn kill_all_connections(&self) {
        self.close_all_connections();
    }

    fn close_live_connections(&self, id: Option<String>) {
        let epoch = CoreEpoch::from_state(&self.process_state());
        if epoch
            .as_ref()
            .is_none_or(|epoch| epoch.core != CoreKind::SingBox)
        {
            self.toast(ToastLevel::Warning, "请先启动支持连接管理的 sing-box 内核");
            return;
        }
        let key = "connection-close";
        if !self.inner.busy.write().insert(key.into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let result = async {
                let (_, settings, _) = service.inner.core.statistics_context()?;
                if epoch != CoreEpoch::from_state(&service.process_state()) {
                    return None;
                }
                let client = ClashClient::new(settings.api_port, "");
                Some(match id {
                    Some(id) => client.close_connection(&id).await,
                    None => client.close_all_connections().await,
                })
            }
            .await;
            match result {
                Some(Ok(())) => {
                    service.toast(ToastLevel::Info, "所选连接已断开");
                    service.poll_traffic_statistics().await;
                }
                Some(Err(err)) => {
                    service.toast(ToastLevel::Warning, format!("断开连接失败：{err}"))
                }
                None => service.toast(ToastLevel::Info, "内核状态已改变，请刷新连接后重试"),
            }
            service.set_busy(key, false);
        });
    }

    /// Change only the selected listener; a connected worker confirms the update before it is saved.
    pub fn edit_mesh_forward(
        &self,
        profile: &str,
        old: Option<&crate::mesh::PortForward>,
        rule: Option<crate::mesh::PortForward>,
    ) -> Result<()> {
        self.edit_mesh_forward_rule(
            profile,
            old.cloned().map(crate::mesh::ForwardRule::ToMesh).as_ref(),
            rule.map(crate::mesh::ForwardRule::ToMesh),
        )
    }

    pub fn edit_mesh_forward_rule(
        &self,
        profile: &str,
        old: Option<&crate::mesh::ForwardRule>,
        rule: Option<crate::mesh::ForwardRule>,
    ) -> Result<()> {
        let (old, next) = self.inner.mesh.forward_edit(profile, old, rule)?;
        if old == next {
            return Ok(());
        }
        if !self.inner.busy.write().insert("mesh".into()) {
            return Err(Error::other("正在处理组网操作，请稍后重试"));
        }
        self.set_busy("mesh-forward", true);
        let service = self.clone();
        let profile = profile.to_owned();
        self.spawn(async move {
            let _guard = service.inner.mesh.operation.lock().await;
            let result = service
                .inner
                .mesh
                .apply_forward_edit(&profile, &old, &next)
                .await;
            service.set_busy("mesh-forward", false);
            service.set_busy("mesh", false);
            match result {
                Ok(()) => service.toast(ToastLevel::Success, "端口转发已更新"),
                Err(e) => service.toast(ToastLevel::Error, e.to_string()),
            }
        });
        Ok(())
    }

    pub fn save_mesh_profile(&self, profile: crate::mesh::MeshProfile) -> Result<String> {
        self.save_mesh_profile_inner(profile, None)
    }

    /// Uses the same live acknowledgement and persistence transaction as local forwarding.
    pub fn edit_mesh_service(
        &self,
        id: &str,
        old: Option<&crate::mesh::ServiceForward>,
        rule: Option<crate::mesh::ServiceForward>,
    ) -> Result<()> {
        self.edit_mesh_forward_rule(
            id,
            old.cloned().map(crate::mesh::ForwardRule::ToHost).as_ref(),
            rule.map(crate::mesh::ForwardRule::ToHost),
        )
    }

    pub fn save_mesh_profile_with_startup(
        &self,
        profile: crate::mesh::MeshProfile,
        connect_on_startup: bool,
    ) -> Result<String> {
        self.save_mesh_profile_inner(profile, Some(connect_on_startup))
    }

    fn save_mesh_profile_inner(
        &self,
        profile: crate::mesh::MeshProfile,
        connect_on_startup: Option<bool>,
    ) -> Result<String> {
        if self.is_busy("mesh-exit") {
            return Err(Error::other("正在切换组网出口，请稍后再编辑配置"));
        }
        if profile.outbound_port != 0
            && mesh_exit::proxy_ports(&self.model(), &self.settings())
                .contains(&profile.outbound_port)
        {
            return Err(Error::config(
                "出口出站端口与代理或管理端口冲突，请使用其他端口",
            ));
        }
        let provider = profile.provider;
        let auto_reconnect = profile.auto_reconnect;
        let id = self.inner.mesh.save(profile)?;
        if !auto_reconnect {
            self.inner.mesh_reconnect.lock().forget(&id);
        }
        self.inner
            .mesh
            .sync_outbounds(&mut self.inner.model.write());
        self.persist_model()?;
        self.mark_dirty();
        if let Some(enabled) = connect_on_startup {
            let mut settings = self.settings();
            settings.set_mesh_startup_profile(provider, &id, enabled);
            self.save_settings(&settings).map_err(|err| {
                Error::other(format!("组网配置已保存，但应用启动设置保存失败：{err}"))
            })?;
        }
        self.toast(ToastLevel::Success, "组网配置已保存");
        Ok(id)
    }

    pub fn delete_mesh_profile(&self, id: &str) -> Result<()> {
        if self.is_busy("mesh-exit") {
            return Err(Error::other("正在切换组网出口，请稍后再删除配置"));
        }
        self.inner.mesh.delete(id)?;
        self.inner.mesh_reconnect.lock().forget(id);
        self.inner.mesh_connection_errors.write().remove(id);
        self.inner
            .model
            .write()
            .remove_node(&format!("mesh-exit-{id}"));
        self.persist_model()?;
        self.mark_dirty();
        let mut settings = self.settings();
        if settings.startup_tailscale.as_deref() == Some(id) {
            settings.startup_tailscale = None;
        }
        if settings.startup_easytier.as_deref() == Some(id) {
            settings.startup_easytier = None;
        }
        self.update_settings(settings);
        Ok(())
    }

    pub fn mesh_connect(&self, id: &str) {
        self.mesh_action("connect", id.to_string(), None);
    }
    pub fn mesh_disconnect(&self, provider: crate::mesh::MeshProvider) {
        self.mesh_action("disconnect", String::new(), Some(provider));
    }
    pub fn mesh_logout(&self, provider: crate::mesh::MeshProvider) {
        self.mesh_action("logout", String::new(), Some(provider));
    }
    pub fn mesh_probe(&self, provider: crate::mesh::MeshProvider, ip: String) {
        self.mesh_action("probe", ip, Some(provider));
    }
    pub fn mesh_install_tailscale(&self) {
        self.mesh_action("install", String::new(), None);
    }

    fn mesh_action(
        &self,
        action: &'static str,
        value: String,
        provider: Option<crate::mesh::MeshProvider>,
    ) {
        if !self.inner.busy.write().insert("mesh".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let _operation = service.inner.mesh.operation.lock().await;
            let result: Result<()> = async {
                match action {
                    "connect" => {
                        service.inner.mesh_connection_errors.write().remove(&value);
                        let profile = service.inner.mesh.profile(&value)?;
                        service.inner.mesh_reconnect.lock().supersede(
                            &service.inner.mesh.snapshot(),
                            profile.provider,
                            &value,
                        );
                        if !service.inner.mesh.snapshot().supported {
                            return Err(Error::other("当前平台不支持组网连接"));
                        }
                        if let Err(error) = service.ensure_helper().await {
                            if service.settings().mesh_startup_profile(profile.provider)
                                == Some(value.as_str())
                            {
                                service.inner.mesh_reconnect.lock().remember(
                                    profile.provider,
                                    &value,
                                    profile.auto_reconnect,
                                );
                            }
                            return Err(Error::other(error));
                        }
                        if service.inner.helper.read().needs_update {
                            service.inner.mesh.verify_protocol().await?;
                        }
                        if service.inner.mesh.snapshot().profiles.iter().any(|p| {
                            p.id == value && p.provider == crate::mesh::MeshProvider::Tailscale
                        }) && crate::mesh::tailscale::verify_binary(
                            &crate::mesh::tailscale::archive_path(&service.inner.paths),
                        )
                        .is_err()
                        {
                            service.toast(ToastLevel::Info, "正在下载 Tailscale 组网组件…");
                            service
                                .inner
                                .mesh
                                .install(&service.resource_network())
                                .await?;
                        }
                        let provider = profile.provider;
                        let reconnect = profile.auto_reconnect;
                        service.connect_mesh_worker(profile).await?;
                        service
                            .inner
                            .mesh_reconnect
                            .lock()
                            .remember(provider, &value, reconnect);
                        return Ok(());
                    }
                    "disconnect" | "logout" => {
                        let provider = provider.unwrap();
                        if service
                            .model()
                            .mesh_mode
                            .target
                            .as_ref()
                            .is_some_and(|t| t.provider == provider)
                        {
                            service
                                .inner
                                .mesh_takeover_requested
                                .store(false, Ordering::Release);
                        }
                        let gateway_profiles: Vec<_> = service
                            .inner
                            .mesh
                            .snapshot()
                            .networks
                            .iter()
                            .filter(|n| n.provider == provider)
                            .map(|n| n.profile_id.clone())
                            .collect();
                        service
                            .inner
                            .mesh_reconnect
                            .lock()
                            .forget_provider(provider);
                        if action == "logout" {
                            service.inner.mesh.logout(provider).await?;
                        } else {
                            service.inner.mesh.disconnect(provider).await?;
                        }
                        for id in gateway_profiles {
                            service.stop_exit_gateway(&id).await;
                        }
                    }
                    "probe" => {
                        service
                            .inner
                            .mesh
                            .probe(provider.unwrap(), value.clone())
                            .await?
                    }
                    "install" => {
                        service
                            .inner
                            .mesh
                            .install(&service.resource_network())
                            .await?
                    }
                    _ => unreachable!(),
                }
                service.reconcile_mesh().await
            }
            .await;
            match result {
                Ok(()) if action == "connect" => {
                    let state = service
                        .inner
                        .mesh
                        .snapshot()
                        .networks
                        .into_iter()
                        .find(|state| state.profile_id == value);
                    match state {
                        Some(state) if state.phase == crate::mesh::MeshPhase::NeedsLogin => service
                            .toast(
                                ToastLevel::Info,
                                if state.login_url.is_some() {
                                    "请点击“在浏览器登录”完成 Tailscale 设备授权"
                                } else {
                                    "正在获取 Tailscale 登录链接"
                                },
                            ),
                        Some(state) if state.phase == crate::mesh::MeshPhase::NeedsApproval => {
                            service.toast(ToastLevel::Info, "请联系目标网络的管理员批准此设备")
                        }
                        Some(state) if state.phase == crate::mesh::MeshPhase::Connected => {
                            service.toast(ToastLevel::Success, "组网已连接")
                        }
                        other => service.mesh_connection_failed(
                            &value,
                            other
                                .and_then(|state| state.error)
                                .unwrap_or_else(|| "未能确认组网连接，请重试".into()),
                        ),
                    }
                }
                Ok(()) => service.toast(
                    ToastLevel::Success,
                    match action {
                        "disconnect" => "组网已断开，身份已保留",
                        "logout" => "已请求注销组网身份",
                        "install" => "Tailscale 组网组件已安装并验证",
                        _ => "正在探测组网设备",
                    },
                ),
                Err(error) if action == "connect" => {
                    service.mesh_connection_failed(&value, error.to_string())
                }
                Err(error) => service.toast(ToastLevel::Error, error.to_string()),
            }
            service.set_busy("mesh", false);
        });
    }

    fn mesh_connection_failed(&self, profile_id: &str, reason: String) {
        self.inner
            .mesh_connection_errors
            .write()
            .insert(profile_id.to_owned(), reason.clone());
        self.toast(ToastLevel::Error, reason);
    }

    async fn apply_mesh_plan(&self, plan: crate::mesh::coexist::CoexistencePlan) -> Result<bool> {
        let result = self.inner.core.apply_mesh_plan(plan).await;
        if let Err(Error::MeshCore { core, .. }) = &result {
            if let Some(report) = self
                .inner
                .core
                .last_validation()
                .filter(|r| !r.ok && r.core == *core)
            {
                *self.inner.validation.write() = Some(report);
            }
            self.check_core_release(*core, false);
        }
        result
    }

    async fn reconcile_mesh(&self) -> Result<()> {
        let states = self.inner.mesh.snapshot().networks;
        let plan = match crate::mesh::coexist::plan(&states, &[]) {
            Ok(plan) => plan,
            Err(error) => {
                // Keep the previously applied network. Pause the backend that
                // introduces a route absent from the last successful plan.
                let previous = self.inner.core.mesh_plan();
                let offender = states
                    .iter()
                    .find(|s| {
                        s.routes.iter().any(|r| {
                            !previous
                                .routes
                                .iter()
                                .any(|old| old.cidr == *r && old.interface == s.interface)
                        })
                    })
                    .or_else(|| states.last());
                if let Some(state) = offender {
                    self.inner
                        .mesh_connection_errors
                        .write()
                        .insert(state.profile_id.clone(), error.to_string());
                    self.inner.mesh_reconnect.lock().forget(&state.profile_id);
                    self.inner.mesh.disconnect(state.provider).await?;
                }
                return Err(error);
            }
        };
        if let Err(error) = self.apply_mesh_plan(plan.clone()).await {
            // Keep the proxy's verified configuration, and remove the new mesh
            // connection rather than leaving traffic captured by the proxy TUN.
            for state in states.iter().filter(|s| {
                matches!(
                    s.phase,
                    crate::mesh::MeshPhase::Starting
                        | crate::mesh::MeshPhase::NeedsLogin
                        | crate::mesh::MeshPhase::NeedsApproval
                        | crate::mesh::MeshPhase::Connected
                )
            }) {
                self.inner
                    .mesh_connection_errors
                    .write()
                    .insert(state.profile_id.clone(), error.to_string());
                self.inner.mesh_reconnect.lock().forget(&state.profile_id);
                let _ = self.inner.mesh.disconnect(state.provider).await;
            }
            return Err(error);
        }
        if let Some(compiled) = self.inner.core.last_compiled() {
            *self.inner.compiled_json.write() = compiled.to_pretty_string().ok();
            *self.inner.compiled.write() = Some(compiled);
        }
        if let Some(pac) = self.inner.pac.lock().as_ref() {
            pac.update_mesh(&plan);
        }
        Ok(())
    }

    fn start_monitor(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.spawn(async move {
            let mut tick: u64 = 0;
            let mut last_phase = crate::core::ProcessPhase::Stopped;
            let mut last_pid = None;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let Some(inner) = weak.upgrade() else { break };
                let service = AppService {
                    inner: inner.clone(),
                };
                if tick.is_multiple_of(10) {
                    service.refresh_local_binaries();
                    service.refresh_exit_gateway_status();
                    if inner.mesh_takeover_requested.load(Ordering::Acquire)
                        && service.model().work_mode == WorkMode::Mesh
                        && service
                            .model()
                            .mesh_mode
                            .target
                            .as_ref()
                            .is_some_and(|target| {
                                inner
                                    .mesh
                                    .profile(&target.profile_id)
                                    .is_ok_and(|profile| profile.auto_reconnect)
                            })
                        && !service.is_running()
                        && !service.is_busy("core")
                        && !service.is_busy("mesh")
                        && service.helper_status().running
                    {
                        service.start_core();
                    }
                }
                // Probe the privilege helper off the UI thread. The status is
                // cached inside `privilege::status`, so this is cheap unless
                // the cache expired.
                if tick.is_multiple_of(3) {
                    if let Ok(status) = tokio::task::spawn_blocking(privilege::status).await {
                        let changed = {
                            let mut guard = inner.helper.write();
                            let diff = *guard != status;
                            *guard = status;
                            diff
                        };
                        if changed {
                            service.refresh_doctor();
                        }
                    }
                }
                if tick.is_multiple_of(2) && !service.is_busy("core") {
                    if let Ok(_guard) = inner.mesh.operation.try_lock() {
                        if inner.mesh.refresh().await.is_err()
                            && inner
                                .mesh_reconnect
                                .lock()
                                .pending(&inner.mesh.snapshot())
                                .is_empty()
                        {
                            service
                                .toast(ToastLevel::Warning, "与提权助手失去联系，请检查助手状态");
                        }
                        if let Err(error) = service.reconcile_mesh().await {
                            service.toast(ToastLevel::Error, error.to_string());
                        } else {
                            service.schedule_mesh_reconnect();
                        }
                    }
                }
                let state = service.inner.core.state();
                if state.phase == crate::core::ProcessPhase::Failed
                    && last_phase != crate::core::ProcessPhase::Failed
                {
                    if let Some(exit) = &state.last_exit {
                        service.toast(ToastLevel::Error, format!("core 已退出：{}", exit.reason));
                    }
                }
                if state.phase != last_phase || state.pid != last_pid {
                    service.refresh_doctor();
                }
                last_phase = state.phase;
                last_pid = state.pid;
                if !state.phase.is_alive() {
                    service.update_connections(None, Vec::new());
                    service.flush_traffic_statistics().await;
                    tick += 1;
                    continue;
                }
                service.poll_traffic_statistics().await;
                if tick.is_multiple_of(5) {
                    if let Some((_, settings, _)) = service.inner.core.statistics_context() {
                        if state.core == Some(CoreKind::SingBox) {
                            let previous = inner.proxies.read().clone();
                            if let Ok(response) =
                                ClashClient::new(settings.api_port, "").proxies().await
                            {
                                let mut proxies = inner.proxies.write();
                                // A hot switch or reload may have completed while the poll was in flight.
                                if Arc::ptr_eq(&previous, &proxies)
                                    && CoreEpoch::from_state(&state)
                                        == CoreEpoch::from_state(&service.process_state())
                                {
                                    *proxies = Arc::new(response.proxies);
                                }
                            }
                        }
                    }
                }
                tick += 1;
            }
        });
    }

    fn start_subscription_scheduler(&self) {
        let weak = Arc::downgrade(&self.inner);
        self.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(25)).await;
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let service = AppService { inner };
                let due: Vec<String> = service
                    .inner
                    .subs
                    .read()
                    .iter()
                    .filter(|s| s.due_for_update())
                    .map(|s| s.id.clone())
                    .collect();
                for id in due {
                    service.refresh_subscription(&id);
                }
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            }
        });
    }

    // ------------------------------------------------------------------
    // Shutdown and misc
    // ------------------------------------------------------------------

    /// Complete best-effort cleanup once, including across cloned service handles.
    pub fn shutdown(&self) {
        self.inner.shutdown.call_once(|| self.shutdown_inner());
    }

    fn shutdown_inner(&self) {
        self.inner.mesh_reconnect.lock().close();
        self.inner.rt.block_on(async {
            self.poll_traffic_statistics().await;
            self.flush_traffic_statistics().await;
        });
        self.inner.mesh.shutdown();
        self.inner.rt.block_on(self.stop_exit_gateways());
        let _ = self.persist_model();
        self.close_system_proxy();
        let core = self.inner.core.clone();
        self.inner.rt.block_on(async move {
            let _ = core.stop().await;
        });
        self.inner.rt.block_on(self.flush_traffic_statistics());
        if self.runtime_uses_tun() && self.inner.helper.read().running {
            let request = HelperRequest::RemoveTun {
                interface: self.tun_interface(),
            };
            let _ = privilege::call_with_timeout(&request, std::time::Duration::from_secs(1));
        }
    }

    pub fn export_backup(&self) -> Result<String> {
        let dir = self.inner.paths.backup_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::other(format!("创建备份目录失败：{e}")))?;
        let path = dir.join(format!(
            "xrose-export-{}.json",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        self.inner.store.export_json(&path)?;
        self.toast(
            ToastLevel::Success,
            format!("备份已导出：{}", path.display()),
        );
        Ok(path.display().to_string())
    }

    pub fn backup_database(&self) -> Result<String> {
        let path = self.inner.store.backup_database()?;
        self.toast(
            ToastLevel::Success,
            format!("数据库已备份：{}", path.display()),
        );
        Ok(path.display().to_string())
    }

    // ------------------------------------------------------------------
    // Link tracer
    // ------------------------------------------------------------------

    pub fn start_trace(&self, target: &str) -> Result<()> {
        let parsed = trace::parse_target(target).map_err(Error::other)?;
        let id = self.inner.trace_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let session = TraceSession::new(id, &parsed);
        *self.inner.trace.write() = Some(session.clone());
        let service = self.clone();
        self.spawn(async move {
            trace::run(service, session).await;
        });
        Ok(())
    }

    pub fn clear_trace(&self) {
        self.inner.trace_generation.fetch_add(1, Ordering::SeqCst);
        *self.inner.trace.write() = None;
    }

    pub(crate) fn publish_trace(&self, session: &TraceSession) {
        let mut guard = self.inner.trace.write();
        let current = guard.as_ref().map(|current| current.id);
        if current == Some(session.id) {
            if std::env::var_os("XROSE_DEBUG_TRACE").is_some() {
                eprintln!(
                    "[trace] publish {:?} success={:?} summary={:?}",
                    session.phase, session.success, session.summary
                );
            }
            *guard = Some(session.clone());
            self.inner.recording.lock().observe_trace(session);
        } else if std::env::var_os("XROSE_DEBUG_TRACE").is_some() {
            eprintln!(
                "[trace] drop publish id {} current {:?}",
                session.id, current
            );
        }
    }

    pub(crate) fn is_trace_current(&self, id: u64) -> bool {
        self.inner.trace.read().as_ref().map(|s| s.id) == Some(id)
    }

    pub fn compiled_config(&self) -> Option<CompiledConfig> {
        self.inner.compiled.read().clone()
    }

    pub fn trace_snapshot(&self) -> Option<TraceSession> {
        self.inner.trace.read().clone()
    }

    pub fn inspect_recorded_trace(&self, id: u64) {
        let trace = self
            .inner
            .recording
            .lock()
            .traces
            .iter()
            .find(|trace| trace.id == id)
            .cloned();
        if let Some(mut trace) = trace {
            // A fresh id prevents a cancelled probe from publishing into replay.
            trace.id = self.inner.trace_generation.fetch_add(1, Ordering::SeqCst) + 1;
            *self.inner.trace.write() = Some(trace);
        }
    }

    pub fn open_url(&self, url: &str) {
        let url = url.to_string();
        self.spawn(async move {
            let _ = system::open_url(&url).await;
        });
    }

    pub fn probe_nat(&self) {
        if !self.inner.busy.write().insert("stun".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let core = service.core();
            let model = core
                .applied_model()
                .filter(|_| service.is_running())
                .unwrap_or_else(|| service.model());
            if model.work_mode == WorkMode::Mesh {
                if let Err(error) = service.mesh_diagnostic_config(&model, &service.settings()) {
                    service.toast(ToastLevel::Error, error.to_string());
                    service.set_busy("stun", false);
                    return;
                }
            }
            let result = system::nat::probe().await;
            *service.inner.nat_probe.write() = Some(result);
            service.set_busy("stun", false);
        });
    }

    pub fn platform_proxy_backend(&self) -> String {
        self.inner.proxy_backend.label().to_string()
    }
}

/// Resolve which saved mesh profile represents `provider`: prefer the Startup
/// selection when valid, otherwise fall back to the first saved profile.
pub fn preferred_mesh_profile_id(
    profiles: &[crate::mesh::MeshProfileView],
    settings: &Settings,
    provider: crate::mesh::MeshProvider,
) -> Option<String> {
    if let Some(startup_id) = settings.mesh_startup_profile(provider)
        && profiles
            .iter()
            .any(|p| p.id == startup_id && p.provider == provider)
    {
        return Some(startup_id.to_owned());
    }
    profiles
        .iter()
        .find(|p| p.provider == provider)
        .map(|p| p.id.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnifiedStartPlan {
    start_core: bool,
    mesh_profile_ids: Vec<String>,
    missing_startup: Vec<crate::mesh::MeshProvider>,
}

fn resolve_startup_mesh(
    settings: &Settings,
    mesh: &crate::mesh::MeshSnapshot,
) -> (Vec<String>, Vec<crate::mesh::MeshProvider>) {
    let mut ids = Vec::new();
    let mut missing = Vec::new();
    for (provider, id) in [
        (
            crate::mesh::MeshProvider::Tailscale,
            settings.startup_tailscale.as_deref(),
        ),
        (
            crate::mesh::MeshProvider::EasyTier,
            settings.startup_easytier.as_deref(),
        ),
    ] {
        let Some(id) = id else {
            continue;
        };
        if mesh
            .profiles
            .iter()
            .any(|p| p.id == id && p.provider == provider)
        {
            ids.push(id.to_owned());
        } else {
            missing.push(provider);
        }
    }
    (ids, missing)
}

fn plan_unified_start(settings: &Settings, mesh: &crate::mesh::MeshSnapshot) -> UnifiedStartPlan {
    let has_startup_mesh = mesh.supported
        && (settings.startup_tailscale.is_some() || settings.startup_easytier.is_some());
    if settings.startup_proxy || has_startup_mesh {
        let (mesh_profile_ids, missing_startup) = if mesh.supported {
            resolve_startup_mesh(settings, mesh)
        } else {
            (Vec::new(), Vec::new())
        };
        return UnifiedStartPlan {
            start_core: settings.startup_proxy,
            mesh_profile_ids,
            missing_startup,
        };
    }

    let mesh_profile_ids = if mesh.supported {
        [
            crate::mesh::MeshProvider::Tailscale,
            crate::mesh::MeshProvider::EasyTier,
        ]
        .into_iter()
        .filter_map(|provider| preferred_mesh_profile_id(&mesh.profiles, settings, provider))
        .collect()
    } else {
        Vec::new()
    };
    UnifiedStartPlan {
        start_core: true,
        mesh_profile_ids,
        missing_startup: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatencyScope {
    Nodes(Vec<String>),
    All,
    Node(String),
    Group(String),
}

/// Only split lists of links. Preserve structured documents and wrapped Base64 verbatim.
fn split_shared_content(content: &str) -> (Vec<&str>, std::borrow::Cow<'_, str>) {
    let is_subscription = |line: &str| {
        !line.contains(char::is_whitespace)
            && !crate::subscription::links::is_http_proxy_link(line)
            && (line
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
                || line
                    .get(..8)
                    .is_some_and(|s| s.eq_ignore_ascii_case("https://")))
            && url::Url::parse(line).is_ok_and(|url| url.host().is_some())
    };
    let urls: Vec<_> = content
        .lines()
        .map(str::trim)
        .filter(|line| is_subscription(line))
        .collect();
    if urls.is_empty()
        || content.trim_start().starts_with(['{', '['])
        || content.contains("proxies:")
    {
        return (Vec::new(), content.into());
    }
    let rest = content
        .lines()
        .filter(|line| !is_subscription(line.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    (urls, rest.into())
}

fn subscription_fields(name: &str, url: &str) -> Result<(String, String)> {
    let url = url.trim();
    if url.is_empty() {
        return Err(Error::other("订阅地址不能为空"));
    }
    if !url::Url::parse(url)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
    {
        return Err(Error::other("请输入有效的 HTTP 或 HTTPS 订阅地址"));
    }
    Ok((
        if name.trim().is_empty() {
            pretty_host(url)
        } else {
            name.trim().into()
        },
        url.into(),
    ))
}

fn pretty_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "订阅".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn direct_import_reports_failed_storage_and_leaves_the_node_retryable() {
        let (service, _dir) = service();
        let db = rusqlite::Connection::open(service.paths().database()).unwrap();
        db.execute_batch("CREATE TRIGGER reject_import BEFORE UPDATE ON profiles BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
        let link = "socks://retry.example:1080#Retry";
        let report = service.import_shared_content(link);
        assert_eq!(report.nodes, 0);
        assert!(!report.errors.is_empty());
        assert!(service.model().nodes.is_empty());
        db.execute_batch("DROP TRIGGER reject_import;").unwrap();
        assert_eq!(service.import_shared_content(link).nodes, 1);
    }

    #[test]
    fn selected_share_links_round_trip_in_requested_order_including_http_proxies() {
        let (service, _dir) = service();
        service.import_shared_content("http://proxy.example:8080?xrose=proxy#HTTP\nsocks://proxy.example:1080#SOCKS\nsocks://extra.example:1080#Extra");
        let model = service.model();
        let links = service.export_links(&[model.nodes[1].id.clone(), model.nodes[0].id.clone()]);
        assert_eq!(links.lines().count(), 2);
        let (urls, nodes) = super::split_shared_content(&links);
        assert!(urls.is_empty());
        let parsed = crate::subscription::parse_subscription(&nodes);
        assert_eq!(
            parsed
                .nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            ["SOCKS", "HTTP"]
        );
        assert!(matches!(
            super::AppService::preview_shared_content(links.lines().last().unwrap()),
            super::SharePreview::Node {
                name,
                protocol: "HTTP",
                endpoint,
                count: 1,
            } if name == "HTTP" && endpoint == "proxy.example:8080"
        ));
    }

    #[test]
    fn direct_import_separates_subscription_urls_without_splitting_documents() {
        let node = "socks://node.example:1080#Demo";
        let url = "https://feed.example/test";
        let mixed = format!("{node}\r\n{url}");
        let (subs, rest) = super::split_shared_content(&mixed);
        assert_eq!(subs, [url]);
        assert_eq!(rest, node);
        let yaml = "proxies:\n  # https://feed.example/test";
        assert_eq!(super::split_shared_content(yaml).1, yaml);
        assert!(super::split_shared_content("https:// bad").0.is_empty());
    }

    #[test]
    fn mixed_paste_keeps_existing_subscription_and_deduplicates_nodes() {
        let (service, _dir) = service();
        let url = "https://feed.example/test";
        service
            .inner
            .subs
            .write()
            .push(crate::subscription::Subscription::new("Fixture", url));
        let content = format!("{url}\nsocks://node.example:1080#Fixture");
        service.import_shared_content(&content);
        service.import_shared_content(&content);
        let snapshot = service.snapshot();
        assert_eq!(snapshot.subscriptions.len(), 1);
        assert_eq!(snapshot.model.nodes.len(), 1);
        assert_eq!(snapshot.model.nodes[0].name, "Fixture");
    }

    #[test]
    fn failed_start_preflight_clears_busy_and_is_retryable_without_downloading() {
        let (service, _dir) = service();
        service.inner.model.write().work_mode = WorkMode::Proxy;
        for _ in 0..2 {
            service.start_core();
            service.inner.rt.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while service.is_busy("core") {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            });
            assert!(!service.is_running());
            assert!(!service.is_busy("install"));
        }
        assert!(service.inner.core.binaries().list().is_empty());
        assert!(
            service
                .active_toasts()
                .iter()
                .any(|t| t.message.contains("没有可编译"))
        );
    }

    #[test]
    fn rule_reorder_preserves_presets_and_noops_do_not_write() {
        let (service, dir) = service();
        let initial = service.model().routing;
        assert!(initial.rules.len() >= 3);
        let first = initial.rules[0].id.clone();
        let last = initial.rules.last().unwrap().id.clone();
        service.set_rule_enabled(&first, false);
        let disabled = service.model().routing.rules[0].clone();
        assert!(
            service
                .move_rule_to(&first, None, &last, Placement::After)
                .unwrap()
        );
        let after = service.model().routing;
        assert_eq!(after.rules.last(), Some(&disabled));
        assert_eq!(after.preset, initial.preset);
        assert_eq!(&after.rules[..after.rules.len() - 1], &initial.rules[1..]);
        let profile = service.snapshot().active_profile.unwrap();
        assert_eq!(
            service
                .inner
                .store
                .load_profile(&profile)
                .unwrap()
                .unwrap()
                .model
                .routing,
            after
        );

        let db = rusqlite::Connection::open(AppPaths::from_root(dir.path()).database()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_profile_update BEFORE UPDATE ON profiles BEGIN SELECT RAISE(FAIL, 'test storage failure'); END;").unwrap();
        assert!(
            !service
                .move_rule_to(&first, None, &last, Placement::After)
                .unwrap()
        );
        assert!(
            !service
                .move_rule_to(&first, None, &first, Placement::Before)
                .unwrap()
        );
        assert!(
            service
                .move_rule_to(&first, None, &last, Placement::Before)
                .is_err()
        );
        assert!(
            service
                .move_rule_to(&first, Some("missing-group"), &last, Placement::Before)
                .is_err()
        );
        assert_eq!(service.model().routing, after);
    }

    #[test]
    fn reorder_is_durable_and_storage_failure_keeps_snapshot() {
        let (service, dir) = service();
        let first = service.save_rule_group(None, "first").unwrap();
        let middle = service.save_rule_group(None, "middle").unwrap();
        let last = service.save_rule_group(None, "last").unwrap();
        assert!(
            service
                .move_rule_group_to(&first, &last, Placement::After)
                .unwrap()
        );
        assert_eq!(
            service
                .model()
                .routing
                .rule_groups
                .iter()
                .map(|g| g.id.clone())
                .collect::<Vec<_>>(),
            [middle.clone(), last.clone(), first.clone()]
        );
        let stored = service
            .inner
            .store
            .load_profile(service.snapshot().active_profile.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.model.routing, service.model().routing);
        assert!(service.snapshot().config_dirty);
        let db = rusqlite::Connection::open(AppPaths::from_root(dir.path()).database()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_profile_update BEFORE UPDATE ON profiles BEGIN SELECT RAISE(FAIL, 'test storage failure'); END;").unwrap();
        let before = service.model();
        assert!(
            service
                .move_rule_group_to(&first, &middle, Placement::Before)
                .is_err()
        );
        assert_eq!(service.model(), before);
    }

    #[test]
    fn dns_deletion_checks_references_without_mutating_on_error() {
        let (service, _dir) = service();
        service.set_dns_preset(crate::dns::DnsPreset::SplitFakeIp);
        let before = service.model();
        assert!(service.delete_dns_server("local").is_err());
        assert!(service.delete_dns_server("fakeip").is_err());
        assert!(service.delete_dns_server("missing").is_err());
        assert_eq!(service.model(), before);
        service.delete_dns_server("remote").unwrap();
        assert!(
            !service
                .model()
                .dns
                .servers
                .iter()
                .any(|s| s.tag == "remote")
        );
    }

    #[test]
    fn rule_groups_persist_coexist_with_presets_and_control_preview() {
        use crate::model::{Matchers, RouteAction};
        let (service, _dir) = service();
        let first = service.save_rule_group(None, "first").unwrap();
        let second = service.save_rule_group(None, "second").unwrap();
        let matcher = Matchers {
            domains: vec!["example.com".into()],
            ..Default::default()
        };
        let direct = RouteRule::new("direct", matcher.clone(), RouteAction::direct());
        service
            .upsert_rule_in_group(direct.clone(), Some(&first))
            .unwrap();
        service
            .upsert_rule_in_group(
                RouteRule::new("block", matcher, RouteAction::block()),
                Some(&second),
            )
            .unwrap();
        service.set_route_preset(RoutePreset::Global);
        let preview = || {
            crate::route::evaluate(
                &service.model(),
                &crate::route::ConnectionInfo {
                    domain: Some("example.com".into()),
                    ..Default::default()
                },
            )
            .action
        };
        assert_eq!(preview(), RouteAction::direct());
        service.move_rule_group(&second, -1);
        assert_eq!(preview(), RouteAction::block());
        service.set_rule_group_enabled(&second, false);
        assert_eq!(preview(), RouteAction::direct());
        service.save_rule_group(Some(&first), "renamed").unwrap();
        service.assign_rule_group(&direct.id, None);
        assert_eq!(service.model().routing.rules.last().unwrap().id, direct.id);
        service.delete_rule_group(&second);
        assert!(!service.model().routing.rules.last().unwrap().enabled);
        let stored = service
            .inner
            .store
            .load_profile(service.inner.active_profile.read().as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.model.routing, service.model().routing);
        assert_eq!(stored.model.routing.rule_groups[0].name, "renamed");
    }

    #[test]
    fn rule_outbound_group_persists_and_routes_through_both_cores() {
        use crate::model::{GroupKind, Matchers, OutboundGroup, Protocol, RouteAction};
        let (service, _dir) = service();
        let node = Node::new("Fixture exit", Protocol::Socks, "192.0.2.10", 1080);
        let mut group = OutboundGroup::new("Fixture outbound", GroupKind::Selector);
        group.members.push(node.id.clone());
        group.selected = Some(node.id.clone());
        service
            .commit_model_change(|model| {
                model.add_nodes([node.clone()]);
                model.groups.push(group.clone());
                model.work_mode = WorkMode::Proxy;
                model.apply_preset(RoutePreset::Global);
                Ok(true)
            })
            .unwrap();
        let rule_group = service.save_rule_group(None, "Fixture rules").unwrap();
        let rule = RouteRule::new(
            "Fixture route",
            Matchers {
                domains: vec!["route.example".into()],
                ..Default::default()
            },
            RouteAction::proxy_via(&group.id),
        );
        service
            .upsert_rule_in_group(rule.clone(), Some(&rule_group))
            .unwrap();
        let stored = service
            .inner
            .store
            .load_profile(service.inner.active_profile.read().as_ref().unwrap())
            .unwrap()
            .unwrap()
            .model;
        assert_eq!(
            stored
                .routing
                .rules_for_mode(WorkMode::Proxy)
                .find(|r| r.id == rule.id)
                .unwrap()
                .action,
            rule.action
        );
        assert_ne!(
            stored.default_group_id().as_deref(),
            Some(group.id.as_str())
        );
        let settings = service.settings();
        let ctx = crate::adapters::CompileContext::new(
            &settings,
            service.paths().data_dir.clone(),
            service.paths().runtime_dir.clone(),
        );
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let compiled = crate::adapters::adapter_for(core)
                .compile(&stored, &settings, &ctx)
                .unwrap();
            let (rules, domain, outbound_key, expected) = match core {
                CoreKind::SingBox => (
                    &compiled.json["route"]["rules"],
                    "route.example",
                    "outbound",
                    group.tag(),
                ),
                CoreKind::Xray => (
                    &compiled.json["routing"]["rules"],
                    "full:route.example",
                    "outboundTag",
                    node.outbound_tag(),
                ),
            };
            let compiled_rule = rules
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["domain"] == serde_json::json!([domain]))
                .unwrap();
            assert_eq!(compiled_rule[outbound_key], expected);
        }
    }

    #[test]
    fn rule_outbound_validation_and_storage_failure_preserve_saved_rules() {
        use crate::model::{Matchers, RouteAction};
        let (service, dir) = service();
        let mut rule = RouteRule::new(
            "Fixture route",
            Matchers {
                domains: vec!["route.example".into()],
                ..Default::default()
            },
            RouteAction::proxy(),
        );
        service.upsert_rule_in_group(rule.clone(), None).unwrap();
        assert!(
            service
                .model()
                .routing
                .rules_for_mode(WorkMode::Proxy)
                .find(|r| r.id == rule.id)
                .unwrap()
                .action
                .group
                .is_none()
        );
        let saved = service.model().routing;
        rule.action = RouteAction::proxy_via("missing-group");
        assert!(service.upsert_rule_in_group(rule.clone(), None).is_err());
        let group_id = service.model().default_group_id().unwrap();
        service
            .inner
            .model
            .write()
            .group_mut(&group_id)
            .unwrap()
            .enabled = false;
        rule.action = RouteAction::proxy_via(&group_id);
        assert!(service.upsert_rule_in_group(rule.clone(), None).is_err());
        assert_eq!(service.model().routing, saved);
        rule.action = RouteAction::direct();
        assert!(
            service
                .upsert_rule_in_group(rule.clone(), Some("missing-rule-group"))
                .is_err()
        );
        let db = rusqlite::Connection::open(AppPaths::from_root(dir.path()).database()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_profile_update BEFORE UPDATE ON profiles BEGIN SELECT RAISE(FAIL, 'test storage failure'); END;").unwrap();
        assert!(service.upsert_rule_in_group(rule.clone(), None).is_err());
        assert_eq!(service.model().routing, saved);
        db.execute_batch("DROP TRIGGER fail_profile_update")
            .unwrap();
        rule.action.group = Some(group_id);
        service.upsert_rule_in_group(rule.clone(), None).unwrap();
        let model = service.model();
        assert_eq!(
            model
                .routing
                .rules_for_mode(WorkMode::Proxy)
                .find(|r| r.id == rule.id)
                .unwrap()
                .action,
            RouteAction::direct()
        );
    }

    #[test]
    fn notifications_queue_once_and_honor_preference_without_a_desktop() {
        let (service, _dir) = service();
        assert!(!service.settings().system_notifications);
        let mut settings = service.settings();
        settings.system_notifications = true;
        service.update_settings(settings);
        let receiver = service.subscribe_notifications();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            NotificationEvent::Configure {
                enabled: true,
                user_initiated: false
            }
        ));
        service.toast(ToastLevel::Success, "fixture");
        assert!(
            matches!(receiver.try_recv().unwrap(), NotificationEvent::Show(toast)
            if toast.message == "fixture")
        );
        let _ = service.snapshot();
        let _ = service.snapshot();
        assert!(receiver.try_recv().is_err());
        assert!(
            service
                .active_toasts()
                .iter()
                .all(|t| !t.visible_in_app(true))
        );
        let mut settings = service.settings();
        settings.system_notifications = false;
        service.update_settings(settings.clone());
        assert!(matches!(
            receiver.try_recv().unwrap(),
            NotificationEvent::Configure {
                enabled: false,
                user_initiated: true
            }
        ));
        service.toast(ToastLevel::Info, "disabled");
        assert!(receiver.try_recv().is_err());
        assert!(
            service
                .active_toasts()
                .iter()
                .any(|t| t.message == "disabled" && t.visible_in_app(false))
        );
        assert!(
            service
                .active_toasts()
                .iter()
                .filter(|t| t.message == "fixture")
                .all(|t| !t.visible_in_app(false))
        );
        settings.system_notifications = true;
        service.update_settings(settings.clone());
        // Enabling must request authorization even when no toast is produced.
        assert!(matches!(
            receiver.try_recv().unwrap(),
            NotificationEvent::Configure {
                enabled: true,
                user_initiated: true
            }
        ));
        service.update_settings(settings);
        assert!(receiver.try_recv().is_err());
        service.report_notification_error("fixture permission failure");
        assert!(receiver.try_recv().is_err());
        assert!(
            service
                .active_toasts()
                .iter()
                .any(|t| t.message == "fixture permission failure"
                    && !t.visible_in_app(true)
                    && !t.visible_in_app(false))
        );
    }

    #[test]
    fn mesh_startup_selection_saves_with_forms_and_keeps_providers_independent() {
        use crate::mesh::{MeshProfile, MeshProvider, MeshSecret};
        let (service, _dir) = service();
        let first = MeshProfile {
            provider: MeshProvider::Tailscale,
            name: "first".into(),
            ..Default::default()
        };
        let second = MeshProfile {
            provider: MeshProvider::Tailscale,
            name: "second".into(),
            ..Default::default()
        };
        let et = MeshProfile {
            network_name: "fixture".into(),
            network_secret: MeshSecret("fake-key".into()),
            peers: vec!["tcp://192.0.2.1:11010".into()],
            ..Default::default()
        };
        service
            .save_mesh_profile_with_startup(first.clone(), true)
            .unwrap();
        service
            .save_mesh_profile_with_startup(second.clone(), false)
            .unwrap();
        assert_eq!(
            service.settings().startup_tailscale.as_deref(),
            Some(first.id.as_str())
        );
        service
            .save_mesh_profile_with_startup(et.clone(), true)
            .unwrap();
        service
            .save_mesh_profile_with_startup(second.clone(), true)
            .unwrap();
        service
            .save_mesh_profile_with_startup(first.clone(), false)
            .unwrap();
        let saved = Settings::load(&service.paths().settings_file()).unwrap();
        assert_eq!(saved.startup_tailscale.as_deref(), Some(second.id.as_str()));
        assert_eq!(saved.startup_easytier.as_deref(), Some(et.id.as_str()));
        assert!(!saved.login_enabled());
        assert!(!service.is_busy("autostart"));
        service.delete_mesh_profile(&first.id).unwrap();
        assert_eq!(
            service.settings().startup_tailscale.as_deref(),
            Some(second.id.as_str())
        );
        service
            .save_mesh_profile_with_startup(second.clone(), false)
            .unwrap();
        assert!(service.settings().startup_tailscale.is_none());
        assert_eq!(
            service.settings().startup_easytier.as_deref(),
            Some(et.id.as_str())
        );
        service.delete_mesh_profile(&et.id).unwrap();
        assert!(service.settings().startup_easytier.is_none());
    }

    #[test]
    fn service_forward_edits_preserve_profile_and_reject_stale_updates() {
        use crate::mesh::{ForwardProtocol, MeshProfile, MeshProvider, ServiceForward};
        let (service, _dir) = service();
        let profile = MeshProfile {
            provider: MeshProvider::Tailscale,
            ..Default::default()
        };
        service.inner.mesh.save(profile.clone()).unwrap();
        let rule = ServiceForward {
            protocol: ForwardProtocol::Tcp,
            port: 18080,
            target: "127.0.0.1:8080".parse().unwrap(),
        };
        service
            .edit_mesh_service(&profile.id, None, Some(rule.clone()))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.is_busy("mesh") {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let saved = service.inner.mesh.profile(&profile.id).unwrap();
        assert_eq!(saved.service_forwards, vec![rule.clone()]);
        assert_eq!(saved.hostname, profile.hostname);
        service
            .edit_mesh_service(&profile.id, Some(&rule), None)
            .unwrap();
        while service.is_busy("mesh") {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            service
                .edit_mesh_service(&profile.id, Some(&rule), None)
                .is_err()
        );
    }

    #[test]
    fn startup_actions_run_once_on_manual_launch_and_preserve_proxy_mode() {
        let (service, _dir) = service();
        let mut settings = service.settings();
        settings.check_app_updates = false;
        settings.hot_reload = false;
        settings.startup_proxy = true;
        settings.startup_tailscale = Some("missing-fixture".into());
        settings.tun_enabled = true;
        settings.system_proxy = false;
        service.update_settings(settings);
        service.inner.model.write().work_mode = WorkMode::Proxy;
        // An empty proxy profile fails before binary download or helper access.
        service.run_startup_actions();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let toasts = service.active_toasts();
            if !service.is_busy("core")
                && toasts
                    .iter()
                    .any(|t| t.message.contains("应用启动连接配置不存在"))
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!service.settings().system_proxy);
        assert!(service.settings().tun_enabled);
        let before = service.inner.toast_seq.load(Ordering::SeqCst);
        service.run_startup_actions();
        assert_eq!(service.inner.toast_seq.load(Ordering::SeqCst), before);
    }

    #[test]
    fn notification_controls_survive_pending_delivery_and_disabled_startup() {
        let (service, _dir) = service();
        let mut settings = service.settings();
        settings.system_notifications = false;
        service.update_settings(settings.clone());
        let receiver = service.subscribe_notifications();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            NotificationEvent::Configure {
                enabled: false,
                user_initiated: false
            }
        ));
        settings.system_notifications = true;
        service.update_settings(settings.clone());
        for index in 0..40 {
            service.toast(ToastLevel::Info, format!("fixture {index}"));
        }
        settings.system_notifications = false;
        service.update_settings(settings);
        let events: Vec<_> = receiver.try_iter().collect();
        assert_eq!(events.len(), 42);
        assert!(matches!(
            events.last().unwrap(),
            NotificationEvent::Configure {
                enabled: false,
                user_initiated: true
            }
        ));
    }

    #[test]
    fn vpn_import_edit_and_storage_preserve_certificate_options() {
        let (service, _dir) = service();
        let content = r#"{"endpoints":[{"type":"openconnect","tag":"office","server":"https://vpn.example:8443/team","flavor":"gp","username":"fixture","password":"fake-vpn-password","auth_group":"team","cookie":"fake-vpn-session"}]}"#;
        assert_eq!(service.import_content(content), 1);
        let original = service
            .model()
            .nodes
            .iter()
            .find(|n| n.protocol == crate::model::Protocol::Openconnect)
            .unwrap()
            .clone();
        let mut draft = crate::model::NodeDraft::from(&original);
        draft.name = "office renamed".into();
        service.update_node(&original.id, draft).unwrap();
        let profile = service
            .inner
            .store
            .load_profile(service.inner.active_profile.read().as_ref().unwrap())
            .unwrap()
            .unwrap();
        let saved = profile.model.node(&original.id).unwrap();
        assert_eq!(saved.name, "office renamed");
        assert_eq!(saved.credentials, original.credentials);
        let link = service.export_links(&[original.id.clone()]);
        let restored = crate::subscription::parse_share_link(&link).unwrap();
        assert_eq!(restored.credentials, saved.credentials);
        assert_eq!(restored.port, 8443);
        assert_eq!(restored.credentials.openconnect.unwrap().path, "/team");
    }

    #[test]
    fn custom_binary_failure_does_not_fall_back_to_network() {
        let (service, dir) = service();
        let mut settings = service.settings();
        settings.binary_paths.insert(
            settings.core.as_str().into(),
            dir.path().join("missing").display().to_string(),
        );
        let result = service
            .inner
            .rt
            .block_on(service.ensure_core_binary(&mut settings));
        assert!(result.unwrap_err().to_string().contains("自定义"));
        assert!(!service.is_busy("install"));
    }

    #[test]
    fn editing_node_preserves_references_metadata_and_persists_parameters() {
        let (service, dir) = service();
        let id = service
            .add_manual_node(NodeDraft {
                server: "before.example.com".into(),
                protocol: Protocol::Http,
                ..Default::default()
            })
            .unwrap();
        {
            let mut model = service.inner.model.write();
            let node = model.node_mut(&id).unwrap();
            node.source = crate::model::NodeSource::Subscription;
            node.subscription_id = Some("fixture-sub".into());
            node.sort = 7;
            node.enabled = false;
        }
        let before = service.model();
        let mut draft = NodeDraft::from(before.node(&id).unwrap());
        draft.name = "edited".into();
        draft.server = "after.example.com".into();
        draft.port = 8443;
        service.inner.latency.write().insert(
            id.clone(),
            crate::storage::LatencyEntry {
                key: id.clone(),
                delay_ms: Some(10),
                checked_at: now_unix(),
                detail: Some("fixture".into()),
            },
        );
        service
            .inner
            .store
            .set_latency(&service.snapshot().latency[&id])
            .unwrap();
        service.update_node(&id, draft).unwrap();
        let after = service.model();
        assert_eq!(after.nodes.len(), before.nodes.len());
        assert_eq!(after.groups, before.groups);
        let edited = after.node(&id).unwrap();
        assert_eq!(
            (edited.name.as_str(), edited.server.as_str(), edited.port),
            ("edited", "after.example.com", 8443)
        );
        assert_eq!(edited.source, crate::model::NodeSource::Subscription);
        assert_eq!(edited.subscription_id.as_deref(), Some("fixture-sub"));
        assert_eq!(edited.sort, 7);
        assert!(!edited.enabled);
        assert!(!service.snapshot().latency.contains_key(&id));
        assert!(
            service
                .inner
                .store
                .latency_cache()
                .unwrap()
                .iter()
                .all(|entry| entry.key != id)
        );
        let saved = Store::open(&AppPaths::from_root(dir.path()))
            .unwrap()
            .load_profile(&service.snapshot().active_profile.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(saved.model.node(&id).unwrap(), edited);
    }

    #[test]
    fn editing_node_rejects_duplicates_invalid_and_missing_nodes_without_changes() {
        let (service, _dir) = service();
        let first = service
            .add_manual_node(NodeDraft {
                protocol: Protocol::Http,
                server: "first.example.com".into(),
                ..Default::default()
            })
            .unwrap();
        let second = service
            .add_manual_node(NodeDraft {
                protocol: Protocol::Http,
                server: "second.example.com".into(),
                ..Default::default()
            })
            .unwrap();
        let before = service.model();
        let duplicate = NodeDraft::from(before.node(&first).unwrap());
        assert!(service.update_node(&second, duplicate.clone()).is_err());
        assert!(service.update_node("missing", duplicate.clone()).is_err());
        let mut invalid = duplicate;
        invalid.port = 0;
        assert!(service.update_node(&first, invalid).is_err());
        assert_eq!(service.model(), before);
        service
            .update_node(&first, NodeDraft::from(before.node(&first).unwrap()))
            .unwrap();
    }

    #[test]
    fn rule_set_source_applies_to_new_tags_and_retains_custom_addresses() {
        use crate::model::RuleSetSource::*;
        let (service, _dir) = service();
        let url = |tag: &str| {
            crate::adapters::resolve_rule_set(&service.model(), tag)
                .unwrap()
                .url
        };
        let upstream = url("geoip-cn");
        service.set_rule_set_source(MetaCubeX, None).unwrap();
        assert!(url("geoip-cn").ends_with("/sing/geo/geoip/cn.srs"));
        service.set_rule_set_source(JsDelivr, None).unwrap();
        assert!(url("geosite-gfw").contains("cdn.jsdelivr.net/gh/MetaCubeX/"));
        assert!(url("geoip-cn").contains("sing-geoip@rule-set/"));
        let exported = crate::route::transfer::export(
            &service.model(),
            &crate::route::transfer::ExportScope::RuleSets,
        )
        .unwrap();
        assert!(exported.contains(&url("geoip-cn")));
        service
            .set_rule_set_source(Custom, Some("https://rules.example/base/"))
            .unwrap();
        assert_eq!(
            url("geosite-new"),
            "https://rules.example/base/geosite-new.srs"
        );
        let template = "https://rules.example/{kind}/{name}.srs";
        service.set_rule_set_source(Custom, Some(template)).unwrap();
        assert_eq!(url("geosite-new"), "https://rules.example/geosite/new.srs");
        for invalid in [
            "",
            "file:///rules",
            "https://rules.example/{bad}.srs",
            "https://{tag}.example/",
            "https://rules.example/a?q=b",
            "https://rules.example/#{tag}",
        ] {
            assert!(service.set_rule_set_source(Custom, Some(invalid)).is_err());
            assert_eq!(service.model().routing.custom_rule_set_template, template);
        }
        service.set_rule_set_source(Upstream, None).unwrap();
        assert_eq!(url("geoip-cn"), upstream);
        let store = service.store();
        let stored = store
            .load_profile(&store.active_profile_id().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.model.routing.custom_rule_set_template, template);
        let mut explicit = crate::adapters::default_rule_set("geosite-gfw").unwrap();
        explicit.url = "https://other.example/explicit.srs".into();
        service
            .inner
            .model
            .write()
            .routing
            .rule_sets
            .push(explicit.clone());
        service.set_rule_set_source(Custom, None).unwrap();
        assert_eq!(url("geosite-gfw"), explicit.url);
        assert!(!service.snapshot().config_dirty);
        service.set_busy("rulesets", true);
        assert!(service.set_rule_set_source(Upstream, None).is_err());
        assert_eq!(service.model().routing.rule_set_source, Custom);
    }

    #[test]
    fn subscription_sharing_uses_the_current_feed_url_even_without_nodes() {
        let (service, _dir) = service();
        let initial = "https://feed.example/first";
        let updated = "https://feed.example/list?format=full&name=A%2FB";
        let mut sub = Subscription::new("First", initial);
        sub.enabled = false;
        sub.headers.insert("X-Example".into(), "not-shared".into());
        let id = sub.id.clone();
        let target = ShareTarget::Subscription(id.clone());
        service.update_subscription_settings(sub).unwrap();
        assert_eq!(
            service.snapshot().share_link(&target),
            Some(("First", initial.into()))
        );
        service.edit_subscription(&id, "Edited", updated).unwrap();
        let snapshot = service.snapshot();
        let (name, link) = snapshot.share_link(&target).unwrap();
        assert_eq!(name, "Edited");
        assert_eq!(link, updated);
        assert!(matches!(
            AppService::preview_shared_content(&link),
            SharePreview::Subscription { .. }
        ));
        service.delete_subscription(&id).unwrap();
        assert!(service.snapshot().share_link(&target).is_none());
    }

    #[test]
    fn graphical_node_qr_encodes_the_share_link() {
        let (service, _dir) = service();
        let id = service
            .add_manual_node(NodeDraft {
                protocol: Protocol::Http,
                server: "qr.example.com".into(),
                ..Default::default()
            })
            .unwrap();
        let matrix = service.node_qr_matrix(&id).unwrap();
        let code = qrcode::QrCode::new(service.export_links(&[id]).as_bytes()).unwrap();
        assert_eq!(matrix.len(), code.width());
        for (y, row) in matrix.iter().enumerate() {
            assert_eq!(row.len(), code.width());
            for (x, dark) in row.iter().enumerate() {
                assert_eq!(*dark, code[(x, y)] == qrcode::Color::Dark);
            }
        }
        assert!(service.node_qr_matrix("missing").is_err());
    }

    #[test]
    fn privacy_preferences_preserve_pending_changes_without_rewriting_the_profile() {
        let (service, _dir) = service();
        let profile = service.model();
        let generation = service.inner.doctor_generation.load(Ordering::SeqCst);
        service.inner.reload.lock().dirty = true;
        let mut settings = service.settings();
        settings.privacy_mode = !settings.privacy_mode;
        service.update_settings(settings.clone());
        assert_eq!(service.settings(), settings);
        assert_eq!(
            Settings::load(&service.paths().settings_file()).unwrap(),
            settings
        );
        assert_eq!(service.model(), profile);
        assert_eq!(
            service.inner.doctor_generation.load(Ordering::SeqCst),
            generation
        );
        assert!(service.snapshot().config_dirty);
    }

    #[test]
    fn latency_batch_tests_every_enabled_requested_node_without_switching_selection() {
        let (service, _dir) = service();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let first = Node::new("first", crate::model::Protocol::Http, "127.0.0.1", port);
        let second_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let second = Node::new(
            "second",
            crate::model::Protocol::Http,
            "127.0.0.1",
            second_listener.local_addr().unwrap().port(),
        );
        let mut disabled = Node::new("disabled", crate::model::Protocol::Http, "127.0.0.1", 1);
        disabled.enabled = false;
        let ids = vec![first.id.clone(), second.id.clone(), disabled.id.clone()];
        service
            .inner
            .model
            .write()
            .add_nodes(vec![first, second, disabled]);
        let before = service.model();
        service.test_latency(LatencyScope::Nodes(ids.clone()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.is_busy("latency") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!service.is_busy("latency"));
        let snapshot = service.snapshot();
        assert!(snapshot.latency[&ids[0]].delay_ms.is_some());
        assert!(snapshot.latency[&ids[1]].delay_ms.is_some());
        assert!(!snapshot.latency.contains_key(&ids[2]));
        assert_eq!(service.model(), before);
    }

    use super::*;
    use crate::model::Protocol;

    #[test]
    fn mesh_reconciliation_preserves_pending_proxy_edits_and_profile() {
        let (service, _dir) = service();
        service.import_content("vless://11111111-2222-3333-4444-555555555555@pending.example.com:443?security=tls#pending");
        // Represent edits made while a real core was running, without starting one.
        service.inner.reload.lock().dirty = true;
        let before = serde_json::to_value(service.model()).unwrap();
        assert!(service.snapshot().config_dirty);
        let active = service.snapshot().active_profile;
        service.inner.rt.block_on(async {
            service
                .inner
                .core
                .apply_mesh_plan(crate::mesh::coexist::CoexistencePlan {
                    active: true,
                    ..Default::default()
                })
                .await
                .unwrap();
            service.reconcile_mesh().await.unwrap();
        });
        assert_eq!(serde_json::to_value(service.model()).unwrap(), before);
        assert_eq!(service.snapshot().active_profile, active);
        assert!(service.snapshot().config_dirty);
    }

    fn service() -> (AppService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let service = AppService::with_paths(paths).unwrap();
        (service, dir)
    }

    #[test]
    fn disconnected_mesh_diagnostics_never_fall_back_to_a_host_connection() {
        for provider in [
            crate::mesh::MeshProvider::EasyTier,
            crate::mesh::MeshProvider::Tailscale,
        ] {
            let (service, _dir) = service();
            {
                let mut model = service.inner.model.write();
                model.work_mode = WorkMode::Mesh;
                model.mesh_mode.target = Some(crate::model::MeshExitTarget {
                    profile_id: "fixture".into(),
                    peer_id: "fixture-exit".into(),
                    provider,
                    name: "Fixture exit".into(),
                    address: "192.0.2.10".into(),
                    gateway_id: "mesh-exit-fixture".into(),
                });
            }
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            service
                .start_trace(&format!("tcp://{}", listener.local_addr().unwrap()))
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let session = service.trace_snapshot().unwrap();
                if session.phase == trace::TracePhase::Failed {
                    assert!(session.error.as_deref().unwrap().contains("组网出口"));
                    assert!(session.group_name.is_none());
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "mesh trace did not reject disconnected mode"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn diagnostic_rules_persist_selected_outbound_and_reject_disabled_or_removed_groups() {
        let (service, dir) = service();
        let group =
            crate::model::OutboundGroup::new("Fixture outbound", crate::model::GroupKind::Selector);
        let id = group.id.clone();
        service.inner.model.write().groups.push(group);
        let target = DiagnosticTarget {
            domain: "fixture.example".into(),
            ..Default::default()
        };
        let action = crate::model::RouteAction::proxy_via(&id);
        service
            .add_diagnostic_rule(&target, DiagnosticRuleKind::Domain, action.clone())
            .unwrap();
        let saved = Store::open(&AppPaths::from_root(dir.path()))
            .unwrap()
            .load_profile(&service.snapshot().active_profile.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(saved.model.routing.rules[0].action, action);
        let count = service.model().routing.rules.len();
        service
            .inner
            .model
            .write()
            .groups
            .iter_mut()
            .find(|g| g.id == id)
            .unwrap()
            .enabled = false;
        assert!(
            service
                .add_diagnostic_rule(&target, DiagnosticRuleKind::Domain, action.clone())
                .is_err()
        );
        service
            .inner
            .model
            .write()
            .groups
            .retain(|group| group.id != id);
        assert!(
            service
                .add_diagnostic_rule(&target, DiagnosticRuleKind::Domain, action)
                .is_err()
        );
        assert_eq!(service.model().routing.rules.len(), count);
    }

    #[test]
    fn log_sharing_preserves_metadata_and_keeps_raw_and_redacted_exports_separate() {
        let (service, _dir) = service();
        service.logs().clear();
        service.inner.model.write().nodes.push(Node::new(
            "Fixture",
            crate::model::Protocol::Vless,
            "edge.example",
            443,
        ));
        service
            .logs()
            .push(LogStream::Stdout, LogLevel::Warn, "dial edge.example:443");
        service
            .logs()
            .push(LogStream::Stderr, LogLevel::Error, "retry token=fixture");
        let mut entries = service.logs().recent(2, LogLevel::Trace, "");
        entries.reverse();
        let raw = service
            .logs_for_clipboard(&entries[..1], LogPrivacy::Original)
            .unwrap();
        let shared = service
            .logs_for_clipboard(&entries[..1], LogPrivacy::Redacted)
            .unwrap();
        assert_eq!(
            raw,
            format!("{} ERROR stderr retry token=fixture", entries[0].timestamp)
        );
        assert_eq!(
            shared,
            format!("{} ERROR stderr retry token=***", entries[0].timestamp)
        );
        let public_file = service
            .export_logs_with_privacy(LogPrivacy::Redacted)
            .unwrap();
        let original_file = service.export_logs().unwrap();
        assert_ne!(public_file, original_file);
        let shared = std::fs::read_to_string(public_file).unwrap();
        assert_eq!(
            shared,
            format!(
                "{} [warn] stdout dial <node-server>:443\n{} [error] stderr retry token=***\n",
                entries[1].timestamp, entries[0].timestamp
            )
        );
        let raw = std::fs::read_to_string(original_file).unwrap();
        assert!(raw.contains("dial edge.example:443") && raw.contains("retry token=fixture"));
    }

    #[test]
    fn editing_a_subscription_preserves_identity_and_refreshes_feed_metadata() {
        let (service, _dir) = service();
        let mut sub = Subscription::new("Sample", "https://feed.example/old");
        sub.enabled = false;
        sub.last_updated = Some(100);
        sub.etag = Some("fixture-etag".into());
        sub.headers.insert("X-Format".into(), "plain".into());
        sub.node_count = 1;
        let id = sub.id.clone();
        service.update_subscription_settings(sub).unwrap();
        let mut node = Node::new(
            "Fixture",
            crate::model::Protocol::Socks,
            "proxy.example",
            1080,
        );
        node.subscription_id = Some(id.clone());
        let node_id = node.id.clone();
        service
            .commit_model_change(|model| {
                model.nodes.push(node);
                Ok(true)
            })
            .unwrap();

        service
            .edit_subscription(&id, " Renamed ", "https://feed.example/old")
            .unwrap();
        let sub = service.store().subscriptions().unwrap().pop().unwrap();
        assert_eq!(sub.name, "Renamed");
        assert_eq!(sub.last_updated, Some(100));
        assert!(sub.etag.is_some());
        service
            .edit_subscription(&id, "", " https://feed.example/new ")
            .unwrap();
        let sub = service.snapshot().subscriptions.pop().unwrap();
        assert_eq!(sub.name, "feed.example");
        assert!(sub.headers.contains_key("X-Format"));
        assert!(sub.last_updated.is_none() && sub.etag.is_none());

        service
            .edit_subscription(&id, "New origin", "https://other.example/new")
            .unwrap();
        let sub = service.store().subscriptions().unwrap().pop().unwrap();
        assert_eq!(sub.id, id);
        assert_eq!(sub.url, "https://other.example/new");
        assert!(sub.headers.is_empty());
        assert!(!sub.enabled);
        assert_eq!(sub.node_count, 1);
        assert_eq!(service.model().nodes_of_subscription(&id)[0].id, node_id);
    }

    #[test]
    fn subscription_edits_reject_invalid_duplicate_missing_and_refreshing_targets() {
        let (service, _dir) = service();
        let add = |url| {
            let mut sub = Subscription::new("Original", url);
            sub.enabled = false;
            let id = sub.id.clone();
            service.update_subscription_settings(sub).unwrap();
            id
        };
        let id = add("https://first.example/feed");
        add("https://other.example/feed");
        for url in ["", "file:///feed", "https://other.example/feed"] {
            assert!(service.edit_subscription(&id, "Changed", url).is_err());
        }
        assert!(
            service
                .edit_subscription("missing", "Changed", "https://valid.example")
                .is_err()
        );
        let key = format!("sub:{id}");
        service.set_busy(&key, true);
        assert!(
            service
                .edit_subscription(&id, "Changed", "https://valid.example")
                .is_err()
        );
        assert!(service.delete_subscription(&id).is_err());
        let sub = service
            .store()
            .subscriptions()
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap();
        assert_eq!(sub.name, "Original");
        assert_eq!(sub.url, "https://first.example/feed");
        service.set_busy(&key, false);
        assert!(
            service
                .edit_subscription(&id, "Changed", "https://valid.example")
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn probes_local_versions_once_per_executable_revision() {
        let (service, dir) = service();
        let write_binary = |path: &std::path::Path, output: &str| {
            let script = format!(
                "#!/bin/sh\n[ \"$1\" = version ] || exit 1\necho probe >> \"$0.calls\"\nprintf '%s\\n' '{output}'\n"
            );
            std::fs::write(path, script).unwrap();
            crate::util::set_executable(path).unwrap();
        };
        for core in CoreKind::all() {
            let path = dir.path().join(core.binary_name());
            let output = match core {
                CoreKind::Xray => "Xray 1.2.3 (fixture) (go1.25 windows/amd64)",
                CoreKind::SingBox => "sing-box version 1.2.3\nEnvironment: go1.25 darwin/arm64",
            };
            write_binary(&path, output);
            service
                .set_binary_override(core, Some(path.to_str().unwrap().into()))
                .unwrap();
        }
        service.inner.startup_started.store(true, Ordering::Release);
        service.refresh_local_binaries();
        let wait_for_version = |core: CoreKind, version: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if service.inner.local_binaries.read()[core.as_str()]
                    .info
                    .as_ref()
                    .and_then(|info| info.version.as_deref())
                    == Some(version)
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "version probe timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        wait_for_version(CoreKind::SingBox, "1.2.3");
        wait_for_version(CoreKind::Xray, "1.2.3");
        let path = dir.path().join("xray");
        write_binary(&path, "Xray 1.2.40 (fixture) (go1.25 linux/arm64)");
        service.refresh_local_binaries();
        wait_for_version(CoreKind::Xray, "1.2.40");
        service.refresh_local_binaries();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("xray.calls"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sing-box.calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn custom_executables_are_independent_of_each_other_and_managed_versions() {
        let (service, dir) = service();
        for core in CoreKind::all() {
            let path = dir.path().join(format!(
                "custom-{}{}",
                core.binary_name(),
                std::env::consts::EXE_SUFFIX
            ));
            std::fs::write(&path, b"fixture").unwrap();
            crate::util::set_executable(&path).unwrap();
            service
                .set_binary_override(core, Some(path.to_str().unwrap().into()))
                .unwrap();
            assert_eq!(
                service
                    .core()
                    .resolve_binary(core, &service.settings())
                    .unwrap(),
                path
            );
            assert!(service.snapshot().core_binary_available(core));
            assert!(
                service
                    .set_binary_override(core, Some("missing".into()))
                    .is_err()
            );
            assert_eq!(
                service.settings().binary_override(core).unwrap(),
                path.to_str().unwrap()
            );
        }
        let core = CoreKind::Xray;
        let path = dir
            .path()
            .join(format!("managed{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"managed fixture").unwrap();
        crate::util::set_executable(&path).unwrap();
        service
            .core()
            .binaries()
            .adopt_existing(core, "1.0", &path)
            .unwrap();
        assert!(service.set_active_version(core, "missing").is_err());
        service.set_active_version(core, "1.0").unwrap();
        assert_eq!(
            service
                .core()
                .resolve_binary(core, &service.settings())
                .unwrap(),
            path
        );
        let saved = service
            .settings()
            .local_binary_path(core)
            .unwrap()
            .to_owned();
        let reloaded = Settings::load(&service.paths().settings_file()).unwrap();
        assert_eq!(reloaded.local_binary_path(core), Some(saved.as_str()));
        assert!(reloaded.binary_override(core).is_none());
        service.select_local_binary(core).unwrap();
        assert_eq!(
            service
                .core()
                .resolve_binary(core, &service.settings())
                .unwrap(),
            std::path::Path::new(&saved)
        );
        service.set_active_version(core, "1.0").unwrap();
        service.set_binary_override(core, None).unwrap();
        assert!(service.settings().local_binary_path(core).is_none());
        assert_eq!(
            service.settings().active_version(core).map(String::as_str),
            Some("1.0")
        );
        service
            .set_binary_override(core, Some(saved.clone()))
            .unwrap();
        service.set_binary_override(core, None).unwrap();
        assert!(std::path::Path::new(&saved).exists());
        assert!(service.settings().active_version(core).is_none());
        assert!(service.settings().binary_override(core).is_none());
        assert!(
            service
                .settings()
                .binary_override(CoreKind::SingBox)
                .is_some()
        );
    }

    #[test]
    fn update_badge_keeps_confirmed_candidates_but_discards_stale_checks() {
        let (service, _dir) = service();
        let core = CoreKind::SingBox;
        let settings = service.settings();
        let candidate = || UpdateCandidate {
            core,
            version: "1.2.3".into(),
            asset: updates::ReleaseAsset {
                name: "fixture".into(),
                url: String::new(),
                size: 0,
            },
            checksum_url: None,
            notes: String::new(),
        };
        assert!(!service.snapshot().settings_update_available());
        service.record_core_check(core, &settings, Err(Error::other("离线")));
        assert!(!service.snapshot().settings_update_available());
        service.record_core_check(core, &settings, Ok(Some(candidate())));
        service.record_core_check(core, &settings, Err(Error::other("离线")));
        assert!(service.snapshot().settings_update_available());
        let mut next = settings.clone();
        next.active_versions
            .insert(core.as_str().into(), "1.2.3".into());
        service.save_settings(&next).unwrap();
        assert!(!service.record_core_check(core, &settings, Ok(Some(candidate()))));
        assert!(!service.snapshot().settings_update_available());
        service.inner.helper.write().needs_update = true;
        assert!(service.snapshot().settings_update_available());
    }

    #[cfg(unix)]
    #[test]
    fn rejected_mesh_configuration_preserves_proxy_and_checks_the_running_core() {
        use std::os::unix::fs::PermissionsExt;
        let (service, dir) = service();
        // No network or privileged service: accept the initial config, reject
        // the mesh candidate, and deliberately fail the local version probe.
        let fixture = dir.path().join("fixture-core");
        std::fs::write(
            &fixture,
            r#"#!/bin/sh
case "$*" in
  version) exit 1 ;;
  *runtime-candidate.json*) echo 'unknown field in fixture' >&2; exit 1 ;;
  *-test*) exit 0 ;;
  *) exec sleep 30 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut settings = service.settings();
        settings.core = CoreKind::SingBox;
        settings.binary_paths.insert(
            CoreKind::Xray.as_str().into(),
            fixture.display().to_string(),
        );
        service.save_settings(&settings).unwrap();
        service.inner.rt.block_on(async {
            let core = &service.inner.core;
            let mut model = AppModel::default();
            model.add_nodes([Node::new(
                "fixture",
                Protocol::Socks,
                "proxy.example.test",
                1080,
            )]);
            let started = core.start(&model, &settings, CoreKind::Xray).await.unwrap();
            assert!(started.started);
            let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let Some(pid) = core.state().pid {
                        break pid;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let error = service
                .apply_mesh_plan(crate::mesh::coexist::CoexistencePlan {
                    active: true,
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                Error::MeshCore {
                    core: CoreKind::Xray,
                    ..
                }
            ));
            assert!(error.to_string().contains("内核不识别配置字段"));
            assert_eq!(core.state().pid, Some(pid));
            assert!(!core.mesh_plan().active);
            core.stop().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while service.is_busy("updates-xray") {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let snapshot = service.snapshot();
            assert!(snapshot.updates.cores["xray"].error.is_some());
            assert!(!snapshot.updates.cores.contains_key("sing-box"));
            assert!(!snapshot.validation.unwrap().ok);
        });
    }

    #[test]
    fn diagnostic_rules_are_exact_prioritized_deduplicated_and_persisted() {
        use crate::model::RouteAction;
        let (service, dir) = service();
        let target = DiagnosticTarget {
            domain: "Example.com".into(),
            ..Default::default()
        };
        let count = service.model().routing.rules.len();
        service
            .add_diagnostic_rule(&target, DiagnosticRuleKind::Domain, RouteAction::direct())
            .unwrap();
        service
            .add_diagnostic_rule(&target, DiagnosticRuleKind::Domain, RouteAction::direct())
            .unwrap();
        let model = service.model();
        assert_eq!(model.routing.rules.len(), count + 1);
        assert_eq!(model.routing.rules[0].matchers.domains, ["example.com"]);
        assert_eq!(model.routing.rules[0].action, RouteAction::direct());
        let reloaded = Store::open(&AppPaths::from_root(dir.path()))
            .unwrap()
            .load_profile(&service.snapshot().active_profile.unwrap())
            .unwrap();
        assert_eq!(
            reloaded.unwrap().model.routing.rules[0],
            model.routing.rules[0]
        );
        assert!(
            service
                .add_diagnostic_rule(&target, DiagnosticRuleKind::Process, RouteAction::direct())
                .is_err()
        );
    }

    #[test]
    fn recording_captures_finished_traces_and_replay_cancels_old_publishers() {
        let (service, _dir) = service();
        service.start_recording();
        let target = trace::parse_target("tcp://192.0.2.1:443").unwrap();
        let mut session = TraceSession::new(1, &target);
        service.inner.trace_generation.store(1, Ordering::SeqCst);
        *service.inner.trace.write() = Some(session.clone());
        service.publish_trace(&session);
        assert!(service.snapshot().recording.traces.is_empty());
        session.phase = trace::TracePhase::Failed;
        session.finished_at = Some(now_unix());
        session.success = Some(false);
        service.publish_trace(&session);
        service.publish_trace(&session);
        assert_eq!(service.snapshot().recording.traces.len(), 1);
        service.stop_recording();
        service.inspect_recorded_trace(1);
        assert_ne!(service.trace_snapshot().unwrap().id, 1);
        service.publish_trace(&session);
        assert_ne!(service.trace_snapshot().unwrap().id, 1);
        service.clear_recording();
        assert!(service.snapshot().recording.traces.is_empty());
    }

    #[test]
    fn imports_links_and_exports() {
        let (service, _dir) = service();
        let link = "vless://11111111-2222-3333-4444-555555555555@example.com:443?security=tls&sni=example.com#Node";
        let added = service.import_content(link);
        assert_eq!(added, 1);
        let model = service.model();
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.nodes[0].protocol, Protocol::Vless);
        let exported = service.export_links(&[model.nodes[0].id.clone()]);
        assert!(exported.starts_with("vless://"));
        assert!(service.node_qr(&model.nodes[0].id).unwrap().contains("██"));
    }

    #[test]
    fn imports_clash_groups() {
        let (service, _dir) = service();
        let yaml = r#"
proxies:
  - name: A
    type: ss
    server: a.example.com
    port: 8388
    cipher: aes-256-gcm
    password: pass
proxy-groups:
  - name: 我的组
    type: url-test
    proxies: [A]
"#;
        service.import_content(yaml);
        let model = service.model();
        assert!(
            model
                .groups
                .iter()
                .any(|g| g.name == "我的组" && g.members.len() == 1)
        );
    }

    #[test]
    fn backdrop_preference_persists_without_dirtying_core_config() {
        let (service, _dir) = service();
        let was_dirty = service.snapshot().config_dirty;
        service.set_backdrop(crate::settings::BackdropStyle::Opaque);
        assert_eq!(
            service.snapshot().settings.backdrop,
            crate::settings::BackdropStyle::Opaque
        );
        assert_eq!(service.snapshot().config_dirty, was_dirty);
        let saved = crate::settings::Settings::load(&service.paths().settings_file()).unwrap();
        assert_eq!(saved.backdrop, crate::settings::BackdropStyle::Opaque);
    }

    #[test]
    fn mesh_outbound_updates_preserve_group_references_and_secrets() {
        use crate::mesh::{MeshProfile, MeshProvider};
        let (service, _dir) = service();
        let mut profile = MeshProfile {
            provider: MeshProvider::Tailscale,
            name: "fixture".into(),
            exit_node: "100.64.1.1".into(),
            outbound_port: 19081,
            ..Default::default()
        };
        let id = service.save_mesh_profile(profile.clone()).unwrap();
        let node_id = format!("mesh-exit-{id}");
        let model = service.model();
        let original = model.node(&node_id).unwrap().clone();
        assert_eq!(original.protocol, Protocol::Socks);
        assert!(
            original
                .credentials
                .password
                .as_ref()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(model.groups.iter().any(|g| g.members.contains(&node_id)));
        let group = model
            .groups
            .iter()
            .find(|g| g.members.contains(&node_id))
            .unwrap()
            .id
            .clone();
        profile.outbound_port = 19082;
        service.save_mesh_profile(profile.clone()).unwrap();
        let updated = service.model();
        assert_eq!(updated.node(&node_id).unwrap().port, 19082);
        assert_eq!(
            updated.node(&node_id).unwrap().credentials,
            original.credentials
        );
        assert!(
            updated
                .groups
                .iter()
                .find(|g| g.id == group)
                .unwrap()
                .members
                .contains(&node_id)
        );
        service.create_profile("second").unwrap();
        assert!(service.model().node(&node_id).is_some());
        profile.outbound_port = 0;
        service.save_mesh_profile(profile).unwrap();
        assert!(service.model().node(&node_id).is_none());
        assert!(
            service
                .model()
                .groups
                .iter()
                .all(|g| !g.members.contains(&node_id))
        );
        service.delete_mesh_profile(&id).unwrap();
    }

    #[test]
    fn strict_route_setting_persists_and_updates_tun_intent() {
        let (service, _dir) = service();
        let mut settings = service.settings();
        settings.tun_strict_route = false;
        service.update_settings(settings);
        assert!(!service.settings().tun_strict_route);
        assert!(
            service
                .model()
                .inbounds
                .iter()
                .filter(|i| i.kind == InboundKind::Tun)
                .all(|i| !i.tun.strict_route)
        );
        assert!(
            !Settings::load(&service.paths().settings_file())
                .unwrap()
                .tun_strict_route
        );
    }

    #[test]
    fn tun_toggle_persists_and_updates_model_without_starting_a_stopped_core() {
        let (service, _dir) = service();
        service.set_tun_enabled(true);
        assert!(service.settings().effective_tun());
        assert!(
            service
                .model()
                .inbounds
                .iter()
                .any(|inbound| inbound.kind == InboundKind::Tun && inbound.enabled)
        );
        assert!(
            Settings::load(&service.paths().settings_file())
                .unwrap()
                .tun_enabled
        );
        assert!(!service.is_running());
        assert!(!service.is_busy("core"));
        service.set_tun_enabled(false);
        assert!(!service.settings().tun_enabled);
        assert!(
            service
                .model()
                .inbounds
                .iter()
                .filter(|inbound| inbound.kind == InboundKind::Tun)
                .all(|inbound| !inbound.enabled)
        );
    }

    #[test]
    fn tun_toggle_supports_xray_and_rejects_busy_restarts() {
        let (service, _dir) = service();
        service.set_core(CoreKind::Xray);
        service.set_tun_enabled(true);
        assert!(service.settings().effective_tun());
        assert!(
            service
                .model()
                .inbounds
                .iter()
                .any(|i| i.kind == InboundKind::Tun && i.enabled)
        );
        service.set_tun_enabled(false);
        service.set_core(CoreKind::SingBox);
        service.set_busy("core", true);
        service.set_tun_enabled(true);
        assert!(!service.settings().tun_enabled);
        service.set_busy("core", false);
    }

    #[test]
    fn snapshots_are_consistent() {
        let (service, _dir) = service();
        let snapshot = service.snapshot();
        assert_eq!(snapshot.app_version, APP_VERSION);
        assert!(!snapshot.profiles.is_empty());
        assert_eq!(snapshot.active_profile, service.store().active_profile_id());
        assert_eq!(snapshot.settings.core, service.settings().core);
    }

    #[test]
    fn selecting_direct_and_the_previous_node_preserves_proxy_intent() {
        let (service, _dir) = service();
        assert!(service.select_proxy_mode().is_err());
        let node = Node::new("Example", Protocol::Socks, "example.org", 1080);
        service.inner.model.write().add_nodes([node.clone()]);
        let initial = service.model();
        service.select_proxy_mode().unwrap();
        assert_eq!(service.snapshot().selected_node(None).unwrap().id, node.id);
        service.select_direct_mode().unwrap();
        assert!(service.snapshot().selected_node(None).is_none());
        let direct = service.model();
        assert_eq!(direct.work_mode, WorkMode::Direct);
        assert_eq!(direct.routing, initial.routing);
        assert_eq!(direct.default_group, initial.default_group);
        assert_eq!(direct.groups, initial.groups);
        assert_eq!(direct.nodes, initial.nodes);
        service.select_node("proxy", "missing");
        assert_eq!(service.model().work_mode, WorkMode::Direct);
        service.select_node("proxy", &node.id);
        let profile_id = service.store().active_profile_id().unwrap();
        let saved = service.store().load_profile(&profile_id).unwrap().unwrap();
        assert_eq!(saved.model.work_mode, WorkMode::Proxy);
        assert_eq!(saved.model.groups, initial.groups);
        assert!(!service.is_running());
    }

    #[test]
    fn node_order_survives_profile_reload() {
        let (service, _dir) = service();
        service.import_content("vless://11111111-2222-3333-4444-555555555555@a.example.com:443#A\nvless://11111111-2222-3333-4444-555555555555@b.example.com:443#B");
        let model = service.model();
        let first = model.nodes[0].id.clone();
        let second = model.nodes[1].id.clone();
        service.move_node_to(&first, &second);
        let profile_id = service.store().active_profile_id().unwrap();
        let profile = service.store().load_profile(&profile_id).unwrap().unwrap();
        assert_eq!(profile.model.nodes[0].id, second);
        assert_eq!(profile.model.nodes[1].id, first);
    }

    #[test]
    fn switch_policy_roundtrips_without_starting_or_changing_the_manual_choice() {
        use crate::model::{FallbackScope, NodeSwitchPolicy, SwitchMode};
        let (service, _dir) = service();
        let mut model = service.model();
        model.work_mode = WorkMode::Proxy;
        let a = Node::new("A", Protocol::Socks, "a.example", 1080);
        let b = Node::new("B", Protocol::Socks, "b.example", 1080);
        model.add_nodes([a.clone(), b.clone()]);
        let mut group =
            crate::model::OutboundGroup::new("测速组", crate::model::GroupKind::UrlTest);
        group.id = "auto".into();
        group.members = vec![a.id.clone(), b.id.clone()];
        model.groups.push(group);
        *service.inner.model.write() = model;
        let policy = NodeSwitchPolicy {
            mode: SwitchMode::Throughput,
            fallback: FallbackScope::SameSubscription,
            consecutive_failures: 3,
        };
        service.set_node_switch_policy("proxy", policy, 60).unwrap();
        service.select_node("proxy", &b.id);
        let snapshot = service.snapshot();
        let stored = service
            .store()
            .load_profile(snapshot.active_profile.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored.model.group("proxy").unwrap().switch_policy, policy);
        assert_eq!(stored.model.group("proxy").unwrap().interval_secs, 60);
        assert_eq!(snapshot.selected_node(None).unwrap().id, a.id);
        assert!(!snapshot.config_dirty && !service.is_running());
        assert!(service.set_node_switch_policy("proxy", policy, 10).is_err());
        assert!(service.set_default_outbound_group("missing").is_err());
        service.set_default_outbound_group("auto").unwrap();
        assert_eq!(service.model().default_group.as_deref(), Some("auto"));
        let mut snapshot = snapshot;
        snapshot.process.phase = crate::core::ProcessPhase::Running;
        snapshot
            .runtime
            .managed_nodes
            .insert("proxy".into(), b.id.clone());
        assert_eq!(snapshot.selected_node(None).unwrap().id, b.id);
    }

    #[test]
    fn selected_node_follows_group_selection_and_live_automatic_choice() {
        use crate::model::{GroupKind, OutboundGroup};

        let (service, _dir) = service();
        let mut snapshot = service.snapshot();
        assert!(snapshot.selected_node(None).is_none());

        let a = Node::new("节点 A", Protocol::Vless, "a.example.com", 443);
        let b = Node::new("节点 B", Protocol::Vless, "b.example.com", 443);
        let mut manual = OutboundGroup::new("手动", GroupKind::Selector);
        manual.members = vec![a.id.clone(), b.id.clone()];
        manual.selected = Some(a.id.clone());
        let mut auto = OutboundGroup::new("自动", GroupKind::UrlTest);
        auto.members = manual.members.clone();
        auto.selected = Some(b.id.clone());
        snapshot.model.work_mode = WorkMode::Proxy;
        snapshot.model.nodes = vec![a.clone(), b.clone()];
        snapshot.model.groups = vec![manual.clone(), auto.clone()];
        snapshot.model.default_group = Some(manual.id.clone());
        assert_eq!(snapshot.selected_node(None).unwrap().id, a.id);
        assert_eq!(
            snapshot.selected_node(Some("stale-group")).unwrap().id,
            a.id
        );
        assert_eq!(snapshot.selected_node(Some(&auto.id)).unwrap().id, b.id);

        snapshot.runtime.proxies.insert(
            auto.tag(),
            ClashProxy {
                now: Some(a.outbound_tag()),
                ..Default::default()
            },
        );
        snapshot.settings.core = CoreKind::SingBox;
        snapshot.process.phase = crate::core::ProcessPhase::Running;
        assert_eq!(snapshot.selected_node(Some(&auto.id)).unwrap().id, a.id);
        snapshot.runtime.proxies.insert(
            manual.tag(),
            ClashProxy {
                now: Some(b.outbound_tag()),
                ..Default::default()
            },
        );
        // A just-clicked selector must not be overwritten by stale API data.
        assert_eq!(snapshot.selected_node(Some(&manual.id)).unwrap().id, a.id);
        snapshot.process.phase = crate::core::ProcessPhase::Stopped;
        assert_eq!(snapshot.selected_node(Some(&auto.id)).unwrap().id, b.id);
        snapshot.process.phase = crate::core::ProcessPhase::Running;
        snapshot.settings.core = CoreKind::Xray;
        assert_eq!(snapshot.selected_node(Some(&auto.id)).unwrap().id, b.id);
    }

    #[test]
    fn location_batch_deduplicates_bounds_concurrency_and_refreshes_results() {
        use std::sync::atomic::AtomicUsize;
        let (service, _dir) = service();
        let language = service.settings().language;
        let mut nodes: Vec<_> = (0..6)
            .map(|i| {
                Node::new(
                    format!("演示节点 {i}"),
                    Protocol::Vless,
                    format!("server-{i}.example"),
                    443,
                )
            })
            .collect();
        nodes.push(Node::new(
            "重复服务器",
            Protocol::Vless,
            "SERVER-0.EXAMPLE",
            8443,
        ));
        let mut disabled = Node::new("已禁用", Protocol::Vless, "disabled.example", 443);
        disabled.enabled = false;
        nodes.push(disabled);
        nodes.push(Node::new(
            "正在探测",
            Protocol::Vless,
            "pending.example",
            443,
        ));
        service.inner.model.write().nodes = nodes;
        let pending_key = location::cache_key("pending.example", language);
        service.inner.node_locations.write().insert(
            pending_key.clone(),
            NodeLocation {
                pending: true,
                ..Default::default()
            },
        );
        let refreshed_key = location::cache_key("server-0.example", language);
        service.inner.node_locations.write().insert(
            refreshed_key.clone(),
            NodeLocation {
                label: Some("旧位置".into()),
                country: None,
                checked_at: now_unix(),
                pending: false,
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        service.request_all_node_locations_with({
            let calls = calls.clone();
            let gate = gate.clone();
            move |server, _| {
                let calls = calls.clone();
                let gate = gate.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    gate.acquire().await.unwrap().forget();
                    NodeLocation {
                        label: (!server.contains("server-5")).then(|| "演示位置".into()),
                        checked_at: now_unix(),
                        ..Default::default()
                    }
                }
            }
        });
        assert!(service.is_busy("node-locations"));
        service.request_all_node_locations_with(|_, _| async { panic!("duplicate batch") });
        service.inner.rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while calls.load(Ordering::SeqCst) < 4 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(calls.load(Ordering::SeqCst), 4);
                gate.add_permits(6);
                while service.is_busy("node-locations") {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        });
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        let locations = service.inner.node_locations.read();
        assert_eq!(locations[&refreshed_key].label.as_deref(), Some("演示位置"));
        let failed = &locations[&location::cache_key("server-5.example", language)];
        assert!(!failed.pending && failed.label.is_none() && failed.checked_at > 0);
        assert!(locations[&pending_key].pending);
        assert!(!locations.contains_key(&location::cache_key("disabled.example", language)));
    }

    #[test]
    fn empty_location_batch_clears_busy_without_lookup() {
        let (service, _dir) = service();
        service.request_all_node_locations_with(|_, _| async { panic!("empty batch") });
        assert!(!service.is_busy("node-locations"));
    }

    #[test]
    fn location_snapshot_is_scoped_to_server_and_language() {
        let (service, _dir) = service();
        let mut snapshot = service.snapshot();
        let mut node = Node::new("节点", Protocol::Vless, "example.com", 443);
        snapshot.settings.language = crate::settings::Language::Zh;
        snapshot.node_locations.insert(
            location::cache_key(&node.server, snapshot.settings.language),
            NodeLocation {
                label: Some("新加坡".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            snapshot.node_location(&node).unwrap().label.as_deref(),
            Some("新加坡")
        );
        node.server = "changed.example.com".into();
        assert!(snapshot.node_location(&node).is_none());
        node.server = "example.com".into();
        snapshot.settings.language = crate::settings::Language::En;
        assert!(snapshot.node_location(&node).is_none());
    }

    #[test]
    fn profiles_can_be_created_and_switched() {
        let (service, _dir) = service();
        service.create_profile("工作").unwrap();
        assert_eq!(service.model().name, "工作");
        let profiles = service.snapshot().profiles;
        assert_eq!(profiles.len(), 2);
        service.switch_profile(&profiles[0].id).unwrap();
        assert_eq!(service.model().name, profiles[0].name);
    }

    #[test]
    fn settings_update_model_ports() {
        let (service, _dir) = service();
        let mut settings = service.settings();
        settings.mixed_port = 10888;
        service.update_settings(settings);
        let model = service.model();
        let mixed = model
            .inbounds
            .iter()
            .find(|i| i.kind == InboundKind::Mixed)
            .unwrap();
        assert_eq!(mixed.port, 10888);
    }

    #[test]
    fn domain_strategy_persists_without_changing_rules_or_fallback() {
        let (service, _dir) = service();
        let before = service.model().routing;
        service.set_domain_strategy(crate::model::DomainStrategy::AsIs);
        let after = service.model().routing;
        assert_eq!(after.domain_strategy, crate::model::DomainStrategy::AsIs);
        assert_eq!(after.rules, before.rules);
        assert_eq!(after.rule_groups, before.rule_groups);
        assert_eq!(after.final_action, before.final_action);
        let profile = service
            .store()
            .load_profile(&service.store().active_profile_id().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(profile.model.routing, after);
        assert!(!service.is_running());
    }

    #[test]
    fn route_preset_replaces_rules() {
        let (service, _dir) = service();
        service.set_route_preset(RoutePreset::Global);
        assert_eq!(service.model().routing.rules.len(), 2);
        assert!(
            service
                .model()
                .routing
                .rules
                .iter()
                .all(|rule| rule.enabled && rule.action == crate::model::RouteAction::direct())
        );
        service.set_route_preset(RoutePreset::BypassMainland);
        assert!(!service.model().routing.rules.is_empty());
    }

    #[test]
    fn global_start_needs_no_cache_and_manual_update_reports_each_set() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let cache = paths.rule_sets_dir();
        // An existing file without a sidecar predates the current run.
        crate::util::atomic_write(&cache.join("geoip-cn.srs"), b"cached").unwrap();
        let service = AppService::with_paths(paths).unwrap();
        let before = service
            .snapshot()
            .rule_set_updates
            .into_iter()
            .find(|row| row.tag == "geoip-cn")
            .unwrap();
        assert!(before.updated_at.is_some());
        let mut settings = service.settings();
        settings.proxy_resource_updates = true;
        service.save_settings(&settings).unwrap();
        service
            .commit_model_change(|model| {
                model.add_nodes([Node::new("Fixture", Protocol::Socks, "proxy.example", 1080)]);
                model.work_mode = WorkMode::Proxy;
                model.apply_preset(RoutePreset::Global);
                Ok(true)
            })
            .unwrap();
        let model = service.model();
        service.inner.rt.block_on(async {
            let _manual_download = service.inner.rule_set_lock.lock().await;
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                service.ensure_rule_sets(&model, &settings),
            )
            .await
            .expect("global startup waited for optional downloads")
            .unwrap();
        });
        service.sync_rule_sets();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.is_busy("rulesets") {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snapshot = service.snapshot();
        assert_eq!(snapshot.model.routing.preset, RoutePreset::Global);
        let sets = snapshot.rule_sets();
        assert_eq!(snapshot.rule_set_updates.len(), sets.len());
        // The resource proxy is deliberately unavailable, so no network is accessed.
        for set in sets {
            let row = snapshot
                .rule_set_updates
                .iter()
                .find(|r| r.tag == set.tag)
                .unwrap();
            assert_eq!(row.status, "failed");
            assert!(row.error.is_some());
            assert_eq!(
                row.updated_at,
                if set.tag == "geoip-cn" {
                    before.updated_at
                } else {
                    None
                }
            );
        }
    }

    #[test]
    fn quick_route_switch_persists_without_starting_a_stopped_core() {
        let (service, _dir) = service();
        service.switch_route_preset(RoutePreset::Global);
        assert_eq!(service.model().routing.preset, RoutePreset::Global);
        assert_eq!(service.model().routing.rules.len(), 2);
        assert!(
            service
                .model()
                .routing
                .rules
                .iter()
                .all(|rule| rule.enabled && rule.action == crate::model::RouteAction::direct())
        );
        let profile_id = service.store().active_profile_id().unwrap();
        let saved = service.store().load_profile(&profile_id).unwrap().unwrap();
        assert_eq!(saved.model.routing.preset, RoutePreset::Global);
        assert!(!service.is_running());
        assert!(!service.is_busy("core"));
        service.switch_route_preset(RoutePreset::BypassMainland);
        assert!(!service.model().routing.rules.is_empty());
    }

    #[test]
    fn quick_route_switch_preserves_same_preset_rules_and_ignores_busy_core() {
        let (service, _dir) = service();
        let mut model = service.model();
        model.routing.preset = RoutePreset::Custom;
        *service.inner.model.write() = model.clone();
        service.switch_route_preset(RoutePreset::Custom);
        assert_eq!(
            service.model().routing.rules.len(),
            model.routing.rules.len()
        );
        service.set_busy("core", true);
        service.switch_route_preset(RoutePreset::Global);
        assert_eq!(service.model().routing.preset, RoutePreset::Custom);
        service.set_busy("core", false);
    }

    #[test]
    fn subscription_merge_dedupes() {
        let (service, _dir) = service();
        let subscription = Subscription::new("sub", "https://example.com/sub");
        service.store().save_subscription(&subscription).unwrap();
        service.inner.subs.write().push(subscription.clone());
        let content = "vless://11111111-2222-3333-4444-555555555555@a.example.com:443?security=tls#A\nvless://11111111-2222-3333-4444-555555555555@a.example.com:443?security=tls#A";
        let parsed = parse_subscription(content);
        let added = service.merge_parsed(&subscription.id, &parsed).unwrap();
        assert_eq!(added, 1);
        assert_eq!(
            service
                .model()
                .nodes_of_subscription(&subscription.id)
                .len(),
            1
        );
    }

    #[test]
    fn selecting_local_binary_registers_it_for_helper_without_managed_row() {
        let (service, dir) = service();
        let ext = std::env::consts::EXE_SUFFIX;
        let external = dir.path().join(format!("sing-box{ext}"));
        std::fs::write(&external, b"custom-sing-box").unwrap();
        crate::util::set_executable(&external).unwrap();

        service
            .set_binary_override(
                CoreKind::SingBox,
                Some(external.to_string_lossy().into_owned()),
            )
            .unwrap();
        assert!(service.snapshot().binaries.is_empty());
        let registered = service
            .core()
            .binaries()
            .find_registered(&external)
            .expect("custom binary should be registered for helper");
        assert!(registered.local);
        assert!(service.core().binaries().verify(&registered).unwrap());

        service
            .core()
            .binaries()
            .unregister_local(CoreKind::SingBox)
            .unwrap();
        assert!(
            service
                .core()
                .binaries()
                .find_registered(&external)
                .is_none()
        );
        service.select_local_binary(CoreKind::SingBox).unwrap();
        assert!(
            service
                .core()
                .binaries()
                .find_registered(&external)
                .is_some()
        );
    }

    fn fixture_mesh_profile(
        id: &str,
        provider: crate::mesh::MeshProvider,
    ) -> crate::mesh::MeshProfileView {
        crate::mesh::MeshProfileView {
            id: id.into(),
            name: id.into(),
            provider,
            hostname: "fixture-host".into(),
            auto_connect: false,
            auto_reconnect: true,
            easytier: Default::default(),
            network_name: "fixture".into(),
            has_secret: false,
            peers: Vec::new(),
            ipv4: String::new(),
            remote_subnets: Vec::new(),
            control_url: String::new(),
            exit_node: String::new(),
            advertise_exit_node: false,
            exit_routing: Default::default(),
            advertised_subnets: Vec::new(),
            port_forwards: Vec::new(),
            service_forwards: Vec::new(),
            outbound_port: 0,
        }
    }

    #[test]
    fn preferred_mesh_profile_prefers_startup_then_first_saved() {
        use crate::mesh::MeshProvider;
        let profiles = vec![
            fixture_mesh_profile("et-1", MeshProvider::EasyTier),
            fixture_mesh_profile("et-2", MeshProvider::EasyTier),
            fixture_mesh_profile("ts-1", MeshProvider::Tailscale),
        ];
        let mut settings = Settings::default();
        assert_eq!(
            preferred_mesh_profile_id(&profiles, &settings, MeshProvider::EasyTier).as_deref(),
            Some("et-1")
        );
        settings.startup_easytier = Some("et-2".into());
        assert_eq!(
            preferred_mesh_profile_id(&profiles, &settings, MeshProvider::EasyTier).as_deref(),
            Some("et-2")
        );
        settings.startup_easytier = Some("missing".into());
        assert_eq!(
            preferred_mesh_profile_id(&profiles, &settings, MeshProvider::EasyTier).as_deref(),
            Some("et-1")
        );
        assert_eq!(
            preferred_mesh_profile_id(&profiles, &settings, MeshProvider::Tailscale).as_deref(),
            Some("ts-1")
        );
    }

    #[test]
    fn unified_start_follows_startup_when_set_and_falls_back_to_all_when_unset() {
        use crate::mesh::{MeshProvider, MeshSnapshot};
        let mesh = MeshSnapshot {
            supported: true,
            profiles: vec![
                fixture_mesh_profile("et-1", MeshProvider::EasyTier),
                fixture_mesh_profile("ts-1", MeshProvider::Tailscale),
                fixture_mesh_profile("ts-2", MeshProvider::Tailscale),
            ],
            ..Default::default()
        };
        let mut settings = Settings::default();
        // Neither startup_proxy nor startup_* mesh selected -> start core + preferred profiles.
        let plan = plan_unified_start(&settings, &mesh);
        assert!(plan.start_core);
        assert_eq!(plan.mesh_profile_ids, vec!["ts-1", "et-1"]);
        assert!(plan.missing_startup.is_empty());

        // Only startup Tailscale selected -> follow startup choices only (do not start core or EasyTier).
        settings.startup_tailscale = Some("ts-2".into());
        let plan = plan_unified_start(&settings, &mesh);
        assert!(!plan.start_core);
        assert_eq!(plan.mesh_profile_ids, vec!["ts-2"]);
        assert!(plan.missing_startup.is_empty());

        // Only startup proxy selected -> follow startup choices only.
        settings.startup_tailscale = None;
        settings.startup_proxy = true;
        let plan = plan_unified_start(&settings, &mesh);
        assert!(plan.start_core);
        assert!(plan.mesh_profile_ids.is_empty());

        // Missing startup mesh profile -> reports missing provider.
        settings.startup_proxy = false;
        settings.startup_easytier = Some("gone".into());
        let plan = plan_unified_start(&settings, &mesh);
        assert!(!plan.start_core);
        assert!(plan.mesh_profile_ids.is_empty());
        assert_eq!(plan.missing_startup, vec![MeshProvider::EasyTier]);
    }
}
