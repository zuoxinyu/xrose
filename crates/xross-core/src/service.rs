//! The single facade the UI talks to. All heavy work is dispatched to an
//! internal Tokio runtime; the UI polls [`UiSnapshot`] and issues plain calls.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::adapters::{CompiledConfig, ValidationReport};
use crate::core::logs::{LogBus, LogEntry, LogStream};
use crate::core::{BinaryRecord, CoreManager, ProcessState, StartOutcome};
use crate::diagnostics::{DiagnosticRecording, DiagnosticRuleKind, DiagnosticTarget};
use crate::dns::{self, DnsPreset};
use crate::error::{Error, Result};
use crate::model::{AppModel, CoreKind, InboundKind, Node, NodeDraft, RoutePreset, RouteRule};
use crate::paths::AppPaths;
use crate::runtime::location::{self, NodeLocation};
use crate::runtime::{
    ClashClient, ClashConnection, ClashProxy, TrafficHistory, TrafficSample, XrayStatsClient,
};
use crate::settings::LogLevel;
use crate::settings::Settings;
use crate::storage::{LatencyEntry, ProfileMeta, Store};
use crate::subscription::{parse_subscription, qr_ascii, Subscription, SubscriptionUsage};
use crate::system::privilege::{self, HelperRequest, HelperStatus};
use crate::system::{self, DoctorCheck, PacServer, ProxyManager};
use crate::trace::{self, TraceSession};
use crate::updates::{self, AppUpdate, RuleSetUpdate, UpdateCandidate};
use crate::util::now_unix;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Toast {
    pub id: u64,
    pub level: ToastLevel,
    pub message: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub proxy_backend: String,
    pub elevated: bool,
    pub helper_available: bool,
    pub helper_installed: bool,
    pub helper_supported: bool,
    pub helper_detail: String,
    pub helper_path: Option<String>,
    pub log_dir: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateStatus {
    pub core: Option<UpdateCandidate>,
    pub app: Option<AppUpdate>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeView {
    pub connections: Vec<ClashConnection>,
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
    pub app_version: String,
    pub settings: Settings,
    pub model: AppModel,
    pub process: ProcessState,
    pub logs: Vec<LogEntry>,
    pub subscriptions: Vec<Subscription>,
    pub profiles: Vec<ProfileMeta>,
    pub active_profile: Option<String>,
    pub runtime: RuntimeView,
    pub latency: BTreeMap<String, LatencyEntry>,
    #[serde(default)]
    pub node_locations: BTreeMap<String, NodeLocation>,
    pub validation: Option<ValidationReport>,
    pub compiled_warnings: Vec<String>,
    pub compiled_json: Option<String>,
    pub binaries: Vec<BinaryRecord>,
    pub toasts: Vec<Toast>,
    pub busy: Vec<String>,
    pub platform: PlatformInfo,
    pub doctor: Vec<DoctorCheck>,
    pub updates: UpdateStatus,
    pub system_proxy_active: bool,
    pub pac_url: Option<String>,
    pub config_dirty: bool,
    pub recovery_note: Option<String>,
    pub rule_set_updates: Vec<RuleSetUpdate>,
    pub trace: Option<TraceSession>,
    #[serde(default)]
    pub recording: DiagnosticRecording,
}

impl UiSnapshot {
    pub fn rule_sets(&self) -> Vec<crate::model::RuleSet> {
        crate::adapters::required_rule_sets(&self.model)
    }

    /// Resolve the same group as the node list, including live automatic choices.
    pub fn selected_node(&self, group_id: Option<&str>) -> Option<&Node> {
        let group = group_id.and_then(|id| self.model.group(id)).or_else(|| {
            self.model
                .default_group_id()
                .and_then(|id| self.model.group(&id))
        })?;
        if self.process.phase.is_alive()
            && self.settings.core == CoreKind::SingBox
            && group.kind != crate::model::GroupKind::Selector
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

struct Inner {
    mesh: crate::mesh::controller::MeshController,
    paths: AppPaths,
    settings: RwLock<Settings>,
    model: RwLock<AppModel>,
    store: Arc<Store>,
    core: Arc<CoreManager>,
    rt: Runtime,
    subs: RwLock<Vec<Subscription>>,
    profiles: RwLock<Vec<ProfileMeta>>,
    active_profile: RwLock<Option<String>>,
    traffic: Mutex<TrafficHistory>,
    connections: RwLock<Vec<ClashConnection>>,
    proxies: RwLock<BTreeMap<String, ClashProxy>>,
    latency: RwLock<BTreeMap<String, LatencyEntry>>,
    node_locations: RwLock<BTreeMap<String, NodeLocation>>,
    busy: RwLock<BTreeSet<String>>,
    toasts: Mutex<Vec<Toast>>,
    proxy: Mutex<ProxyManager>,
    pac: Mutex<Option<PacServer>>,
    validation: RwLock<Option<ValidationReport>>,
    compiled: RwLock<Option<CompiledConfig>>,
    compiled_json: RwLock<Option<String>>,
    config_dirty: AtomicBool,
    updates: Mutex<UpdateStatus>,
    doctor: RwLock<Vec<DoctorCheck>>,
    doctor_generation: AtomicU64,
    recovery_note: RwLock<Option<String>>,
    helper: RwLock<HelperStatus>,
    rule_set_updates: RwLock<Vec<RuleSetUpdate>>,
    trace: RwLock<Option<TraceSession>>,
    trace_generation: AtomicU64,
    recording: Mutex<DiagnosticRecording>,
    toast_seq: AtomicU64,
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
        let settings = Settings::load(&paths.settings_file())?;
        if !paths.settings_file().exists() {
            let _ = settings.save(&paths.settings_file());
        }
        if std::env::var_os("XROSS_DEBUG").is_some() {
            eprintln!("[debug] settings loaded");
        }
        let store = Store::open(&paths)?;
        if std::env::var_os("XROSS_DEBUG").is_some() {
            eprintln!("[debug] store opened");
        }
        let active_profile = store.active_profile_id();
        let model = active_profile
            .as_deref()
            .and_then(|id| store.load_profile(id).ok().flatten())
            .map(|p| p.model)
            .unwrap_or_default();
        let logs = LogBus::with_file(
            paths.logs_dir(),
            "xross",
            settings.log_max_bytes,
            settings.log_keep,
        );
        logs.load_tail(&paths.logs_dir().join("xross.log"), 300);
        let core = Arc::new(CoreManager::new(paths.clone(), logs));
        if std::env::var_os("XROSS_DEBUG").is_some() {
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
        if std::env::var_os("XROSS_DEBUG").is_some() {
            eprintln!("[debug] proxy backend detected");
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("xross-worker")
            .build()
            .map_err(|e| Error::other(format!("无法创建异步运行时：{e}")))?;

        let mesh = crate::mesh::controller::MeshController::new(paths.clone(), store.clone())?;
        let inner = Arc::new(Inner {
            mesh,
            paths,
            settings: RwLock::new(settings),
            model: RwLock::new(model),
            store,
            core,
            rt,
            subs: RwLock::new(subs),
            profiles: RwLock::new(profiles),
            active_profile: RwLock::new(active_profile),
            traffic: Mutex::new(TrafficHistory::new(180)),
            connections: RwLock::new(Vec::new()),
            proxies: RwLock::new(BTreeMap::new()),
            latency: RwLock::new(latency),
            node_locations: RwLock::new(BTreeMap::new()),
            busy: RwLock::new(BTreeSet::new()),
            toasts: Mutex::new(Vec::new()),
            proxy: Mutex::new(proxy),
            pac: Mutex::new(None),
            validation: RwLock::new(None),
            compiled: RwLock::new(None),
            compiled_json: RwLock::new(None),
            config_dirty: AtomicBool::new(false),
            updates: Mutex::new(UpdateStatus::default()),
            doctor: RwLock::new(Vec::new()),
            doctor_generation: AtomicU64::new(0),
            recovery_note: RwLock::new(None),
            helper: RwLock::new(privilege::initial_status()),
            rule_set_updates: RwLock::new(Vec::new()),
            trace: RwLock::new(None),
            trace_generation: AtomicU64::new(0),
            recording: Mutex::new(DiagnosticRecording::default()),
            toast_seq: AtomicU64::new(1),
        });
        let service = Self {
            inner: inner.clone(),
        };

        if let Some(snapshot) = inner.proxy.lock().recover_after_crash() {
            let settings = inner.settings.read().clone();
            if settings.system_proxy {
                let mut updated = settings;
                updated.system_proxy = false;
                let _ = updated.save(&inner.paths.settings_file());
                *inner.settings.write() = updated;
            }
            *inner.recovery_note.write() = Some(format!(
                "上次退出时系统代理未还原（{}），已自动恢复",
                snapshot.backend
            ));
            service.toast(ToastLevel::Warning, "已恢复上次异常退出时的系统代理设置");
        }

        if std::env::var_os("XROSS_DEBUG").is_some() {
            eprintln!("[debug] runtime ready");
        }
        service.refresh_doctor();
        if std::env::var_os("XROSS_DEBUG").is_some() {
            eprintln!("[debug] doctor done");
        }
        service.start_monitor();
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
        let settings = self.inner.settings.read().clone();
        let model = self.inner.model.read().clone();
        let process = self.inner.core.state();
        let (up_total, down_total, up_speed, down_speed) = self.inner.traffic.lock().snapshot();
        let history = self.inner.traffic.lock().samples();
        let pac_url = self.inner.pac.lock().as_ref().map(|p| p.info.url.clone());
        let proxy_backend = self.inner.proxy.lock().backend().label().to_string();
        let doctor = self.inner.doctor.read().clone();
        let validation = self.inner.validation.read().clone();
        let helper = self.inner.helper.read().clone();
        let runtime = RuntimeView {
            connections: self.inner.connections.read().clone(),
            proxies: self.inner.proxies.read().clone(),
            up_total,
            down_total,
            up_speed,
            down_speed,
            history,
            error: None,
        };
        UiSnapshot {
            mesh: self.inner.mesh.snapshot(),
            app_version: APP_VERSION.into(),
            settings,
            model,
            process,
            logs: self.inner.core.logs().recent(800, LogLevel::Trace, ""),
            subscriptions: self.inner.subs.read().clone(),
            profiles: self.inner.profiles.read().clone(),
            active_profile: self.inner.active_profile.read().clone(),
            runtime,
            latency: self.inner.latency.read().clone(),
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
            toasts: self.active_toasts(),
            busy: self.inner.busy.read().iter().cloned().collect(),
            platform: PlatformInfo {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                proxy_backend,
                elevated: helper.elevated,
                helper_available: helper.running,
                helper_installed: helper.installed,
                helper_supported: helper.supported,
                helper_detail: helper.detail.clone(),
                helper_path: helper.executable.clone(),
                log_dir: self.inner.paths.logs_dir().display().to_string(),
                data_dir: self.inner.paths.data_dir.display().to_string(),
            },
            doctor,
            updates: self.inner.updates.lock().clone(),
            system_proxy_active: self.inner.settings.read().system_proxy,
            pac_url,
            config_dirty: self.inner.config_dirty.load(Ordering::SeqCst),
            recovery_note: self.inner.recovery_note.read().clone(),
            rule_set_updates: self.inner.rule_set_updates.read().clone(),
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
            let label = location::lookup(&server, language).await;
            service.inner.node_locations.write().insert(
                key,
                NodeLocation {
                    label,
                    pending: false,
                    checked_at: now_unix(),
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
        }
    }

    pub fn is_busy(&self, key: &str) -> bool {
        self.inner.busy.read().contains(key)
    }

    pub fn toast(&self, level: ToastLevel, message: impl Into<String>) {
        let id = self.inner.toast_seq.fetch_add(1, Ordering::SeqCst);
        let message = message.into();
        let toast = Toast {
            id,
            level,
            message: message.clone(),
            at: now_unix(),
        };
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

    fn mark_dirty(&self) {
        if self.is_running() {
            self.inner.config_dirty.store(true, Ordering::SeqCst);
        }
    }

    fn sync_model_settings(&self) {
        let settings = self.inner.settings.read().clone();
        let mut model = self.inner.model.write();
        for inbound in model.inbounds.iter_mut() {
            match inbound.kind {
                InboundKind::Mixed => {
                    inbound.port = settings.mixed_port;
                    inbound.allow_lan = settings.allow_lan;
                    inbound.listen = if settings.allow_lan {
                        "0.0.0.0".into()
                    } else {
                        settings.listen_host.clone()
                    };
                }
                InboundKind::Tun => {
                    inbound.enabled = settings.tun_enabled;
                }
                _ => {}
            }
        }
    }

    fn save_settings(&self, settings: &Settings) -> Result<()> {
        settings.save(&self.inner.paths.settings_file())?;
        *self.inner.settings.write() = settings.clone();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Profiles
    // ------------------------------------------------------------------

    pub fn switch_profile(&self, id: &str) -> Result<()> {
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
        *self.inner.model.write() = profile.model;
        *self.inner.active_profile.write() = Some(id.to_string());
        self.inner.store.set_active_profile(id)?;
        self.inner.validation.write().take();
        self.inner.compiled.write().take();
        self.inner.compiled_json.write().take();
        self.inner.config_dirty.store(false, Ordering::SeqCst);
        self.sync_model_settings();
        self.refresh_doctor();
        self.refresh_profiles();
        self.toast(
            ToastLevel::Success,
            format!("已切换到配置“{}”", profile.name),
        );
        Ok(())
    }

    pub fn create_profile(&self, name: impl Into<String>) -> Result<()> {
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
        let id = self.inner.store.save_profile(&model)?;
        self.inner.store.set_active_profile(&id)?;
        *self.inner.model.write() = model;
        *self.inner.active_profile.write() = Some(id);
        self.sync_model_settings();
        self.refresh_profiles();
        self.toast(ToastLevel::Success, "已创建新配置");
        Ok(())
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
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
        let url = url.into();
        if url.trim().is_empty() {
            return Err(Error::other("订阅地址不能为空"));
        }
        let name = name.into();
        let mut subscription = Subscription::new(
            if name.trim().is_empty() {
                pretty_host(&url)
            } else {
                name
            },
            url,
        );
        subscription.sort = self.inner.subs.read().len() as i64;
        self.inner.store.save_subscription(&subscription)?;
        self.inner.subs.write().push(subscription.clone());
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

    pub fn delete_subscription(&self, id: &str) -> Result<()> {
        self.inner.store.delete_subscription(id)?;
        {
            let mut model = self.inner.model.write();
            model.remove_subscription_nodes(id);
            for group in &mut model.groups {
                group.repair();
            }
        }
        let _ = self.persist_model();
        self.inner.subs.write().retain(|s| s.id != id);
        self.refresh_doctor();
        self.toast(ToastLevel::Info, "订阅及对应节点已删除");
        Ok(())
    }

    pub fn refresh_subscription(&self, id: &str) {
        let Some(subscription) = self.inner.subs.read().iter().find(|s| s.id == id).cloned() else {
            self.toast(ToastLevel::Error, "订阅不存在");
            return;
        };
        let key = format!("sub:{id}");
        if self.is_busy(&key) {
            return;
        }
        self.set_busy(&key, true);
        let service = self.clone();
        self.spawn(async move {
            let request = crate::subscription::FetchRequest {
                url: subscription.url.clone(),
                user_agent: subscription.user_agent.clone(),
                headers: subscription.headers.clone(),
                etag: subscription.etag.clone(),
                last_modified: subscription.last_modified.clone(),
            };
            let result = crate::subscription::fetch_subscription(request).await;
            service.set_busy(&key, false);
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
                    let added = service.merge_parsed(&subscription.id, &parsed);
                    let mut updated = subscription.clone();
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
    ) -> usize {
        let mut model = self.inner.model.write();
        let before: BTreeSet<String> = model.nodes.iter().map(|n| n.id.clone()).collect();
        let mut incoming: Vec<Node> = parsed.nodes.clone();
        for node in &mut incoming {
            node.subscription_id = Some(subscription_id.to_string());
            node.source = crate::model::NodeSource::Subscription;
        }
        let added = model.add_nodes(incoming);
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
        drop(model);
        let _ = self.persist_model();
        self.refresh_doctor();
        self.mark_dirty();
        added
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
        let added = self.merge_parsed("import", &parsed);
        self.toast(
            ToastLevel::Success,
            format!("已导入 {} 个节点（{}）", added, parsed.format.label()),
        );
        added
    }

    /// Adds a node entered in the manual form. Validation lives in
    /// [`crate::model::NodeDraft::into_node`]; the returned id can be used to
    /// test or select the node right away.
    pub fn add_manual_node(&self, draft: NodeDraft) -> Result<String> {
        let core = self.inner.settings.read().core;
        let node = draft.into_node(core)?;
        let fingerprint = node.fingerprint();
        {
            let mut model = self.inner.model.write();
            if model.nodes.iter().any(|n| n.fingerprint() == fingerprint) {
                return Err(Error::other(
                    "已存在相同节点（协议 / 地址 / 端口 / 凭据一致）",
                ));
            }
            if model.add_nodes([node.clone()]) == 0 {
                return Err(Error::other("节点未加入配置（可能已存在）"));
            }
        }
        self.persist_model()?;
        self.refresh_doctor();
        self.mark_dirty();
        self.toast(
            ToastLevel::Success,
            format!("已添加节点“{}”（{}:{}）", node.name, node.server, node.port),
        );
        Ok(node.id)
    }

    // ------------------------------------------------------------------
    // Nodes and groups
    // ------------------------------------------------------------------

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

    pub fn select_node(&self, group_id: &str, node_id: &str) {
        let group_tag;
        {
            let mut model = self.inner.model.write();
            if !model.select_node(group_id, node_id) {
                return;
            }
            let Some(group) = model.group(group_id) else {
                return;
            };
            group_tag = group.tag();
        }
        let _ = self.persist_model();
        let node = self.inner.model.read().node(node_id).cloned();
        let Some(node) = node else { return };

        if !self.is_running() {
            return;
        }
        let settings = self.inner.settings.read().clone();
        if settings.core == CoreKind::SingBox {
            // sing-box's selector supports instant switching through Clash API.
            let client = ClashClient::new(settings.api_port, "");
            let tag = node.outbound_tag();
            let service = self.clone();
            self.spawn(async move {
                match client.select(&group_tag, &tag).await {
                    Ok(()) => {
                        service.toast(ToastLevel::Success, format!("已切换到“{}”", node.name))
                    }
                    Err(err) => {
                        // Fall back to a config reapply if the group is not live.
                        service.toast(
                            ToastLevel::Warning,
                            format!("实时切换失败，将重启 core：{err}"),
                        );
                        service.apply_config();
                    }
                }
            });
        } else {
            self.toast(
                ToastLevel::Info,
                "Xray 不支持热切换节点，正在重启 core 以应用新节点",
            );
            self.apply_config();
        }
    }

    pub fn set_node_enabled(&self, node_id: &str, enabled: bool) {
        {
            let mut model = self.inner.model.write();
            let Some(node) = model.node_mut(node_id) else {
                return;
            };
            node.enabled = enabled;
        }
        let _ = self.persist_model();
        self.refresh_doctor();
        self.mark_dirty();
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
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn export_links(&self, node_ids: &[String]) -> String {
        let model = self.inner.model.read();
        node_ids
            .iter()
            .filter_map(|id| model.node(id))
            .filter_map(crate::subscription::node_to_share_link)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn node_qr(&self, node_id: &str) -> Result<String> {
        let node = self
            .inner
            .model
            .read()
            .node(node_id)
            .cloned()
            .ok_or_else(|| Error::other("节点不存在"))?;
        let link = crate::subscription::node_to_share_link(&node)
            .ok_or_else(|| Error::other("该协议暂不支持导出分享链接"))?;
        qr_ascii(&link)
    }

    // ------------------------------------------------------------------
    // Latency
    // ------------------------------------------------------------------

    pub fn test_latency(&self, scope: LatencyScope) {
        if self.is_busy("latency") {
            return;
        }
        let model = self.inner.model.read().clone();
        let node_ids: Vec<String> = match &scope {
            LatencyScope::All => model.nodes.iter().map(|n| n.id.clone()).collect(),
            LatencyScope::Node(id) => vec![id.clone()],
            LatencyScope::Group(id) => model
                .group(id)
                .map(|g| g.members.clone())
                .unwrap_or_default(),
        };
        let targets: Vec<(String, String, u16)> = node_ids
            .iter()
            .filter_map(|id| model.node(id))
            .filter(|n| n.enabled)
            .map(|n| (n.id.clone(), n.server.clone(), n.port))
            .collect();
        if targets.is_empty() {
            self.toast(ToastLevel::Warning, "没有可测试的节点");
            return;
        }
        self.set_busy("latency", true);
        let service = self.clone();
        self.spawn(async move {
            let results =
                crate::runtime::test_many(targets, 8, std::time::Duration::from_secs(4)).await;
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
                        detail: result.error.clone(),
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

    /// Real latency through the running core (requires Clash API / sing-box).
    pub fn test_real_latency(&self, node_id: &str) {
        let settings = self.inner.settings.read().clone();
        if !self.is_running() || settings.core != CoreKind::SingBox {
            self.toast(ToastLevel::Warning, "走代理的真实延迟需要运行中的 sing-box");
            return;
        }
        let Some(node) = self.inner.model.read().node(node_id).cloned() else {
            return;
        };
        let client = ClashClient::new(settings.api_port, "");
        let service = self.clone();
        let key = node.id.clone();
        let url = settings.urltest_url.clone();
        self.spawn(async move {
            let delay = client.delay(&node.outbound_tag(), &url, 5000).await;
            let entry = LatencyEntry {
                key: key.clone(),
                delay_ms: delay,
                checked_at: now_unix(),
                detail: delay.is_none().then(|| "通过代理请求失败".to_string()),
            };
            let _ = service.inner.store.set_latency(&entry);
            service.inner.latency.write().insert(key, entry);
            service.toast(
                if delay.is_some() {
                    ToastLevel::Success
                } else {
                    ToastLevel::Warning
                },
                match delay {
                    Some(ms) => format!("“{}”真实延迟 {ms} ms", node.name),
                    None => format!("“{}”真实延迟测试失败", node.name),
                },
            );
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
    async fn ensure_rule_sets(&self) -> std::result::Result<(), String> {
        if self.inner.settings.read().core != CoreKind::SingBox {
            return Ok(());
        }
        let model = self.inner.model.read().clone();
        let sets = crate::adapters::required_rule_sets(&model);
        let dir = self.inner.paths.rule_sets_dir();
        let missing: Vec<crate::model::RuleSet> = sets
            .into_iter()
            .filter(|set| !dir.join(format!("{}.srs", set.tag)).exists())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        self.toast(
            ToastLevel::Info,
            format!("正在预下载 {} 个规则集…", missing.len()),
        );
        let results = updates::sync_rule_sets(&missing, &dir).await;
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
        *self.inner.rule_set_updates.write() = results;
        let still_missing: Vec<String> = missing
            .iter()
            .filter(|set| !dir.join(format!("{}.srs", set.tag)).exists())
            .map(|set| set.tag.clone())
            .collect();
        if still_missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "规则集下载失败：{}。请检查网络，或在“路由 → 规则集”中使用可访问的镜像。",
                still_missing.join(", ")
            ))
        }
    }

    pub fn start_core(&self) {
        if self.is_running() {
            self.toast(ToastLevel::Info, "core 已在运行");
            return;
        }
        if self.is_busy("core") {
            return;
        }
        self.sync_model_settings();
        let _ = self.persist_model();
        self.refresh_doctor();
        self.set_busy("core", true);
        let service = self.clone();
        self.spawn(async move {
            let settings = service.inner.settings.read().clone();
            let model = service.inner.model.read().clone();
            if let Err(err) = service.ensure_rule_sets().await {
                service.set_busy("core", false);
                service.toast(ToastLevel::Error, err);
                return;
            }
            if let Some(warning) = service.prepare_tun_via_helper().await {
                service.toast(
                    ToastLevel::Warning,
                    format!("提权助手未能准备 TUN：{warning}"),
                );
            }
            let result = service
                .inner
                .core
                .start(&model, &settings, settings.core)
                .await;
            service.set_busy("core", false);
            match result {
                Ok(outcome) => {
                    let started = service.record_outcome(&outcome);
                    if started {
                        service.inner.traffic.lock().reset();
                        service.inner.connections.write().clear();
                        service.inner.proxies.write().clear();
                        service.inner.config_dirty.store(false, Ordering::SeqCst);
                        if settings.system_proxy {
                            service.enable_system_proxy();
                        }
                        service.toast(
                            ToastLevel::Success,
                            format!(
                                "{} 已启动，代理端口 {}",
                                settings.core.display_name(),
                                settings.mixed_port
                            ),
                        );
                    }
                }
                Err(err) => {
                    service.toast(ToastLevel::Error, format!("启动失败：{err}"));
                }
            }
        });
    }

    pub fn stop_core(&self) {
        if !self.is_running() {
            self.toast(ToastLevel::Info, "core 未运行");
            return;
        }
        if self.is_busy("core") {
            return;
        }
        self.set_busy("core", true);
        let service = self.clone();
        self.spawn(async move {
            let result = service.inner.core.stop().await;
            service.set_busy("core", false);
            service.cleanup_tun_via_helper().await;
            match result {
                Ok(()) => {
                    service.inner.traffic.lock().reset();
                    service.inner.connections.write().clear();
                    service.inner.proxies.write().clear();
                    service.toast(ToastLevel::Success, "core 已停止");
                    if service.inner.settings.read().system_proxy {
                        service.disable_system_proxy();
                    }
                }
                Err(err) => service.toast(ToastLevel::Error, format!("停止失败：{err}")),
            }
        });
    }

    pub fn toggle_core(&self) {
        if self.is_running() {
            self.stop_core();
        } else {
            self.start_core();
        }
    }

    /// Stops (if needed) and starts with the current model. This is the
    /// "apply changes" path: neither core supports config hot reload.
    pub fn apply_config(&self) {
        self.apply_config_with_tun_cleanup(false);
    }

    fn apply_config_with_tun_cleanup(&self, cleanup_old_tun: bool) {
        if self.is_busy("core") {
            return;
        }
        self.sync_model_settings();
        let _ = self.persist_model();
        self.refresh_doctor();
        let was_running = self.is_running();
        self.set_busy("core", true);
        let service = self.clone();
        self.spawn(async move {
            let settings = service.inner.settings.read().clone();
            let model = service.inner.model.read().clone();
            if was_running {
                // Prepare dependencies before stopping the working core.
                if let Err(err) = service.ensure_rule_sets().await {
                    service.set_busy("core", false);
                    service.toast(ToastLevel::Error, err);
                    return;
                }
                if let Err(err) = service.inner.core.stop().await {
                    service.set_busy("core", false);
                    service.toast(ToastLevel::Error, format!("重启前停止失败：{err}"));
                    return;
                }
                service.inner.traffic.lock().reset();
                service.inner.connections.write().clear();
            }
            if was_running {
                if cleanup_old_tun {
                    service.cleanup_tun_interface_via_helper().await;
                }
                if let Some(warning) = service.prepare_tun_via_helper().await {
                    service.toast(
                        ToastLevel::Warning,
                        format!("提权助手未能准备 TUN：{warning}"),
                    );
                }
            }
            let result = if was_running {
                service
                    .inner
                    .core
                    .start(&model, &settings, settings.core)
                    .await
            } else {
                service
                    .inner
                    .core
                    .prepare(&model, &settings, settings.core)
                    .await
            };
            service.set_busy("core", false);
            match result {
                Ok(outcome) => {
                    let ok = service.record_outcome(&outcome);
                    if was_running && ok {
                        service.inner.config_dirty.store(false, Ordering::SeqCst);
                        if settings.system_proxy {
                            service.enable_system_proxy();
                        }
                        service.toast(ToastLevel::Success, "配置已应用，core 已重启");
                    } else if !was_running && ok {
                        service.toast(ToastLevel::Success, "配置校验通过（core 未运行）");
                    }
                }
                Err(err) => service.toast(ToastLevel::Error, format!("应用配置失败：{err}")),
            }
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
        new.mixed_port = new.mixed_port.max(1);
        new.api_port = new.api_port.max(1);
        new.stats_port = new.stats_port.max(1);
        if new.api_port == new.mixed_port || new.stats_port == new.mixed_port {
            self.toast(ToastLevel::Warning, "端口不能重复，已保留旧端口");
            new.mixed_port = old.mixed_port;
        }
        if let Err(err) = self.save_settings(&new) {
            self.toast(ToastLevel::Error, format!("保存设置失败：{err}"));
            return;
        }
        self.sync_model_settings();
        let _ = self.persist_model();
        if new.autostart != old.autostart {
            if let Err(err) = system::autostart::set_enabled(new.autostart) {
                self.toast(ToastLevel::Warning, format!("设置开机自启失败：{err}"));
            }
        }
        if new.core != old.core && self.is_running() {
            self.toast(
                ToastLevel::Warning,
                format!("已切换到 {}，请先停止再启动 core", new.core.display_name()),
            );
        }
        if new.system_proxy != old.system_proxy {
            self.set_system_proxy(new.system_proxy);
        }
        if new.tun_enabled && !new.core.capabilities().native_tun {
            self.toast(
                ToastLevel::Warning,
                format!("{} 不支持 TUN，请切换到 sing-box", new.core.display_name()),
            );
        }
        self.refresh_doctor();
        self.mark_dirty();
    }

    pub fn set_language(&self, language: crate::settings::Language) {
        let mut settings = self.settings();
        settings.language = language;
        let _ = self.save_settings(&settings);
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
            self.toast(
                ToastLevel::Warning,
                "当前 Core 不支持 TUN，请先切换到 sing-box",
            );
            return;
        }
        if self.is_busy("core") {
            self.toast(ToastLevel::Info, "Core 正在处理，请稍后再切换 TUN");
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
            self.toast(ToastLevel::Info, "正在重启 Core 以应用 TUN 设置");
            self.apply_config_with_tun_cleanup(old_tun);
        } else {
            self.toast(ToastLevel::Info, "TUN 设置已保存，下次启动 Core 时生效");
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
            self.toast(ToastLevel::Info, "Core 正在处理，请稍后再切换路由");
            return;
        }
        if self.model().routing.preset == preset {
            return;
        }
        let running = self.is_running();
        self.set_route_preset(preset);
        if running {
            self.toast(ToastLevel::Info, "正在重启 Core 以应用路由设置");
            self.apply_config();
        } else {
            self.toast(ToastLevel::Info, "路由设置已保存，下次启动 Core 时生效");
        }
    }

    pub fn upsert_rule(&self, rule: RouteRule) {
        {
            let mut model = self.inner.model.write();
            if let Some(existing) = model.routing.rules.iter_mut().find(|r| r.id == rule.id) {
                *existing = rule;
            } else {
                model.routing.rules.push(rule);
            }
            model.routing.preset = RoutePreset::Custom;
        }
        let _ = self.persist_model();
        self.mark_dirty();
        self.toast(ToastLevel::Info, "规则已保存，应用配置后生效");
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
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_rule_enabled(&self, rule_id: &str, enabled: bool) {
        {
            let mut model = self.inner.model.write();
            if let Some(rule) = model.routing.rules.iter_mut().find(|r| r.id == rule_id) {
                rule.enabled = enabled;
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
    }

    pub fn set_routing_flags(
        &self,
        bypass_lan: bool,
        bypass_china_ips: bool,
        strategy: crate::model::DomainStrategy,
    ) {
        {
            let mut model = self.inner.model.write();
            model.routing.bypass_lan = bypass_lan;
            model.routing.bypass_china_ips = bypass_china_ips;
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

    pub fn delete_dns_server(&self, tag: &str) {
        {
            let mut model = self.inner.model.write();
            model.dns.servers.retain(|s| s.tag != tag);
            if model.dns.final_server.as_deref() == Some(tag) {
                model.dns.final_server = model.dns.servers.first().map(|s| s.tag.clone());
            }
        }
        let _ = self.persist_model();
        self.mark_dirty();
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

    pub fn set_system_proxy(&self, enabled: bool) {
        if enabled {
            self.enable_system_proxy();
        } else {
            self.disable_system_proxy();
        }
    }

    fn enable_system_proxy(&self) {
        if self.is_busy("proxy") {
            return;
        }
        self.set_busy("proxy", true);
        let service = self.clone();
        self.spawn(async move {
            let settings = service.inner.settings.read().clone();
            let compiled_http_port = service
                .inner
                .compiled
                .read()
                .as_ref()
                .map(|c| c.http_port())
                .unwrap_or(settings.mixed_port);
            let pac_url = if settings.pac_enabled {
                let model = service.inner.model.read().clone();
                let mut direct_domains = Vec::new();
                let mut direct_cidrs = vec![
                    "10.0.0.0/8".to_string(),
                    "172.16.0.0/12".to_string(),
                    "192.168.0.0/16".to_string(),
                ];
                for rule in model.routing.rules.iter().filter(|r| r.enabled) {
                    if rule.action.kind == crate::model::RouteActionKind::Direct {
                        direct_domains.extend(rule.matchers.domains.iter().cloned());
                        direct_domains.extend(rule.matchers.domain_suffixes.iter().cloned());
                        direct_cidrs.extend(rule.matchers.ip_cidrs.iter().cloned());
                    }
                }
                let pac = system::build_pac(
                    "127.0.0.1",
                    compiled_http_port,
                    &direct_domains,
                    &direct_cidrs,
                );
                match PacServer::start(pac, settings.pac_port) {
                    Ok(server) => {
                        server.update_mesh(&service.inner.core.mesh_plan());
                        let url = server.info.url.clone();
                        *service.inner.pac.lock() = Some(server);
                        Some(url)
                    }
                    Err(err) => {
                        service.toast(ToastLevel::Error, format!("PAC 服务启动失败：{err}"));
                        None
                    }
                }
            } else {
                None
            };
            let result = service.inner.proxy.lock().apply(
                "127.0.0.1",
                compiled_http_port,
                pac_url.as_deref(),
            );
            service.set_busy("proxy", false);
            match result {
                Ok(_) => {
                    let mut settings = service.inner.settings.read().clone();
                    settings.system_proxy = true;
                    let _ = service.save_settings(&settings);
                    service.toast(
                        ToastLevel::Success,
                        format!("系统代理已开启（127.0.0.1:{compiled_http_port}）"),
                    );
                }
                Err(err) => {
                    *service.inner.pac.lock() = None;
                    let mut settings = service.inner.settings.read().clone();
                    settings.system_proxy = false;
                    let _ = service.save_settings(&settings);
                    service.toast(ToastLevel::Error, format!("设置系统代理失败：{err}"));
                }
            }
        });
    }

    fn disable_system_proxy(&self) {
        if self.is_busy("proxy") {
            return;
        }
        self.set_busy("proxy", true);
        let service = self.clone();
        self.spawn(async move {
            let result = service.inner.proxy.lock().restore();
            *service.inner.pac.lock() = None;
            let mut settings = service.inner.settings.read().clone();
            settings.system_proxy = false;
            let _ = service.save_settings(&settings);
            service.set_busy("proxy", false);
            match result {
                Ok(()) => service.toast(ToastLevel::Info, "系统代理已关闭并还原"),
                Err(err) => service.toast(ToastLevel::Warning, format!("还原系统代理失败：{err}")),
            }
        });
    }

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
        let service = self.clone();
        self.spawn(async move {
            let status = tokio::task::spawn_blocking(privilege::status).await;
            if let Ok(status) = status {
                *service.inner.helper.write() = status;
            }
        });
    }

    pub fn install_helper(&self) {
        self.run_helper_action("安装提权助手", privilege::install);
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
        if self.is_busy("helper") {
            return;
        }
        self.set_busy("helper", true);
        let service = self.clone();
        self.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                privilege::invalidate_status();
                let outcome = action();
                // Refresh before returning so the settings card updates
                // immediately instead of waiting for the next monitor tick.
                (outcome, privilege::status())
            })
            .await;
            service.set_busy("helper", false);
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
        });
    }

    /// TUN interface name from the model, used for helper requests.
    fn tun_interface(&self) -> String {
        self.inner
            .model
            .read()
            .inbounds
            .iter()
            .find(|inbound| inbound.kind == InboundKind::Tun && inbound.enabled)
            .and_then(|inbound| inbound.tun.interface_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "xross-tun".to_string())
    }

    /// Asks the helper to prepare privileges for TUN. Returns a warning only
    /// when the helper is running but the request failed; a missing helper is
    /// not an error because the core still creates its own device.
    async fn prepare_tun_via_helper(&self) -> Option<String> {
        if !self.inner.settings.read().effective_tun() || !privilege::status().running {
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
        if !self.inner.settings.read().effective_tun() {
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
        let model = self.inner.model.read().clone();
        let settings = self.inner.settings.read().clone();
        let state = self.inner.core.state();
        let pid = state.phase.is_alive().then_some(state.pid).flatten();
        let generation = self.inner.doctor_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.set_busy("doctor", true);
        let service = self.clone();
        self.spawn(async move {
            let checks = tokio::task::spawn_blocking(move || {
                system::tun::diagnose_for_process(&model, &settings, pid)
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

    pub fn set_active_version(&self, core: CoreKind, version: &str) {
        let mut settings = self.inner.settings.read().clone();
        settings
            .active_versions
            .insert(core.as_str().into(), version.to_string());
        settings.binary_paths.remove(core.as_str());
        let _ = self.save_settings(&settings);
        self.toast(
            ToastLevel::Info,
            format!("{} 将使用版本 {}", core.display_name(), version),
        );
    }

    pub fn set_binary_override(&self, core: CoreKind, path: Option<String>) {
        let mut settings = self.inner.settings.read().clone();
        match path.filter(|p| !p.trim().is_empty()) {
            Some(path) => {
                settings.binary_paths.insert(core.as_str().into(), path);
            }
            None => {
                settings.binary_paths.remove(core.as_str());
            }
        }
        let _ = self.save_settings(&settings);
    }

    pub fn check_updates(&self) {
        if self.is_busy("updates") {
            return;
        }
        self.set_busy("updates", true);
        let service = self.clone();
        self.spawn(async move {
            let settings = service.inner.settings.read().clone();
            let current = service
                .inner
                .core
                .binaries()
                .resolve(
                    settings.core,
                    settings.active_version(settings.core).map(|s| s.as_str()),
                )
                .map(|b| b.version);
            let core_result = updates::check_core_update(settings.core, current.as_deref()).await;
            let app_result = updates::check_app_update(APP_VERSION).await;
            let mut status = UpdateStatus {
                checked_at: Some(now_unix()),
                ..Default::default()
            };
            match core_result {
                Ok(candidate) => status.core = candidate,
                Err(err) => status.error = Some(err.to_string()),
            }
            match app_result {
                Ok(update) => status.app = update,
                Err(err) => {
                    status.error = Some(match status.error.take() {
                        Some(existing) => format!("{existing}；{err}"),
                        None => err.to_string(),
                    });
                }
            }
            *service.inner.updates.lock() = status.clone();
            service.set_busy("updates", false);
            let core_label = status
                .core
                .as_ref()
                .map(|u| format!("{} {}", u.core.display_name(), u.version));
            let app_label = status.app.as_ref().map(|a| format!("App {}", a.version));
            if core_label.is_none() && app_label.is_none() {
                service.toast(ToastLevel::Info, "已是最新版本");
            } else {
                service.toast(
                    ToastLevel::Info,
                    format!(
                        "发现更新：{}",
                        [core_label, app_label]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join("，")
                    ),
                );
            }
            if let Some(error) = &status.error {
                if status.core.is_none() {
                    service.toast(ToastLevel::Warning, format!("检查更新失败：{error}"));
                }
            }
        });
    }

    pub fn install_core_update(&self, core: CoreKind) {
        if self.is_busy("install") {
            return;
        }
        self.set_busy("install", true);
        let service = self.clone();
        self.spawn(async move {
            let current = service
                .inner
                .core
                .binaries()
                .resolve(
                    core,
                    service
                        .inner
                        .settings
                        .read()
                        .active_version(core)
                        .map(|s| s.as_str()),
                )
                .map(|b| b.version);
            let candidate = match updates::check_core_update(core, current.as_deref()).await {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
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
                Some(url) => updates::download_checksum(url).await.ok().flatten(),
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
                .install_from_url(core, &candidate.version, &candidate.asset.url, checksum)
                .await;
            service.set_busy("install", false);
            match result {
                Ok(record) => {
                    let mut settings = service.inner.settings.read().clone();
                    settings
                        .active_versions
                        .insert(core.as_str().into(), record.version.clone());
                    settings.binary_paths.remove(core.as_str());
                    let _ = service.save_settings(&settings);
                    service.toast(
                        ToastLevel::Success,
                        format!("{} {} 安装完成", core.display_name(), record.version),
                    );
                }
                Err(err) => service.toast(ToastLevel::Error, format!("安装失败：{err}")),
            }
        });
    }

    pub fn sync_rule_sets(&self) {
        if self.is_busy("rulesets") {
            return;
        }
        self.set_busy("rulesets", true);
        let service = self.clone();
        self.spawn(async move {
            let model = service.inner.model.read().clone();
            let dir = service.inner.paths.rule_sets_dir();
            let results =
                updates::sync_rule_sets(&crate::adapters::required_rule_sets(&model), &dir).await;
            let updated = results.iter().filter(|r| r.status == "updated").count();
            let failed = results.iter().filter(|r| r.status == "failed").count();
            let unchanged = results
                .iter()
                .filter(|r| r.status == "not-modified")
                .count();
            *service.inner.rule_set_updates.write() = results;
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

    pub fn clear_logs(&self) {
        self.inner.core.logs().clear();
        self.toast(ToastLevel::Info, "日志已清空（磁盘日志仍保留）");
    }

    /// Exports the in-memory log buffer to `logs/exports/`.
    pub fn export_logs(&self) -> Result<String> {
        let dir = self.inner.paths.logs_dir().join("exports");
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::other(format!("创建日志目录失败：{e}")))?;
        let path = dir.join(format!(
            "xross-log-{}.log",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        self.inner.core.logs().export_to(&path)?;
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
        recording.observe(&connections, now);
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

    fn update_connections(&self, connections: Vec<ClashConnection>) {
        let mut current = self.inner.connections.write();
        self.inner
            .recording
            .lock()
            .observe(&connections, now_unix());
        *current = connections;
    }

    /// Insert an exact diagnostic override before broader preset rules.
    pub fn add_diagnostic_rule(
        &self,
        target: &DiagnosticTarget,
        kind: DiagnosticRuleKind,
        action: crate::model::RouteAction,
    ) -> Result<()> {
        let matchers = target.matchers(kind)?;
        let mut model = self.inner.model.write();
        if let Some(group) = &action.group {
            if model.group(group).is_none() {
                return Err(Error::config("所选策略组已不存在，请重新选择"));
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
        next.routing.preset = RoutePreset::Custom;
        next.updated_at = now_unix();
        let profile = self.inner.store.save_active_profile(&next)?;
        *self.inner.active_profile.write() = Some(profile);
        *model = next;
        self.mark_dirty();
        self.toast(
            ToastLevel::Success,
            "规则已置顶保存，点击应用配置后重新测试",
        );
        Ok(())
    }

    pub fn refresh_connections(&self) {
        let settings = self.inner.settings.read().clone();
        if !self.is_running() || settings.core != CoreKind::SingBox {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let client = ClashClient::new(settings.api_port, "");
            match client.connections().await {
                Ok(response) => {
                    service
                        .inner
                        .traffic
                        .lock()
                        .sample(response.upload_total, response.download_total);
                    service.update_connections(response.connections);
                }
                Err(err) => service.inner.core.logs().push(
                    LogStream::App,
                    LogLevel::Warn,
                    format!("读取连接列表失败：{err}"),
                ),
            }
        });
    }

    pub fn kill_connection(&self, id: &str) {
        let settings = self.inner.settings.read().clone();
        if settings.core != CoreKind::SingBox {
            self.toast(ToastLevel::Warning, "当前 core 不支持断开单个连接");
            return;
        }
        let id = id.to_string();
        let service = self.clone();
        self.spawn(async move {
            let client = ClashClient::new(settings.api_port, "");
            match client.close_connection(&id).await {
                Ok(()) => {
                    service.toast(ToastLevel::Info, "连接已断开");
                    service.inner.connections.write().retain(|c| c.id != id);
                    let connections = service.inner.connections.read().clone();
                    service
                        .inner
                        .recording
                        .lock()
                        .observe(&connections, now_unix());
                }
                Err(err) => service.toast(ToastLevel::Warning, format!("断开连接失败：{err}")),
            }
        });
    }

    pub fn kill_all_connections(&self) {
        let settings = self.inner.settings.read().clone();
        if settings.core != CoreKind::SingBox {
            self.toast(ToastLevel::Warning, "当前 core 不支持断开连接");
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let client = ClashClient::new(settings.api_port, "");
            match client.close_all_connections().await {
                Ok(()) => {
                    service.update_connections(Vec::new());
                    service.toast(ToastLevel::Info, "所有连接已断开");
                }
                Err(err) => service.toast(ToastLevel::Warning, format!("断开连接失败：{err}")),
            }
        });
    }

    pub fn save_mesh_profile(&self, profile: crate::mesh::MeshProfile) -> Result<String> {
        let id = self.inner.mesh.save(profile)?;
        self.toast(ToastLevel::Success, "组网配置已加密保存");
        Ok(id)
    }

    pub fn delete_mesh_profile(&self, id: &str) -> Result<()> {
        self.inner.mesh.delete(id)
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
        if self.is_busy("mesh") {
            return;
        }
        self.set_busy("mesh", true);
        let service = self.clone();
        self.spawn(async move {
            let _operation = service.inner.mesh.operation.lock().await;
            let result: Result<()> = async {
                match action {
                    "connect" => {
                        if !cfg!(windows) {
                            return Err(Error::other("异地组网当前仅在 Windows 启用"));
                        }
                        let mut preflight = service.inner.core.mesh_plan();
                        preflight.active = true;
                        service.inner.core.apply_mesh_plan(preflight).await?;
                        if let Err(error) = service.inner.mesh.connect(&value).await {
                            let _ = service.reconcile_mesh().await;
                            return Err(error);
                        }
                    }
                    "disconnect" => service.inner.mesh.disconnect(provider.unwrap()).await?,
                    "logout" => service.inner.mesh.logout(provider.unwrap()).await?,
                    "probe" => {
                        service
                            .inner
                            .mesh
                            .probe(provider.unwrap(), value.clone())
                            .await?
                    }
                    "install" => service.inner.mesh.install().await?,
                    _ => unreachable!(),
                }
                service.reconcile_mesh().await
            }
            .await;
            service.set_busy("mesh", false);
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
                                    "Tailscale 正在等待登录或管理员批准"
                                },
                            ),
                        _ => service.toast(ToastLevel::Success, "组网已连接"),
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
                Err(error) => service.toast(ToastLevel::Error, error.to_string()),
            }
        });
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
                    self.inner.mesh.disconnect(state.provider).await?;
                }
                return Err(error);
            }
        };
        if let Err(error) = self.inner.core.apply_mesh_plan(plan.clone()).await {
            // Keep the proxy's verified configuration, and remove the new mesh
            // connection rather than leaving traffic captured by the proxy TUN.
            for state in states.iter().filter(|s| {
                matches!(
                    s.phase,
                    crate::mesh::MeshPhase::Starting | crate::mesh::MeshPhase::Connected
                )
            }) {
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
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                let Some(inner) = weak.upgrade() else { break };
                let service = AppService {
                    inner: inner.clone(),
                };
                // Probe the privilege helper off the UI thread. The status is
                // cached inside `privilege::status`, so this is cheap unless
                // the cache expired.
                if tick % 3 == 0 {
                    if let Ok(status) = tokio::task::spawn_blocking(privilege::status).await {
                        *inner.helper.write() = status;
                    }
                }
                if tick % 2 == 0 && !service.is_busy("core") {
                    if let Ok(_guard) = inner.mesh.operation.try_lock() {
                        if let Err(error) = inner.mesh.refresh().await {
                            service.toast(ToastLevel::Warning, error.to_string());
                        }
                        if let Err(error) = service.reconcile_mesh().await {
                            service.toast(ToastLevel::Error, error.to_string());
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
                if state.phase != last_phase {
                    service.refresh_doctor();
                }
                last_phase = state.phase;
                if !state.phase.is_alive() {
                    service.update_connections(Vec::new());
                    tick += 1;
                    continue;
                }
                let settings = service.inner.settings.read().clone();
                if settings.core == CoreKind::SingBox {
                    let client = ClashClient::new(settings.api_port, "");
                    if let Ok(response) = client.connections().await {
                        inner
                            .traffic
                            .lock()
                            .sample(response.upload_total, response.download_total);
                        service.update_connections(response.connections);
                    }
                    if tick % 5 == 0 {
                        if let Ok(proxies) = client.proxies().await {
                            *inner.proxies.write() = proxies.proxies;
                        }
                    }
                } else if let Ok(binary) = inner.core.resolve_binary(CoreKind::Xray, &settings) {
                    let client = XrayStatsClient::new(binary, settings.stats_port);
                    if let Ok(traffic) = client.query().await {
                        inner
                            .traffic
                            .lock()
                            .sample(traffic.uplink, traffic.downlink);
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

    /// Best-effort cleanup called by the UI on exit.
    pub fn shutdown(&self) {
        self.inner.mesh.shutdown();
        let _ = self.persist_model();
        if self.inner.settings.read().system_proxy {
            let _ = self.inner.proxy.lock().restore();
            let mut settings = self.inner.settings.read().clone();
            settings.system_proxy = false;
            let _ = settings.save(&self.inner.paths.settings_file());
        }
        *self.inner.pac.lock() = None;
        let core = self.inner.core.clone();
        let _ = self.inner.rt.block_on(async move {
            let _ = core.stop().await;
        });
        if self.inner.settings.read().effective_tun() && self.inner.helper.read().running {
            let request = HelperRequest::RemoveTun {
                interface: self.tun_interface(),
            };
            let _ = privilege::call_with_timeout(&request, std::time::Duration::from_secs(1));
        }
    }

    /// Re-applies the system proxy with the current PAC/port settings.
    /// Disabling and re-enabling is sequenced on the runtime so the two
    /// command sequences can never interleave.
    pub fn reapply_system_proxy(&self) {
        if !self.inner.settings.read().system_proxy || self.is_busy("proxy") {
            return;
        }
        self.disable_system_proxy();
        let service = self.clone();
        self.spawn(async move {
            for _ in 0..100 {
                if !service.is_busy("proxy") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            service.enable_system_proxy();
        });
    }

    pub fn export_backup(&self) -> Result<String> {
        let dir = self.inner.paths.backup_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::other(format!("创建备份目录失败：{e}")))?;
        let path = dir.join(format!(
            "xross-export-{}.json",
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
            if std::env::var_os("XROSS_DEBUG_TRACE").is_some() {
                eprintln!(
                    "[trace] publish {:?} success={:?} summary={:?}",
                    session.phase, session.success, session.summary
                );
            }
            *guard = Some(session.clone());
            self.inner.recording.lock().observe_trace(session);
        } else if std::env::var_os("XROSS_DEBUG_TRACE").is_some() {
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
            #[cfg(target_os = "linux")]
            let program = "xdg-open";
            #[cfg(target_os = "macos")]
            let program = "open";
            #[cfg(target_os = "windows")]
            {
                // Authentication URLs can contain shell metacharacters. Send
                // the URL directly to Windows instead of passing it through cmd.
                use std::os::windows::ffi::OsStrExt;
                let target: Vec<u16> = std::ffi::OsStr::new(&url)
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
                unsafe {
                    windows_sys::Win32::UI::Shell::ShellExecuteW(
                        std::ptr::null_mut(),
                        verb.as_ptr(),
                        target.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        1,
                    );
                }
            }
            #[cfg(not(target_os = "windows"))]
            let args = vec![url];
            #[cfg(not(target_os = "windows"))]
            let _ = tokio::process::Command::new(program).args(args).spawn();
        });
    }

    pub fn platform_proxy_backend(&self) -> String {
        self.inner.proxy.lock().backend().label().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatencyScope {
    All,
    Node(String),
    Group(String),
}

fn pretty_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "订阅".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;

    #[test]
    fn mesh_reconciliation_preserves_pending_proxy_edits_and_profile() {
        let (service, _dir) = service();
        service.import_content("vless://11111111-2222-3333-4444-555555555555@pending.example.com:443?security=tls#pending");
        // Represent edits made while a real core was running, without starting one.
        service.inner.config_dirty.store(true, Ordering::SeqCst);
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
        assert!(service
            .add_diagnostic_rule(&target, DiagnosticRuleKind::Process, RouteAction::direct())
            .is_err());
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
        assert!(model
            .groups
            .iter()
            .any(|g| g.name == "我的组" && g.members.len() == 1));
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
    fn tun_toggle_persists_and_updates_model_without_starting_a_stopped_core() {
        let (service, _dir) = service();
        service.set_tun_enabled(true);
        assert!(service.settings().effective_tun());
        assert!(service
            .model()
            .inbounds
            .iter()
            .any(|inbound| inbound.kind == InboundKind::Tun && inbound.enabled));
        assert!(
            Settings::load(&service.paths().settings_file())
                .unwrap()
                .tun_enabled
        );
        assert!(!service.is_running());
        assert!(!service.is_busy("core"));
        service.set_tun_enabled(false);
        assert!(!service.settings().tun_enabled);
        assert!(service
            .model()
            .inbounds
            .iter()
            .filter(|inbound| inbound.kind == InboundKind::Tun)
            .all(|inbound| !inbound.enabled));
    }

    #[test]
    fn tun_toggle_rejects_unsupported_cores_and_busy_restarts() {
        let (service, _dir) = service();
        service.set_core(CoreKind::Xray);
        service.set_tun_enabled(true);
        assert!(!service.settings().tun_enabled);
        assert!(service
            .snapshot()
            .toasts
            .iter()
            .any(|toast| toast.message.contains("不支持 TUN")));
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
    fn route_preset_replaces_rules() {
        let (service, _dir) = service();
        service.set_route_preset(RoutePreset::Global);
        assert!(service.model().routing.rules.is_empty());
        service.set_route_preset(RoutePreset::BypassMainland);
        assert!(!service.model().routing.rules.is_empty());
    }

    #[test]
    fn quick_route_switch_persists_without_starting_a_stopped_core() {
        let (service, _dir) = service();
        service.switch_route_preset(RoutePreset::Global);
        assert_eq!(service.model().routing.preset, RoutePreset::Global);
        assert!(service.model().routing.rules.is_empty());
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
        let added = service.merge_parsed(&subscription.id, &parsed);
        assert_eq!(added, 1);
        assert_eq!(
            service
                .model()
                .nodes_of_subscription(&subscription.id)
                .len(),
            1
        );
    }
}
