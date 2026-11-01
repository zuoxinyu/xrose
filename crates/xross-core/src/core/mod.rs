//! Binary store, process supervision, config validation and lifecycle.

pub mod binary;
pub mod logs;
pub mod process;

use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use crate::adapters::{self, CompiledConfig, ValidationReport};
use crate::error::{Error, Result};
use crate::model::{AppModel, CoreKind};
use crate::paths::AppPaths;
use crate::settings::Settings;
use crate::util::atomic_write;

pub use binary::{BinaryManager, BinaryRecord};
pub use logs::{LogBus, LogEntry, LogStream};
pub use process::{
    human_failure_reason, ExitInfo, ProcessPhase, ProcessState, ProcessSupervisor, SpawnSpec,
    WatchdogConfig,
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
    paths: AppPaths,
    binaries: BinaryManager,
    logs: LogBus,
    supervisor: ProcessSupervisor,
    last_validation: Mutex<Option<ValidationReport>>,
    last_compiled: Mutex<Option<CompiledConfig>>,
    last_config_path: Mutex<Option<PathBuf>>,
    last_core: Mutex<Option<CoreKind>>,
}

impl CoreManager {
    pub fn new(paths: AppPaths, logs: LogBus) -> Self {
        let binaries = BinaryManager::new(&paths);
        let supervisor = ProcessSupervisor::new(logs.clone());
        Self {
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
            if path.exists() {
                return Ok(path);
            }
            return Err(Error::binary(format!(
                "自定义 {} 路径不存在：{}",
                core.display_name(),
                path.display()
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

    pub fn set_watchdog(&self, config: WatchdogConfig) {
        self.supervisor.set_watchdog(config);
    }

    /// Compiles the model, writes the config to the runtime directory and runs
    /// the core's own `-test`/`check` before returning.
    pub async fn prepare(
        &self,
        model: &AppModel,
        settings: &Settings,
        core: CoreKind,
    ) -> Result<StartOutcome> {
        let binary = self.resolve_binary(core, settings)?;
        let ctx = adapters::CompileContext::new(
            settings,
            self.paths.data_dir.clone(),
            self.paths.runtime_dir.clone(),
        );
        let compiled = adapters::adapter_for(core).compile(model, settings, &ctx)?;
        for warning in &compiled.warnings {
            self.logs
                .core_line(LogStream::App, &format!("WARN {warning}"));
        }
        let pretty = compiled.to_pretty_string()?;
        let config_path = self.paths.generated_config(core.as_str());
        atomic_write(&config_path, pretty.as_bytes())?;
        self.logs.push(
            LogStream::App,
            crate::settings::LogLevel::Info,
            format!("已生成配置：{}", config_path.display()),
        );
        *self.last_compiled.lock() = Some(compiled.clone());
        *self.last_config_path.lock() = Some(config_path.clone());
        *self.last_core.lock() = Some(core);

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
        let mut outcome = self.prepare(model, settings, core).await?;
        if !outcome.report.ok {
            return Ok(outcome);
        }
        let binary = self.resolve_binary(core, settings)?;
        let args = run_args(core, &outcome.config_path);
        let mut spec = SpawnSpec::new(core, binary.clone());
        spec.args = args;
        spec.workdir = Some(self.paths.data_dir.clone());
        spec.config_path = Some(outcome.config_path.clone());
        let watchdog = WatchdogConfig {
            enabled: settings.watchdog,
            max_retries: settings.watchdog_max_retries,
        };
        self.supervisor.start(spec, watchdog).await?;
        outcome.started = true;
        Ok(outcome)
    }

    pub async fn stop(&self) -> Result<()> {
        self.supervisor.stop().await
    }

    pub async fn restart(&self) -> Result<()> {
        self.supervisor.restart().await
    }

    pub fn last_tail(&self) -> Vec<String> {
        self.supervisor.last_tail()
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
        atomic_write(&path, raw.as_bytes())?;
        let report = adapters::validate(core, &binary, &path).await?;
        Ok(report)
    }
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
    use super::*;

    #[test]
    fn run_arguments_match_cores() {
        let path = Path::new("/tmp/config.json");
        assert_eq!(run_args(CoreKind::Xray, path)[1], "-config");
        assert_eq!(run_args(CoreKind::SingBox, path)[1], "-c");
    }
}
