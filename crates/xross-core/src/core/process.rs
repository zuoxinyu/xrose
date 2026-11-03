use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

use super::logs::{LogBus, LogStream};
use crate::error::{Error, Result};
use crate::model::CoreKind;
use crate::settings::LogLevel;
use crate::util::{now_rfc3339, now_unix};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPhase {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl ProcessPhase {
    pub fn is_alive(&self) -> bool {
        matches!(
            self,
            ProcessPhase::Starting | ProcessPhase::Running | ProcessPhase::Stopping
        )
    }
    pub fn label(&self) -> &'static str {
        match self {
            ProcessPhase::Stopped => "已停止",
            ProcessPhase::Starting => "启动中",
            ProcessPhase::Running => "运行中",
            ProcessPhase::Stopping => "停止中",
            ProcessPhase::Failed => "启动失败",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub reason: String,
    pub detail: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessState {
    pub phase: ProcessPhase,
    pub core: Option<CoreKind>,
    pub pid: Option<u32>,
    pub started_at: Option<i64>,
    pub binary: Option<String>,
    pub restarts: u32,
    pub last_exit: Option<ExitInfo>,
    pub uplink: u64,
    pub downlink: u64,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self {
            phase: ProcessPhase::Stopped,
            core: None,
            pid: None,
            started_at: None,
            binary: None,
            restarts: 0,
            last_exit: None,
            uplink: 0,
            downlink: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    pub enabled: bool,
    pub max_retries: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub core: CoreKind,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub workdir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// For xray: the exact config path shown in validation hints.
    pub config_path: Option<PathBuf>,
    /// Windows TUN cores must be launched by the installed privileged helper.
    pub elevated_tun: bool,
}

impl SpawnSpec {
    pub fn new(core: CoreKind, program: PathBuf) -> Self {
        Self {
            core,
            program,
            args: Vec::new(),
            workdir: None,
            env: Vec::new(),
            config_path: None,
            elevated_tun: false,
        }
    }
}

struct RunContext {
    spec: SpawnSpec,
    logs: LogBus,
    state: Arc<RwLock<ProcessState>>,
    stop: watch::Receiver<bool>,
    stop_requested: Arc<AtomicBool>,
    tail: Arc<Mutex<VecDeque<String>>>,
    watchdog: Arc<Mutex<WatchdogConfig>>,
}

/// Runs the core process and keeps it alive. Restarting after a crash reuses
/// the exact same command line: config changes are applied by the manager, not
/// the watchdog.
pub struct ProcessSupervisor {
    state: Arc<RwLock<ProcessState>>,
    stop_tx: Mutex<Option<watch::Sender<bool>>>,
    stop_requested: Arc<AtomicBool>,
    logs: LogBus,
    tail: Arc<Mutex<VecDeque<String>>>,
    spec: Mutex<Option<SpawnSpec>>,
    watchdog: Arc<Mutex<WatchdogConfig>>,
    generation: Arc<AtomicBool>,
}

impl ProcessSupervisor {
    pub fn new(logs: LogBus) -> Self {
        Self {
            state: Arc::new(RwLock::new(ProcessState::default())),
            stop_tx: Mutex::new(None),
            stop_requested: Arc::new(AtomicBool::new(false)),
            logs,
            tail: Arc::new(Mutex::new(VecDeque::with_capacity(40))),
            spec: Mutex::new(None),
            watchdog: Arc::new(Mutex::new(WatchdogConfig::default())),
            generation: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn state(&self) -> ProcessState {
        self.state.read().clone()
    }

    pub fn logs(&self) -> &LogBus {
        &self.logs
    }

    pub fn set_watchdog(&self, config: WatchdogConfig) {
        *self.watchdog.lock() = config;
    }

    pub async fn start(&self, spec: SpawnSpec, watchdog: WatchdogConfig) -> Result<()> {
        if self.state.read().phase.is_alive() {
            return Err(Error::process("core 已在运行，请先停止"));
        }
        self.stop_requested.store(false, Ordering::SeqCst);
        self.generation.store(false, Ordering::SeqCst);
        let (tx, rx) = watch::channel(false);
        *self.stop_tx.lock() = Some(tx);
        *self.watchdog.lock() = watchdog;
        *self.spec.lock() = Some(spec.clone());
        self.tail.lock().clear();
        {
            let mut state = self.state.write();
            state.phase = ProcessPhase::Starting;
            state.core = Some(spec.core);
            state.pid = None;
            state.started_at = Some(now_unix());
            state.binary = Some(spec.program.display().to_string());
            state.restarts = 0;
            state.last_exit = None;
        }
        self.logs.push(
            LogStream::App,
            LogLevel::Info,
            format!(
                "启动 {}：{} {}",
                spec.core.display_name(),
                spec.program.display(),
                spec.args.join(" ")
            ),
        );
        let ctx = RunContext {
            spec,
            logs: self.logs.clone(),
            state: self.state.clone(),
            stop: rx,
            stop_requested: self.stop_requested.clone(),
            tail: self.tail.clone(),
            watchdog: self.watchdog.clone(),
        };
        tokio::spawn(run_loop(ctx));
        Ok(())
    }

    /// Stops the process; waits briefly for the state to settle so the UI can
    /// rely on `state()` right after the call returns.
    pub async fn stop(&self) -> Result<()> {
        if !self.state.read().phase.is_alive() {
            let mut state = self.state.write();
            state.phase = ProcessPhase::Stopped;
            state.pid = None;
            return Ok(());
        }
        self.stop_requested.store(true, Ordering::SeqCst);
        if let Some(tx) = self.stop_tx.lock().as_ref() {
            let _ = tx.send(true);
        }
        {
            let mut state = self.state.write();
            state.phase = ProcessPhase::Stopping;
        }
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            if !self.state.read().phase.is_alive() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        Err(Error::process("core 未能在超时时间内退出"))
    }

    /// Restarts with the last command line.
    pub async fn restart(&self) -> Result<()> {
        let spec = self
            .spec
            .lock()
            .clone()
            .ok_or_else(|| Error::process("没有可重启的 core"))?;
        let watchdog = *self.watchdog.lock();
        self.stop().await?;
        self.start(spec, watchdog).await
    }

    pub fn last_tail(&self) -> Vec<String> {
        self.tail.lock().iter().cloned().collect()
    }
}

async fn run_loop(mut ctx: RunContext) {
    let mut attempt: u32 = 0;
    loop {
        if ctx.stop_requested.load(Ordering::SeqCst) {
            mark_stopped(&ctx);
            return;
        }
        let spawn_result = spawn_child(&ctx).await;
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(err) => {
                let reason = ExitInfo {
                    code: None,
                    signal: None,
                    reason: format!("无法启动 core：{err}"),
                    detail: Some(err.to_string()),
                    at: now_unix(),
                };
                ctx.logs
                    .push(LogStream::App, LogLevel::Error, reason.reason.clone());
                fail_or_retry(&ctx, reason, &mut attempt).await.map(|_| ());
                return;
            }
        };
        let pid = child.id();
        {
            let mut state = ctx.state.write();
            state.phase = ProcessPhase::Running;
            state.pid = pid;
            state.started_at = Some(now_unix());
            state.restarts = attempt;
        }
        ctx.logs.push(
            LogStream::App,
            LogLevel::Info,
            format!("{} 已启动 (pid {:?})", ctx.spec.core.display_name(), pid),
        );

        let status = tokio::select! {
            status = child.wait() => status,
            _ = ctx.stop.changed() => {
                if *ctx.stop.borrow() {
                    let _ = child.start_kill().await;
                    let _ = child.wait().await;
                    mark_stopped(&ctx);
                    ctx.logs.push(LogStream::App, LogLevel::Info, "core 已停止");
                    return;
                }
                continue;
            }
        };
        // Release the helper session before a watchdog restart reuses this owner.
        let _ = child.start_kill().await;
        // Give the stdout/stderr readers a moment to flush the final lines.
        tokio::time::sleep(Duration::from_millis(80)).await;

        if ctx.stop_requested.load(Ordering::SeqCst) {
            mark_stopped(&ctx);
            ctx.logs.push(LogStream::App, LogLevel::Info, "core 已停止");
            return;
        }

        let exit = status
            .ok()
            .map(describe_exit_status)
            .unwrap_or_else(|| ExitInfo {
                code: None,
                signal: None,
                reason: "core 进程异常结束".into(),
                detail: None,
                at: now_unix(),
            });
        let tail: Vec<String> = ctx.tail.lock().iter().cloned().collect();
        let description = human_failure_reason(&exit, &tail);
        let info = ExitInfo {
            reason: description,
            detail: exit.detail,
            ..exit
        };
        ctx.logs.push(
            LogStream::App,
            LogLevel::Warn,
            format!("core 退出：{}", info.reason),
        );
        {
            let mut state = ctx.state.write();
            state.pid = None;
            state.last_exit = Some(info.clone());
        }
        let watchdog = *ctx.watchdog.lock();
        if watchdog.enabled && attempt < watchdog.max_retries {
            attempt += 1;
            let backoff = Duration::from_millis(500 * 2u64.min(attempt as u64).pow(2) as u64);
            ctx.logs.push(
                LogStream::App,
                LogLevel::Warn,
                format!(
                    "watchdog：{:?} 后自动重启（第 {}/{} 次）",
                    backoff, attempt, watchdog.max_retries
                ),
            );
            {
                let mut state = ctx.state.write();
                state.phase = ProcessPhase::Starting;
                state.restarts = attempt;
            }
            tokio::time::sleep(backoff).await;
            continue;
        }
        {
            let mut state = ctx.state.write();
            state.phase = ProcessPhase::Failed;
            state.pid = None;
            state.last_exit = Some(info);
        }
        return;
    }
}

enum Child {
    Local(tokio::process::Child),
    Elevated(crate::system::tun_process::Client),
}

impl Child {
    fn id(&self) -> Option<u32> {
        match self {
            Self::Local(child) => child.id(),
            Self::Elevated(child) => Some(child.id()),
        }
    }
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            Self::Local(child) => child.wait().await,
            Self::Elevated(child) => child.wait().await,
        }
    }
    async fn start_kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Local(child) => child.start_kill(),
            Self::Elevated(child) => child.stop().await,
        }
    }
}

async fn spawn_child(ctx: &RunContext) -> std::io::Result<Child> {
    if !ctx.spec.program.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("找不到 core 可执行文件：{}", ctx.spec.program.display()),
        ));
    }
    if ctx.spec.elevated_tun && !crate::system::privilege::is_elevated() {
        return crate::system::tun_process::Client::start(
            &ctx.spec,
            ctx.logs.clone(),
            ctx.tail.clone(),
        )
        .await
        .map(Child::Elevated)
        .map_err(std::io::Error::other);
    }
    let mut command = tokio::process::Command::new(&ctx.spec.program);
    command
        .args(&ctx.spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    if let Some(dir) = &ctx.spec.workdir {
        command.current_dir(dir);
    }
    for (k, v) in &ctx.spec.env {
        command.env(k, v);
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    {
        if let Some(handle) = child.raw_handle() {
            if let Err(err) = super::job::adopt(handle) {
                if super::job::take_warning() {
                    ctx.logs.push(
                        LogStream::App,
                        LogLevel::Warn,
                        format!(
                            "无法把 core 纳入 Windows 作业对象：{err}（应用异常退出时可能残留 core 进程）"
                        ),
                    );
                }
            }
        }
    }
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, LogStream::Stdout, ctx);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, LogStream::Stderr, ctx);
    }
    Ok(Child::Local(child))
}

fn spawn_reader<R>(reader: R, stream: LogStream, ctx: &RunContext)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let logs = ctx.logs.clone();
    let tail = ctx.tail.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            logs.core_line(stream, &line);
            let mut tail = tail.lock();
            tail.push_back(line);
            while tail.len() > 40 {
                tail.pop_front();
            }
        }
    });
}

fn mark_stopped(ctx: &RunContext) {
    let mut state = ctx.state.write();
    state.phase = ProcessPhase::Stopped;
    state.pid = None;
}

async fn fail_or_retry(ctx: &RunContext, info: ExitInfo, _attempt: &mut u32) -> Option<()> {
    let mut state = ctx.state.write();
    state.phase = ProcessPhase::Failed;
    state.pid = None;
    state.last_exit = Some(info);
    None
}

fn describe_exit_status(status: std::process::ExitStatus) -> ExitInfo {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let signal = status.signal();
        if let Some(sig) = signal {
            return ExitInfo {
                code: None,
                signal: Some(sig),
                reason: format!("被信号 {sig} 终止"),
                detail: None,
                at: now_unix(),
            };
        }
    }
    ExitInfo {
        code: status.code(),
        signal: None,
        reason: status
            .code()
            .map(|c| format!("退出码 {c}"))
            .unwrap_or_else(|| "异常退出".into()),
        detail: None,
        at: now_unix(),
    }
}

/// Maps a raw exit into something a non-developer can act on. Core stderr is
/// inspected first: both cores print precise causes before exiting.
pub fn human_failure_reason(info: &ExitInfo, tail: &[String]) -> String {
    let lower = tail.join("\n").to_ascii_lowercase();
    let cause = if lower.contains("address already in use") || lower.contains("bind:") {
        Some("端口已被占用")
    } else if lower.contains("access is denied")
        || lower.contains("a required privilege is not held")
    {
        Some("Windows TUN 权限不足，请重新安装并启动提权助手")
    } else if lower.contains("permission denied") || lower.contains("operation not permitted") {
        Some("权限不足（TUN/系统代理可能需要管理员权限）")
    } else if lower.contains("no such host") || lower.contains("dns") && lower.contains("failed") {
        Some("域名解析失败")
    } else if lower.contains("failed to load config")
        || lower.contains("failed to parse")
        || lower.contains("decode config")
        || lower.contains("invalid config")
        || lower.contains("unmarshal")
    {
        Some("配置文件无效")
    } else if lower.contains("dns query loopback") {
        Some("DNS 解析回环：节点域名无法在不经过代理的情况下解析")
    } else if lower.contains("initialize cache-file") {
        Some("core 缓存文件被另一个实例占用（可能存在残留的 sing-box 进程）")
    } else if lower.contains("initialize rule-set") || lower.contains("start service") {
        Some("启动阶段初始化失败（规则集下载或网络不可用）")
    } else if lower.contains("certificate") || lower.contains("tls") {
        Some("TLS 握手失败")
    } else if lower.contains("out of memory") || lower.contains("cannot allocate") {
        Some("内存不足")
    } else {
        None
    };

    match info.code {
        Some(0) if cause.is_none() => "core 正常退出".into(),
        Some(2) => "命令行参数错误".into(),
        Some(126) | Some(127) => "core 不可执行或文件缺失".into(),
        Some(137) => "core 被强制结束（可能是内存不足）".into(),
        Some(143) => "core 收到终止信号并退出".into(),
        _ => {
            if let Some(cause) = cause {
                format!("{cause}（{}）", info.reason)
            } else if let Some(signal) = info.signal {
                match signal {
                    9 => "core 被 SIGKILL 终止（可能是内存不足）".into(),
                    15 => "core 收到 SIGTERM 并退出".into(),
                    11 => "core 发生段错误（SIGSEGV），请更新 core 并附上日志反馈".into(),
                    other => format!("core 被信号 {other} 终止"),
                }
            } else {
                format!("core 异常退出（{}）", info.reason)
            }
        }
    }
}

/// Runs a one-shot process and captures output, with a hard timeout.
pub async fn run_capture(
    program: &std::path::Path,
    args: &[String],
    timeout: Duration,
) -> Result<(i32, String)> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command
        .spawn()
        .map_err(|e| Error::process(format!("无法执行 {}：{e}", program.display())))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            Ok((output.status.code().unwrap_or(-1), text))
        }
        Ok(Err(e)) => Err(Error::process(format!("运行 core 失败：{e}"))),
        Err(_) => Err(Error::process("core 检查超时（20 秒）")),
    }
}

pub fn now_stamp() -> String {
    now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tun_access_denied_is_not_reported_as_a_network_failure() {
        let info = ExitInfo {
            code: Some(1),
            reason: "退出码 1".into(),
            ..Default::default()
        };
        let message = human_failure_reason(&info, &["FATAL start service: start inbound/tun[tun-in]: configure tun interface: Access is denied.".into()]);
        assert!(message.contains("权限不足"));
        assert!(!message.contains("规则集"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn watchdog_can_be_reenabled_after_transaction_verification() {
        let supervisor = ProcessSupervisor::new(LogBus::new(50));
        let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut spec = SpawnSpec::new(CoreKind::SingBox, shell);
        spec.args = vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Milliseconds 600; exit 1".into(),
        ];
        supervisor
            .start(
                spec,
                WatchdogConfig {
                    enabled: false,
                    max_retries: 0,
                },
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while supervisor.state().phase == ProcessPhase::Starting && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(supervisor.state().phase, ProcessPhase::Running);
        supervisor.set_watchdog(WatchdogConfig {
            enabled: true,
            max_retries: 1,
        });
        while supervisor.state().restarts == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let retried = supervisor.state().restarts;
        supervisor.stop().await.unwrap();
        assert_eq!(retried, 1);
    }

    #[test]
    fn reason_detects_port_conflict() {
        let info = ExitInfo {
            code: Some(1),
            signal: None,
            reason: "退出码 1".into(),
            detail: None,
            at: 0,
        };
        let tail = vec!["listen tcp 127.0.0.1:7890: bind: address already in use".into()];
        assert!(human_failure_reason(&info, &tail).contains("端口已被占用"));
    }

    #[test]
    fn reason_detects_stale_cache_lock() {
        let info = ExitInfo {
            code: Some(1),
            signal: None,
            reason: "退出码 1".into(),
            detail: None,
            at: 0,
        };
        let tail = vec!["FATAL[0009] start service: initialize cache-file: timeout".into()];
        let reason = human_failure_reason(&info, &tail);
        assert!(reason.contains("残留"), "{reason}");
        assert!(!reason.contains("规则集下载"), "{reason}");
    }

    #[test]
    fn reason_detects_config_errors() {
        let info = ExitInfo {
            code: Some(1),
            signal: None,
            reason: "退出码 1".into(),
            detail: None,
            at: 0,
        };
        assert!(
            human_failure_reason(&info, &["failed to load config".into()]).contains("配置文件无效")
        );
    }
}
