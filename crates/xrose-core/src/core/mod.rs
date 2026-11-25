//! Binary store, process supervision, config validation and lifecycle.

pub mod binary;
#[cfg(windows)]
pub mod job;
pub mod logs;
pub mod process;

use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use crate::adapters::{self, CompiledConfig, ValidationReport};
use crate::error::{Error, Result};
use crate::model::{AppModel, CoreKind};
use crate::paths::AppPaths;
use crate::settings::Settings;
use crate::util::atomic_write_private;

pub use binary::{BinaryManager, BinaryRecord};
pub use logs::{LogBus, LogEntry, LogStream};
pub use process::{
    ExitInfo, ProcessPhase, ProcessState, ProcessSupervisor, SpawnSpec, WatchdogConfig,
    human_failure_reason,
};

#[derive(Debug, Clone)]
pub struct StartOutcome {
    pub core: CoreKind,
    pub config_path: PathBuf,
    pub report: ValidationReport,
    pub compiled: CompiledConfig,
    pub started: bool,
}

/// Owns everything about the running core. UI-facing calls are limited to
/// start/stop/restart/validate/apply and state snapshots.
pub struct CoreManager {
    operation: tokio::sync::Mutex<()>,
    mesh: Mutex<crate::mesh::coexist::CoexistencePlan>,
    mesh_exit: Mutex<Option<adapters::MeshExitRuntime>>,
    exit_bridge: Mutex<Option<crate::mesh::ExitBridge>>,
    applied: Mutex<Option<AppliedConfig>>,
    paths: AppPaths,
    binaries: BinaryManager,
    logs: LogBus,
    supervisor: ProcessSupervisor,
    last_validation: Mutex<Option<ValidationReport>>,
    last_compiled: Mutex<Option<CompiledConfig>>,
    last_config_path: Mutex<Option<PathBuf>>,
    last_core: Mutex<Option<CoreKind>>,
}

#[derive(Clone)]
struct AppliedConfig {
    model: AppModel,
    settings: Settings,
    core: CoreKind,
    compiled: CompiledConfig,
    path: PathBuf,
}

impl CoreManager {
    pub fn new(paths: AppPaths, logs: LogBus) -> Self {
        let binaries = BinaryManager::new(&paths);
        let supervisor = ProcessSupervisor::new(logs.clone());
        Self {
            operation: tokio::sync::Mutex::new(()),
            mesh: Mutex::new(Default::default()),
            mesh_exit: Mutex::new(None),
            exit_bridge: Mutex::new(None),
            applied: Mutex::new(None),
            paths,
            binaries,
            logs,
            supervisor,
            last_validation: Mutex::new(None),
            last_compiled: Mutex::new(None),
            last_config_path: Mutex::new(None),
            last_core: Mutex::new(None),
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn logs(&self) -> &LogBus {
        &self.logs
    }

    pub fn binaries(&self) -> &BinaryManager {
        &self.binaries
    }

    pub fn state(&self) -> ProcessState {
        self.supervisor.state()
    }

    pub fn last_validation(&self) -> Option<ValidationReport> {
        self.last_validation.lock().clone()
    }

    /// Checks the applied configuration; validation may have compiled pending edits.
    pub fn native_tun_active(&self) -> bool {
        self.state().phase.is_alive()
            && self.applied.lock().as_ref().is_some_and(|applied| {
                applied
                    .compiled
                    .inbounds
                    .iter()
                    .any(|inbound| inbound.kind == "tun")
            })
    }

    pub(crate) fn active_proxy(&self, protocol: &str) -> Option<crate::updates::LocalProxy> {
        if self.state().phase != ProcessPhase::Running {
            return None;
        }
        let guard = self.applied.lock();
        let applied = guard.as_ref()?;
        let inbound = applied
            .compiled
            .inbounds
            .iter()
            .find(|i| i.kind == "mixed" || i.kind == protocol)?;
        let host = match inbound.listen.as_str() {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "::1",
            host => host,
        };
        Some(crate::updates::LocalProxy {
            host: host.into(),
            port: inbound.port,
            account: (inbound.allow_lan && applied.settings.lan_auth)
                .then(|| applied.settings.lan_account.clone()),
        })
    }

    pub(crate) fn update_network(
        &self,
        settings: &Settings,
        use_proxy: bool,
    ) -> crate::updates::UpdateNetwork {
        crate::updates::UpdateNetwork {
            use_proxy,
            socks5: settings.update_proxy.clone(),
            proxy: if use_proxy && settings.update_proxy.as_str().is_empty() {
                self.active_proxy("http")
            } else {
                None
            },
            user_agent: settings.update_user_agent.clone(),
        }
    }

    /// Listen addresses of proxy inbounds in the currently running configuration.
    pub fn active_proxy_addresses(&self) -> Vec<String> {
        if !self.state().phase.is_alive() {
            return Vec::new();
        }
        let guard = self.applied.lock();
        let Some(applied) = guard.as_ref() else {
            return Vec::new();
        };
        let proxy_inbounds: Vec<_> = applied
            .compiled
            .inbounds
            .iter()
            .filter(|inbound| inbound.kind != "tun" && inbound.port != 0)
            .collect();
        let single_mixed = proxy_inbounds.len() == 1 && proxy_inbounds[0].kind == "mixed";
        proxy_inbounds
            .into_iter()
            .map(|inbound| {
                let listen = &inbound.listen;
                if single_mixed {
                    format!("{listen}:{}", inbound.port)
                } else {
                    format!(
                        "{} {listen}:{}",
                        inbound.kind.to_ascii_uppercase(),
                        inbound.port
                    )
                }
            })
            .collect()
    }

    /// Active TUN interface name when TUN is running in the current core.
    pub fn active_tun_interface(&self) -> Option<String> {
        if !self.native_tun_active() {
            return None;
        }
        for entry in self
            .logs
            .recent(200, crate::settings::LogLevel::Trace, "")
            .into_iter()
            .rev()
        {
            if entry.stream == LogStream::App && entry.message.starts_with("启动 ") {
                break;
            }
            if let Some((_, rest)) = entry.message.split_once("inbound/tun[") {
                if let Some((_, iface)) = rest.split_once("]: started at ") {
                    let iface = iface.trim();
                    if !iface.is_empty() {
                        return Some(iface.to_string());
                    }
                }
            }
        }
        let configured = self.applied.lock().as_ref().and_then(|applied| {
            applied
                .model
                .inbounds
                .iter()
                .find(|inbound| inbound.enabled && inbound.kind == crate::model::InboundKind::Tun)
                .and_then(|inbound| inbound.tun.interface_name.clone())
                .filter(|name| !name.trim().is_empty())
        });
        Some(configured.unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "utun".to_string()
            } else {
                "tun0".to_string()
            }
        }))
    }

    pub fn last_compiled(&self) -> Option<CompiledConfig> {
        self.last_compiled.lock().clone()
    }

    pub fn last_config_path(&self) -> Option<PathBuf> {
        self.last_config_path.lock().clone()
    }

    /// Resolves the binary, honoring an explicit path override.
    pub fn resolve_binary(&self, core: CoreKind, settings: &Settings) -> Result<PathBuf> {
        if let Some(custom) = settings.binary_override(core) {
            let path = PathBuf::from(custom);
            if binary::is_executable(&path) {
                return std::path::absolute(&path)
                    .map_err(|e| Error::binary(format!("无法解析自定义内核路径：{e}")));
            }
            return Err(Error::binary(format!(
                "自定义 {} 路径不存在或不可执行：{}",
                core.display_name(),
                path.display()
            )));
        }
        if settings.active_version(core).is_none() {
            return binary::path_binary(core).ok_or_else(|| Error::binary(format!(
                "系统 PATH 中未找到 {} 可执行文件，请在设置中选择已安装版本、指定路径或下载内核。", core.binary_name()
            )));
        }
        let record = self
            .binaries
            .resolve(core, settings.active_version(core).map(|s| s.as_str()))
            .ok_or_else(|| {
                Error::binary(format!(
                    "尚未安装 {}。请在设置 → Core 中下载，或指定可执行文件路径。",
                    core.display_name()
                ))
            })?;
        let path = record.absolute_path(&self.binaries);
        if !path.exists() {
            return Err(Error::binary(format!(
                "{} 的文件已丢失：{}",
                core.display_name(),
                path.display()
            )));
        }
        Ok(path)
    }

    fn resolve_and_register_binary(&self, core: CoreKind, settings: &Settings) -> Result<PathBuf> {
        let binary = self.resolve_binary(core, settings)?;
        if settings.active_version(core).is_none() {
            self.binaries.register_local(core, &binary)?;
        }
        Ok(binary)
    }

    pub fn set_watchdog(&self, config: WatchdogConfig) {
        self.supervisor.set_watchdog(config);
        if let Some(applied) = self.applied.lock().as_mut() {
            applied.settings.watchdog = config.enabled;
            applied.settings.watchdog_max_retries = config.max_retries;
        }
    }

    /// Compiles the model, writes the config to the runtime directory and runs
    /// the core's own `-test`/`check` before returning.
    pub async fn prepare(
        &self,
        model: &AppModel,
        settings: &Settings,
        core: CoreKind,
    ) -> Result<StartOutcome> {
        let _guard = self.operation.lock().await;
        self.prepare_locked(model, settings, core).await
    }

    async fn prepare_locked(
        &self,
        model: &AppModel,
        settings: &Settings,
        core: CoreKind,
    ) -> Result<StartOutcome> {
        let path = self.paths.generated_config(core.as_str());
        let outcome = self.prepare_at(model, settings, core, path).await?;
        self.remember_config(&outcome);
        Ok(outcome)
    }

    fn remember_config(&self, outcome: &StartOutcome) {
        *self.last_compiled.lock() = Some(outcome.compiled.clone());
        *self.last_config_path.lock() = Some(outcome.config_path.clone());
        *self.last_core.lock() = Some(outcome.core);
    }

    async fn prepare_at(
        &self,
        model: &AppModel,
        settings: &Settings,
        core: CoreKind,
        config_path: PathBuf,
    ) -> Result<StartOutcome> {
        let binary = self.resolve_and_register_binary(core, settings)?;
        let mut detected_version = None;
        if model.work_mode == crate::model::WorkMode::Proxy
            && core == CoreKind::SingBox
            && model.nodes.iter().any(|n| n.enabled && n.protocol.is_vpn())
        {
            let (status, output) = crate::core::process::run_capture(
                &binary,
                &["version".into()],
                std::time::Duration::from_secs(5),
            )
            .await?;
            detected_version = regex::Regex::new(r"\b(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)")
                .unwrap()
                .captures(&output)
                .and_then(|c| semver::Version::parse(&c[1]).ok());
            if status != 0 || detected_version.is_none() {
                return Err(Error::binary(
                    "无法确认 sing-box 版本；OpenVPN / OpenConnect 需要 1.14.0 或更新版本",
                ));
            }
        }
        if core == CoreKind::Xray && uses_native_tun(model, core) {
            let (_, output) = crate::core::process::run_capture(
                &binary,
                &["version".into()],
                std::time::Duration::from_secs(5),
            )
            .await?;
            let version = regex::Regex::new(r"\b(\d+\.\d+\.\d+)")
                .unwrap()
                .captures(&output)
                .and_then(|c| semver::Version::parse(&c[1]).ok());
            if !version
                .as_ref()
                .is_some_and(|v| *v >= semver::Version::new(26, 6, 27))
            {
                return Err(Error::binary(
                    "Xray 自动 TUN 需要 26.6.27 或更新版本，请在设置中更新内核",
                ));
            }
            detected_version = version;
        }
        #[cfg(windows)]
        if uses_native_tun(model, core) {
            let archive = crate::mesh::driver::install(
                &self.paths,
                &self.update_network(settings, settings.proxy_resource_updates),
            )
            .await?;
            let dir = binary
                .parent()
                .ok_or_else(|| Error::binary("TUN 核心路径无效"))?;
            if crate::mesh::driver::unpack(&archive, dir).is_err() {
                crate::mesh::driver::unpack(&archive, &self.paths.data_dir)?;
            }
        }
        let mut ctx = adapters::CompileContext::new(
            settings,
            self.paths.data_dir.clone(),
            self.paths.runtime_dir.clone(),
        );
        ctx.core_version = detected_version;
        ctx.mesh = self.mesh.lock().clone();
        ctx.mesh_exit = self.mesh_exit.lock().clone();
        let bridge = self.exit_bridge.lock().clone();
        let compiled = if let Some(bridge) = bridge {
            adapters::compile_exit_gateway(model, settings, &ctx, bridge.port, &bridge.password.0)?
        } else {
            adapters::adapter_for(core).compile(model, settings, &ctx)?
        };
        for warning in &compiled.warnings {
            self.logs
                .core_line(LogStream::App, &format!("WARN {warning}"));
        }
        let pretty = compiled.to_pretty_string()?;
        atomic_write_private(&config_path, pretty.as_bytes())?;
        self.logs.push(
            LogStream::App,
            crate::settings::LogLevel::Info,
            format!("已生成配置：{}", config_path.display()),
        );

        let report = adapters::validate(core, &binary, &config_path).await?;
        if !report.ok {
            self.logs.push(
                LogStream::App,
                crate::settings::LogLevel::Error,
                format!("配置校验失败：{}", report.summary()),
            );
            for issue in &report.issues {
                self.logs.push(
                    LogStream::App,
                    crate::settings::LogLevel::Error,
                    match issue.line {
                        Some(line) => format!("第 {line} 行：{}", issue.message),
                        None => issue.message.clone(),
                    },
                );
            }
        }
        *self.last_validation.lock() = Some(report.clone());
        Ok(StartOutcome {
            core,
            config_path,
            report,
            compiled,
            started: false,
        })
    }

    /// Compile → validate → spawn. The core is only spawned when validation
    /// passes; otherwise the returned report is shown verbatim in the UI.
    pub async fn start(
        &self,
        model: &AppModel,
        settings: &Settings,
        core: CoreKind,
    ) -> Result<StartOutcome> {
        let _guard = self.operation.lock().await;
        let mut outcome = self.prepare_locked(model, settings, core).await?;
        if !outcome.report.ok {
            return Ok(outcome);
        }
        let binary = self.resolve_and_register_binary(core, settings)?;
        let args = run_args(core, &outcome.config_path);
        let mut spec = SpawnSpec::new(core, binary.clone());
        spec.args = args;
        spec.workdir = Some(self.paths.data_dir.clone());
        spec.config_path = Some(outcome.config_path.clone());
        spec.elevated_tun = uses_native_tun(model, core);
        let watchdog = WatchdogConfig {
            enabled: settings.watchdog,
            max_retries: settings.watchdog_max_retries,
        };
        self.supervisor.start(spec, watchdog).await?;
        if core == CoreKind::SingBox
            && let Err(err) = self
                .restore_mesh_selectors(&outcome.compiled, settings)
                .await
        {
            self.supervisor.stop().await?;
            return Err(err);
        }
        *self.applied.lock() = Some(AppliedConfig {
            model: model.clone(),
            settings: settings.clone(),
            core,
            compiled: outcome.compiled.clone(),
            path: outcome.config_path.clone(),
        });
        outcome.started = true;
        Ok(outcome)
    }

    /// Validate a separate file before interrupting traffic. Keep the last working
    /// configuration under the operation lock until the replacement is ready.
    pub(crate) async fn reload(
        &self,
        model: &AppModel,
        settings: &Settings,
        before_start: impl std::future::Future<Output = ()>,
    ) -> Result<StartOutcome> {
        let _guard = self.operation.lock().await;
        if !self.state().phase.is_alive() {
            return Err(Error::other("内核已停止，保存的配置将在下次启动时生效"));
        }
        let old = self
            .applied
            .lock()
            .clone()
            .ok_or_else(|| Error::other("缺少已应用配置，请重新启动内核"))?;
        let core = settings.core;
        let candidate = self.paths.runtime_dir.join("reload-candidate.json");
        let result = self
            .prepare_at(model, settings, core, candidate.clone())
            .await;
        let _ = std::fs::remove_file(candidate);
        let mut outcome = result?;
        if !outcome.report.ok {
            return Ok(outcome);
        }
        outcome.config_path = self.paths.generated_config(core.as_str());
        let next = AppliedConfig {
            model: model.clone(),
            settings: settings.clone(),
            core,
            compiled: outcome.compiled.clone(),
            path: outcome.config_path.clone(),
        };
        self.supervisor.stop().await?;
        before_start.await;
        replace_with_rollback(self.spawn_reloaded(&next), async {
            self.supervisor.stop().await?;
            self.spawn_reloaded(&old).await
        })
        .await?;
        *self.applied.lock() = Some(next);
        self.remember_config(&outcome);
        outcome.started = true;
        Ok(outcome)
    }

    async fn spawn_reloaded(&self, applied: &AppliedConfig) -> Result<()> {
        self.spawn_applied(applied).await?;
        if applied.core == CoreKind::SingBox {
            self.restore_mesh_selectors(&applied.compiled, &applied.settings)
                .await?;
        }
        Ok(())
    }

    /// sing-box prefers the cached selection over its configured default. A choice made
    /// while stopped (or before a new gateway existed) must reach the running selector.
    async fn restore_mesh_selectors(
        &self,
        compiled: &CompiledConfig,
        settings: &Settings,
    ) -> Result<()> {
        let client = crate::runtime::ClashClient::new(settings.api_port, "");
        for (group, tag) in adapters::singbox::mesh_selector_defaults(compiled) {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            while let Err(error) = client.select(group, tag).await {
                if !self.state().phase.is_alive() || tokio::time::Instant::now() >= deadline {
                    return Err(Error::other(format!(
                        "无法应用出站组的节点选择，请检查内核状态：{error}"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        let _guard = self.operation.lock().await;
        self.supervisor.stop().await
    }

    /// Stops the core during application exit without queuing behind an in-flight
    /// start/reload operation (such as selector restoration or config validation).
    pub async fn stop_for_shutdown(&self) -> Result<()> {
        self.supervisor.close_and_stop().await
    }

    pub async fn restart(&self) -> Result<()> {
        let _guard = self.operation.lock().await;
        self.supervisor.restart().await
    }

    pub fn last_tail(&self) -> Vec<String> {
        self.supervisor.last_tail()
    }

    pub(crate) fn mesh_exit_runtime(&self) -> Option<adapters::MeshExitRuntime> {
        self.mesh_exit.lock().clone()
    }

    pub(crate) fn set_mesh_exit(&self, runtime: Option<adapters::MeshExitRuntime>) {
        *self.mesh_exit.lock() = runtime;
    }

    pub(crate) fn set_exit_bridge(&self, bridge: crate::mesh::ExitBridge) {
        *self.exit_bridge.lock() = Some(bridge);
    }

    pub fn mesh_plan(&self) -> crate::mesh::coexist::CoexistencePlan {
        self.mesh.lock().clone()
    }

    /// Recompile the last applied intent, keeping unsaved UI edits completely separate.
    /// Validate before stopping; retain the original bytes and restart them on failure.
    pub async fn apply_mesh_plan(
        &self,
        plan: crate::mesh::coexist::CoexistencePlan,
    ) -> Result<bool> {
        let _guard = self.operation.lock().await;
        if *self.mesh.lock() == plan {
            return Ok(false);
        }
        if !self.state().phase.is_alive() {
            *self.mesh.lock() = plan;
            return Ok(false);
        }
        let old = self
            .applied
            .lock()
            .clone()
            .ok_or_else(|| Error::other("缺少已应用代理配置，请先重新应用配置"))?;
        self.replace_applied(&old, old.model.clone(), &plan)
            .await
            .map_err(|source| Error::MeshCore {
                core: old.core,
                source: Box::new(source),
            })?;
        *self.mesh.lock() = plan;
        Ok(true)
    }

    pub(crate) fn managed_selections(&self) -> std::collections::BTreeMap<String, String> {
        self.applied
            .lock()
            .iter()
            .flat_map(|a| &a.compiled.managed_groups)
            .filter_map(|g| Some((g.id.clone(), g.active_node()?.clone())))
            .collect()
    }

    pub(crate) fn managed_probe_groups(&self) -> Vec<crate::model::OutboundGroup> {
        self.applied
            .lock()
            .as_ref()
            .map(|a| a.compiled.managed_groups.clone())
            .unwrap_or_default()
    }

    /// Identify the applied default proxy without cloning the profile on tray ticks.
    pub(crate) fn applied_proxy_node(
        &self,
        live_node: impl FnOnce(&str) -> Option<String>,
    ) -> Option<String> {
        let applied = self.applied.lock();
        let selection = &applied.as_ref()?.compiled.default_proxy;
        selection
            .live_group
            .as_deref()
            .and_then(live_node)
            .or_else(|| selection.node.clone())
    }

    pub(crate) fn applied_work_mode(&self) -> Option<crate::model::WorkMode> {
        self.applied
            .lock()
            .as_ref()
            .map(|applied| applied.model.work_mode)
    }

    /// Read the actual running configuration, never the pending editor state.
    pub(crate) fn statistics_context(&self) -> Option<(AppModel, Settings, Vec<String>)> {
        self.applied.lock().as_ref().map(|a| {
            (
                a.model.clone(),
                a.settings.clone(),
                a.compiled.inbounds.iter().map(|i| i.tag.clone()).collect(),
            )
        })
    }

    /// Select the same applied listener as the probe transport, including the
    /// separate HTTP and SOCKS listeners generated for an Xray mixed inbound.
    pub(crate) fn trace_context(
        &self,
        kind: &str,
    ) -> Option<(AppModel, Settings, Option<crate::adapters::CompiledInbound>)> {
        self.applied.lock().as_ref().map(|a| {
            let inbound = a
                .compiled
                .inbounds
                .iter()
                .find(|i| i.kind == kind || (kind != "tun" && i.kind == "mixed"))
                .cloned();
            (a.model.clone(), a.settings.clone(), inbound)
        })
    }

    pub(crate) fn applied_model(&self) -> Option<AppModel> {
        self.applied.lock().as_ref().map(|a| a.model.clone())
    }

    /// Recheck the running session under the same lock as stop, start and mesh updates.
    pub(crate) async fn switch_managed_node(
        &self,
        expected: &ProcessState,
        group: &crate::model::OutboundGroup,
        node_id: &str,
    ) -> Result<bool> {
        let _guard = self.operation.lock().await;
        let state = self.state();
        if state.phase != ProcessPhase::Running
            || state.pid != expected.pid
            || state.started_at != expected.started_at
        {
            return Ok(false);
        }
        let Some(old) = self.applied.lock().clone() else {
            return Ok(false);
        };
        if !old.compiled.managed_groups.contains(group)
            || !group.members.iter().any(|id| id == node_id)
        {
            return Ok(false);
        }
        let mut model = old.model.clone();
        if !model.select_node(&group.id, node_id) {
            return Ok(false);
        }
        let plan = self.mesh_plan();
        self.replace_applied(&old, model, &plan).await?;
        Ok(true)
    }

    async fn replace_applied(
        &self,
        old: &AppliedConfig,
        model: AppModel,
        plan: &crate::mesh::coexist::CoexistencePlan,
    ) -> Result<()> {
        let mut ctx = adapters::CompileContext::new(
            &old.settings,
            self.paths.data_dir.clone(),
            self.paths.runtime_dir.clone(),
        );
        ctx.mesh = plan.clone();
        ctx.mesh_exit = self.mesh_exit.lock().clone();
        ctx.core_version = old.compiled.core_version.clone();
        let bridge = self.exit_bridge.lock().clone();
        let compiled = if let Some(bridge) = bridge {
            adapters::compile_exit_gateway(
                &model,
                &old.settings,
                &ctx,
                bridge.port,
                &bridge.password.0,
            )?
        } else {
            adapters::adapter_for(old.core).compile(&model, &old.settings, &ctx)?
        };
        let candidate = self.paths.runtime_dir.join("runtime-candidate.json");
        atomic_write_private(&candidate, compiled.to_pretty_string()?.as_bytes())?;
        let binary = self.resolve_and_register_binary(old.core, &old.settings)?;
        let validation = adapters::validate(old.core, &binary, &candidate).await;
        let _ = std::fs::remove_file(&candidate);
        let report = validation?;
        if !report.ok {
            // Validator output may contain configuration values. Keep the raw
            // report in memory and use a safe diagnosis in notifications/logs.
            let reason = validation_failure(&report);
            *self.last_validation.lock() = Some(report);
            return Err(Error::other(reason));
        }
        self.supervisor.stop().await?;
        let next = AppliedConfig {
            model,
            compiled: compiled.clone(),
            ..old.clone()
        };
        replace_with_rollback(self.spawn_applied(&next), async {
            self.supervisor.stop().await?;
            self.spawn_applied(old).await
        })
        .await?;
        *self.applied.lock() = Some(next);
        *self.last_compiled.lock() = Some(compiled);
        *self.last_validation.lock() = Some(report);
        Ok(())
    }

    async fn spawn_applied(&self, applied: &AppliedConfig) -> Result<()> {
        atomic_write_private(
            &applied.path,
            applied.compiled.to_pretty_string()?.as_bytes(),
        )?;
        let mut spec = SpawnSpec::new(
            applied.core,
            self.resolve_and_register_binary(applied.core, &applied.settings)?,
        );
        spec.args = run_args(applied.core, &applied.path);
        spec.workdir = Some(self.paths.data_dir.clone());
        spec.config_path = Some(applied.path.clone());
        spec.elevated_tun = uses_native_tun(&applied.model, applied.core);
        // Do not let watchdog retries conceal a failed transaction.
        self.supervisor
            .start(
                spec,
                WatchdogConfig {
                    enabled: false,
                    max_retries: 0,
                },
            )
            .await?;
        let started = tokio::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(30);
        loop {
            let state = self.state();
            let ready = state.phase == ProcessPhase::Running
                && started.elapsed() >= std::time::Duration::from_secs(2);
            #[cfg(windows)]
            let ready = ready
                && state.pid.is_some_and(|pid| {
                    applied
                        .compiled
                        .inbounds
                        .iter()
                        .filter(|i| {
                            i.port != 0 && ["mixed", "http", "socks"].contains(&i.kind.as_str())
                        })
                        .all(|i| crate::system::port_owner::owns_listener(pid, i.port))
                });
            if startup_ready(state.phase, ready, tokio::time::Instant::now() >= deadline)? {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        self.supervisor.set_watchdog(WatchdogConfig {
            enabled: applied.settings.watchdog,
            max_retries: applied.settings.watchdog_max_retries,
        });
        Ok(())
    }

    /// Re-runs validation against the most recently written config.
    pub async fn revalidate(
        &self,
        core: CoreKind,
        settings: &Settings,
    ) -> Result<ValidationReport> {
        let binary = self.resolve_binary(core, settings)?;
        let path = self
            .last_config_path()
            .ok_or_else(|| Error::config("尚未生成配置，无法校验"))?;
        let report = adapters::validate(core, &binary, &path).await?;
        *self.last_validation.lock() = Some(report.clone());
        Ok(report)
    }

    /// Writes a config the user edited by hand (advanced) and validates it.
    pub async fn validate_raw(
        &self,
        core: CoreKind,
        settings: &Settings,
        raw: &str,
    ) -> Result<ValidationReport> {
        let binary = self.resolve_binary(core, settings)?;
        if serde_json::from_str::<serde_json::Value>(raw).is_err() {
            return Ok(ValidationReport {
                ok: false,
                core,
                issues: vec![adapters::ValidationIssue {
                    line: None,
                    message: "不是有效的 JSON 文本".into(),
                    raw: String::new(),
                }],
                raw_output: String::new(),
                duration_ms: 0,
                checked_at: crate::util::now_unix(),
            });
        }
        let path = self
            .paths
            .runtime_dir
            .join(format!("{}-manual.json", core.as_str()));
        atomic_write_private(&path, raw.as_bytes())?;
        let report = adapters::validate(core, &binary, &path).await?;
        Ok(report)
    }
}

fn startup_ready(phase: ProcessPhase, ready: bool, expired: bool) -> Result<bool> {
    if !matches!(phase, ProcessPhase::Starting | ProcessPhase::Running) {
        return Err(Error::other("新配置启动失败，请检查内核日志"));
    }
    if phase == ProcessPhase::Running && ready {
        return Ok(true);
    }
    if expired {
        return Err(Error::other("新配置启动超时，请检查内核日志"));
    }
    Ok(false)
}

async fn replace_with_rollback(
    replace: impl std::future::Future<Output = Result<()>>,
    restore: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    if let Err(error) = replace.await {
        restore.await.map_err(|_| {
            Error::other("新配置启动失败，原代理恢复失败；请检查内核日志并重新启动")
        })?;
        return Err(Error::other(format!("{error}；已恢复原代理配置")));
    }
    Ok(())
}

fn validation_failure(report: &ValidationReport) -> String {
    let text = report.raw_output.to_ascii_lowercase();
    let reason = if text.contains("unknown field") || text.contains("unrecognized field") {
        "内核不识别配置字段"
    } else if text.contains("unknown outbound type") || text.contains("unknown rule") {
        "内核不支持配置中的出站或规则类型"
    } else {
        "内核配置校验失败"
    };
    match report.issues.iter().find_map(|issue| issue.line) {
        Some(line) => format!("{reason}（第 {line} 行），原代理继续运行"),
        None => format!("{reason}，原代理继续运行"),
    }
}

fn uses_native_tun(model: &AppModel, core: CoreKind) -> bool {
    core.capabilities().native_tun
        && (model.work_mode == crate::model::WorkMode::Mesh
            || model
                .inbounds
                .iter()
                .any(|i| i.enabled && i.kind == crate::model::InboundKind::Tun))
}

pub fn run_args(core: CoreKind, config_path: &Path) -> Vec<String> {
    match core {
        CoreKind::Xray => vec![
            "run".into(),
            "-config".into(),
            config_path.display().to_string(),
        ],
        CoreKind::SingBox => vec!["run".into(), "-c".into(), config_path.display().to_string()],
    }
}

/// Convenience for the UI/settings screen.
pub fn binary_table(manager: &BinaryManager) -> Vec<BinaryRecord> {
    manager.list()
}

#[cfg(test)]
mod tests {
    #[test]
    fn coexist_start_waits_for_helper_and_listeners() {
        use super::*;
        assert!(!startup_ready(ProcessPhase::Starting, false, false).unwrap());
        assert!(!startup_ready(ProcessPhase::Running, false, false).unwrap());
        assert!(startup_ready(ProcessPhase::Running, true, false).unwrap());
        assert!(startup_ready(ProcessPhase::Starting, false, true).is_err());
        assert!(startup_ready(ProcessPhase::Failed, false, false).is_err());
    }

    use super::*;

    #[test]
    fn run_arguments_match_cores() {
        let path = Path::new("/tmp/config.json");
        assert_eq!(run_args(CoreKind::Xray, path)[1], "-config");
        assert_eq!(run_args(CoreKind::SingBox, path)[1], "-c");
    }

    #[tokio::test]
    async fn mesh_transaction_restores_on_start_failure() {
        let value = std::cell::Cell::new("original");
        let result = replace_with_rollback(
            async {
                value.set("candidate");
                Err(Error::other("测试失败"))
            },
            async {
                value.set("original");
                Ok(())
            },
        )
        .await;
        assert_eq!(value.get(), "original");
        assert!(result.unwrap_err().to_string().contains("已恢复"));
        replace_with_rollback(
            async {
                value.set("candidate");
                Ok(())
            },
            async {
                value.set("unexpected rollback");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(value.get(), "candidate");
    }

    #[tokio::test]
    async fn mesh_transaction_reports_failed_recovery() {
        let error = replace_with_rollback(async { Err(Error::other("测试失败")) }, async {
            Err(Error::other("恢复失败"))
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("原代理恢复失败"));
    }
}
