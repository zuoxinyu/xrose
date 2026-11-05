//! Minimal-privilege IPC boundary for operations that need root/admin.
//!
//! The GUI never runs as root. A small helper is installed as a Windows
//! scheduled task / macOS LaunchDaemon / Linux systemd service and talks to
//! the GUI over a local socket (Unix domain socket or Windows named pipe)
//! using newline-delimited JSON.
//!
//! The helper exposes only the operations the GUI actually needs (TUN
//! preparation, system DNS, Linux transparent-proxy rules) and rejects peers
//! that do not belong to the installing user on Unix. Installing or removing
//! it goes through the platform elevation prompt (`runas`, `osascript`,
//! `pkexec`), never through a setuid wrapper.
//!
//! [`helper_main`] is the entry point shared by `xross-app` and `xross-cli`:
//! it recognizes the `--helper`, `--install-helper`, `--uninstall-helper`,
//! `--start-helper` and `--helper-status` flags before any GUI or database
//! code starts.

use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use interprocess::local_socket::{ListenerOptions, Stream, prelude::*};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const SOCKET_NAME: &str = "io.xross.helper.v1";

/// Flags recognized by [`helper_main`]. `xross-app` and `xross-cli` both run
/// these before touching the database or the GUI.
pub const HELPER_ARG: &str = "--helper";
pub const INSTALL_ARG: &str = "--install-helper";
pub const UNINSTALL_ARG: &str = "--uninstall-helper";
pub const START_ARG: &str = "--start-helper";
pub const STATUS_ARG: &str = "--helper-status";
const ALLOW_UID_ARG: &str = "--allow-uid";

#[cfg(target_os = "windows")]
const TASK_NAME: &str = "XrossPrivilegeHelper";
#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "io.xross.helper";
#[cfg(target_os = "linux")]
const SYSTEMD_UNIT: &str = "xross-helper.service";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HelperRequest {
    ProxyTunAddresses,
    TunCore {
        request: Box<super::tun_process::Request>,
    },
    Mesh {
        request: Box<crate::mesh::ipc::MeshRequest>,
    },
    Ping,
    Status,
    /// Create the TUN device ahead of time (Windows wintun helper).
    PrepareTun {
        interface: String,
    },
    RemoveTun {
        interface: String,
    },
    SetSystemDns {
        servers: Vec<String>,
    },
    RestoreSystemDns,
    /// Install nftables rules for tproxy mode.
    ApplyTransparentRules {
        port: u16,
    },
    ClearTransparentRules,
    /// Ask the helper to exit. Used by uninstall/restart, not by the UI.
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HelperResponse {
    Ok {
        message: String,
    },
    Unsupported {
        message: String,
    },
    Error {
        message: String,
    },
    /// Terminal response: the server writes it and stops accepting requests.
    Stop {
        message: String,
    },
}

impl HelperResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self::Ok {
            message: message.into(),
        }
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
    pub fn stop(message: impl Into<String>) -> Self {
        Self::Stop {
            message: message.into(),
        }
    }
    pub fn is_ok(&self) -> bool {
        matches!(self, HelperResponse::Ok { .. })
    }
    pub fn message(&self) -> &str {
        match self {
            HelperResponse::Ok { message }
            | HelperResponse::Unsupported { message }
            | HelperResponse::Error { message }
            | HelperResponse::Stop { message } => message,
        }
    }
}

/// Snapshot of the helper installation shown on the settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperStatus {
    pub supported: bool,
    pub installed: bool,
    pub running: bool,
    pub elevated: bool,
    pub detail: String,
    pub executable: Option<String>,
}

impl HelperStatus {
    /// Short Chinese state label; the UI may render its own translation from
    /// the booleans instead.
    pub fn label(&self) -> &'static str {
        if !self.supported {
            "当前平台不支持"
        } else if self.running {
            "运行中"
        } else if self.installed {
            "已安装，未运行"
        } else {
            "未安装"
        }
    }

    pub fn can_manage(&self) -> bool {
        self.supported
    }

    pub fn is_available(&self) -> bool {
        self.running
    }
}

#[cfg(unix)]
pub(crate) fn unix_socket_path() -> std::path::PathBuf {
    let raw = std::env::var("XROSS_HELPER_SOCKET").unwrap_or_else(|_| SOCKET_NAME.into());
    let path = std::path::PathBuf::from(&raw);
    if path.is_absolute() {
        path
    } else {
        std::path::PathBuf::from("/tmp").join(raw)
    }
}

fn socket_name() -> Result<interprocess::local_socket::Name<'static>> {
    // A foreground test helper can use an isolated pipe without replacing the
    // installed helper or accepting commands from the user's running GUI.
    #[cfg(unix)]
    {
        use interprocess::local_socket::GenericFilePath;
        unix_socket_path()
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| Error::other(format!("无效的 helper socket 名：{e}")))
    }
    #[cfg(not(unix))]
    {
        use interprocess::local_socket::GenericNamespaced;
        std::env::var("XROSS_HELPER_SOCKET")
            .unwrap_or_else(|_| SOCKET_NAME.into())
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| Error::other(format!("无效的 helper socket 名：{e}")))
    }
}

/// Calls the privileged helper. Fails fast when it is not installed.
pub fn call(request: &HelperRequest) -> Result<HelperResponse> {
    call_with_timeout(request, Duration::from_secs(10))
}

pub fn call_with_timeout(request: &HelperRequest, timeout: Duration) -> Result<HelperResponse> {
    let name = socket_name()?;
    let mut stream = Stream::connect(name)
        .map_err(|e| Error::other(format!("无法连接提权助手（未安装或未运行）：{e}")))?;
    stream.set_recv_timeout(Some(timeout)).ok();
    stream.set_send_timeout(Some(timeout)).ok();
    let line = serde_json::to_string(request)?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| Error::other(format!("发送请求失败：{e}")))?;
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| Error::other(format!("读取助手响应失败：{e}")))?;
    serde_json::from_str(response.trim()).map_err(|e| Error::other(format!("助手响应无效：{e}")))
}

/// Runs the helper server. The server handles one connection at a time (all
/// requests are tiny) and exits when a handler returns
/// [`HelperResponse::Stop`].
pub fn serve<F>(handler: F) -> Result<()>
where
    F: FnMut(HelperRequest) -> HelperResponse + Send + 'static,
{
    #[cfg(unix)]
    let socket_path = {
        let path = unix_socket_path();
        let _ = std::fs::remove_file(&path);
        path
    };
    let name = socket_name()?;
    let options = ListenerOptions::new().name(name);
    #[cfg(unix)]
    let options = options.try_overwrite(true);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        options.security_descriptor(helper_security_descriptor()?)
    };
    let listener = options
        .create_sync()
        .map_err(|e| Error::other(format!("创建 helper socket 失败：{e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Allow unprivileged client UIDs to connect(2) to the root-owned socket;
        // `authorize` and `peer_pid` enforce UID/PID checks via kernel peer credentials.
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o666));
    }
    let mut handler = handler;
    for connection in listener {
        let Ok(mut stream) = connection else { continue };
        // A client that connects and stalls must not wedge the helper.
        stream.set_recv_timeout(Some(Duration::from_secs(10))).ok();
        stream.set_send_timeout(Some(Duration::from_secs(10))).ok();
        if !authorize(&stream) {
            let denied = HelperResponse::error("未授权的连接：提权助手只接受安装用户或 root");
            let _ = stream.write_all(
                format!("{}\n", serde_json::to_string(&denied).unwrap_or_default()).as_bytes(),
            );
            continue;
        }
        let mut line = String::new();
        {
            let mut reader = BufReader::new(std::io::Read::take(&mut stream, 262_145));
            if reader.read_line(&mut line).is_err() {
                continue;
            }
        }
        if line.len() > 262_144 {
            continue;
        }
        let response = match serde_json::from_str::<HelperRequest>(line.trim()) {
            Ok(HelperRequest::TunCore { request }) => {
                if peer_pid(&stream) != Some(request.owner_pid) {
                    HelperResponse::error("TUN 调用进程身份校验失败")
                } else {
                    handler(HelperRequest::TunCore { request })
                }
            }
            Ok(HelperRequest::Mesh { request }) => {
                if peer_pid(&stream) != Some(request.owner_pid) {
                    HelperResponse::error("组网调用进程身份校验失败")
                } else {
                    handler(HelperRequest::Mesh { request })
                }
            }
            Ok(request) => handler(request),
            Err(_) => HelperResponse::error("请求解析失败，请检查客户端与助手版本"),
        };
        let stop = matches!(response, HelperResponse::Stop { .. });
        let _ = stream.write_all(
            format!("{}\n", serde_json::to_string(&response).unwrap_or_default()).as_bytes(),
        );
        if stop {
            break;
        }
    }
    super::tun_process::stop_all();
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Executes one privileged request. Shared by the real helper and by
/// `xross-cli helper` so both behave identically.
pub fn handle_request(request: HelperRequest) -> HelperResponse {
    match request {
        HelperRequest::TunCore { request } => match super::tun_process::dispatch(&request) {
            Ok(reply) => match serde_json::to_string(&reply) {
                Ok(message) => HelperResponse::ok(message),
                Err(_) => HelperResponse::error("TUN 助手响应编码失败"),
            },
            Err(error) => HelperResponse::error(error.to_string()),
        },
        HelperRequest::Mesh { request } => match crate::mesh::ipc::dispatch(&request) {
            Ok(reply) => match serde_json::to_string(&reply) {
                Ok(message) => HelperResponse::ok(message),
                Err(_) => HelperResponse::error("组网响应编码失败"),
            },
            Err(err) => HelperResponse::error(err.to_string()),
        },
        HelperRequest::ProxyTunAddresses => {
            match serde_json::to_string(&super::tun_process::active_tun_addresses()) {
                Ok(addresses) => HelperResponse::ok(addresses),
                Err(_) => HelperResponse::error("无法读取代理 TUN 状态"),
            }
        }
        HelperRequest::Ping => HelperResponse::ok("pong"),
        HelperRequest::Status => HelperResponse::ok(format!(
            "Xross 提权助手 {}（{}，管理员权限）",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS
        )),
        HelperRequest::PrepareTun { interface } => platform::prepare_tun(&interface),
        HelperRequest::RemoveTun { interface } => platform::remove_tun(&interface),
        HelperRequest::SetSystemDns { servers } => platform::set_system_dns(&servers),
        HelperRequest::RestoreSystemDns => platform::restore_system_dns(),
        HelperRequest::ApplyTransparentRules { port } => platform::apply_transparent_rules(port),
        HelperRequest::ClearTransparentRules => platform::clear_transparent_rules(),
        HelperRequest::Shutdown => {
            super::tun_process::stop_all();
            HelperResponse::stop("提权助手已退出")
        }
    }
}

/// True when the helper answers `Ping` (cached with [`status`]).
pub fn helper_available() -> bool {
    status().running
}

/// `true` when this process already has administrator/root rights.
pub fn is_elevated() -> bool {
    static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ELEVATED.get_or_init(detect_elevated)
}

fn detect_elevated() -> bool {
    #[cfg(windows)]
    {
        windows_elevated()
    }
    #[cfg(unix)]
    {
        libc_geteuid() == 0
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(windows)]
fn windows_elevated() -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Security::{
            GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
            TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::Shell::IsUserAnAdmin,
    };
    // An explicitly labelled executable can retain an enabled Administrators
    // SID while being capped at Medium integrity. Such a token cannot use the
    // privileges needed for Wintun, even though IsUserAnAdmin returns true.
    unsafe {
        let mut raw_token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) == 0 {
            return false;
        }
        let token = OwnedHandle::from_raw_handle(raw_token);
        let mut size = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
        if size == 0 {
            return false;
        }
        let mut data = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            data.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return false;
        }
        let sid = (*(data.as_ptr() as *const TOKEN_MANDATORY_LABEL)).Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return false;
        }
        windows_token_is_elevated(
            IsUserAnAdmin() != 0,
            *GetSidSubAuthority(sid, u32::from(count) - 1),
        )
    }
}

#[cfg(windows)]
fn windows_token_is_elevated(administrator: bool, integrity: u32) -> bool {
    administrator && integrity >= 0x3000
}

#[cfg(all(test, windows))]
#[test]
fn administrator_membership_at_medium_integrity_is_not_elevation() {
    assert!(!windows_token_is_elevated(true, 0x2000));
    assert!(!windows_token_is_elevated(false, 0x3000));
    assert!(windows_token_is_elevated(true, 0x3000));
    assert!(windows_token_is_elevated(true, 0x4000));
}

/// Cheap status used before the background monitor has probed the pipe. It
/// never blocks, so constructing an `AppService` cannot stall on a stale
/// helper socket.
pub fn initial_status() -> HelperStatus {
    let supported = supported();
    HelperStatus {
        supported,
        installed: supported && platform::is_installed(),
        running: false,
        elevated: is_elevated(),
        detail: "正在检测提权助手…".to_string(),
        executable: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
    }
}

static STATUS_CACHE: Mutex<Option<(Instant, HelperStatus)>> = Mutex::new(None);

/// Cached [`HelperStatus`], refreshed at most every two seconds so the UI can
/// poll `snapshot()` without touching the pipe on every frame.
pub fn status() -> HelperStatus {
    let mut cache = STATUS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, cached)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(5) {
            return cached.clone();
        }
    }
    let computed = compute_status();
    *cache = Some((Instant::now(), computed.clone()));
    computed
}

/// Drops the cached status after install/uninstall so the next poll is fresh.
pub fn invalidate_status() {
    *STATUS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn compute_status() -> HelperStatus {
    let supported = supported();
    let running = matches!(
        call_with_timeout(&HelperRequest::Ping, Duration::from_millis(1500)),
        Ok(response) if response.is_ok()
    );
    // A running helper necessarily means it was installed (or started by a
    // privileged user); skipping the task/service query keeps the 400 ms UI
    // poll from spawning `schtasks`/`systemctl` continuously.
    let installed = supported && (running || platform::is_installed());
    let detail = if !supported {
        "该操作系统没有可用的提权助手实现".to_string()
    } else if running {
        "助手正在运行：可代启 TUN 核心、修改系统 DNS 与透明代理规则".to_string()
    } else if installed {
        "已安装但未运行：点击“启动”或重新安装".to_string()
    } else {
        "尚未安装：开启 TUN、修改系统 DNS 或透明代理前需安装提权助手".to_string()
    };
    HelperStatus {
        supported,
        installed,
        running,
        elevated: is_elevated(),
        detail,
        executable: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
    }
}

pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

// ---------------------------------------------------------------------------
// Installation lifecycle
// ---------------------------------------------------------------------------

/// Installs the helper (platform service/task) and starts it. Requires an
/// elevation prompt unless the process is already elevated.
pub fn install() -> Result<String> {
    if !supported() {
        return Err(Error::other("当前平台不支持提权助手"));
    }
    if !is_elevated() {
        platform::run_elevated(&elevated_args(INSTALL_ARG))?;
        invalidate_status();
        if !platform::is_installed() {
            return Err(Error::other(
                "提权助手安装未生效（计划任务/服务不存在），请检查系统日志",
            ));
        }
        let running = wait_until_running(Duration::from_secs(8));
        return Ok(if running {
            "提权助手已安装并启动".into()
        } else {
            "提权助手已安装，但尚未响应；下次登录会自动启动".into()
        });
    }
    let message = platform::install()?;
    invalidate_status();
    Ok(message)
}

/// Re-registers the installed service with this executable and verifies the replacement.
pub fn update() -> Result<String> {
    if !supported() {
        return Err(Error::other("当前平台不支持提权助手"));
    }
    if !platform::is_installed() {
        return Err(Error::other("请先安装提权助手"));
    }
    if is_elevated() {
        platform::install()?;
    } else {
        platform::run_elevated(&elevated_args(INSTALL_ARG))?;
    }
    invalidate_status();
    if wait_until_running(Duration::from_secs(8)) {
        Ok("提权助手已更新并启动".into())
    } else {
        Err(Error::other(
            "提权助手已更新，但未能启动，请检查系统服务状态后重试",
        ))
    }
}

/// Removes the service/task. A running helper is stopped first.
pub fn uninstall() -> Result<String> {
    if !supported() {
        return Err(Error::other("当前平台不支持提权助手"));
    }
    if !is_elevated() {
        platform::run_elevated(&elevated_args(UNINSTALL_ARG))?;
        invalidate_status();
        if platform::is_installed() {
            return Err(Error::other(
                "卸载未生效，请以管理员身份手动删除计划任务/服务",
            ));
        }
        return Ok("提权助手已卸载".into());
    }
    let _ = request_shutdown();
    let message = platform::uninstall()?;
    invalidate_status();
    Ok(message)
}

/// Restarts an installed helper so it picks up a new binary.
pub fn restart() -> Result<String> {
    if !supported() {
        return Err(Error::other("当前平台不支持提权助手"));
    }
    if !platform::is_installed() {
        return Err(Error::other("提权助手尚未安装"));
    }
    let _ = request_shutdown();
    std::thread::sleep(Duration::from_millis(250));
    if !is_elevated() {
        platform::run_elevated(&elevated_args(START_ARG))?;
    } else {
        platform::start()?;
    }
    invalidate_status();
    if wait_until_running(Duration::from_secs(6)) {
        Ok("提权助手已重启".into())
    } else {
        Err(Error::other(
            "提权助手未能重启，请查看系统服务/计划任务状态",
        ))
    }
}

/// Starts an already installed helper.
pub fn start() -> Result<String> {
    if !supported() {
        return Err(Error::other("当前平台不支持提权助手"));
    }
    if status().running {
        return Ok("提权助手已在运行".into());
    }
    if !platform::is_installed() {
        return Err(Error::other("请先安装提权助手"));
    }
    if !is_elevated() {
        platform::run_elevated(&elevated_args(START_ARG))?;
    } else {
        platform::start()?;
    }
    invalidate_status();
    if wait_until_running(Duration::from_secs(6)) {
        Ok("提权助手已启动".into())
    } else {
        Err(Error::other(
            "提权助手未能启动，请查看系统服务/计划任务状态",
        ))
    }
}

pub fn request_shutdown() -> Result<()> {
    call_with_timeout(&HelperRequest::Shutdown, Duration::from_secs(2))?;
    Ok(())
}

fn wait_until_running(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(
            call_with_timeout(&HelperRequest::Ping, Duration::from_millis(600)),
            Ok(response) if response.is_ok()
        ) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn elevated_args(flag: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        vec![
            flag.to_string(),
            ALLOW_UID_ARG.into(),
            libc_geteuid().to_string(),
        ]
    }
    #[cfg(not(unix))]
    {
        vec![flag.to_string()]
    }
}

// ---------------------------------------------------------------------------
// Process entry points
// ---------------------------------------------------------------------------

/// Handles helper flags before the GUI starts. Returns `Ok(true)` when the
/// flag was consumed and the process should exit.
pub fn helper_main(args: &[String]) -> Result<bool> {
    if args.first().map(String::as_str) == Some("--mesh-worker") {
        crate::mesh::worker::main()?;
        return Ok(true);
    }
    let Some(flag) = args.first().map(String::as_str) else {
        return Ok(false);
    };
    if !matches!(
        flag,
        HELPER_ARG | INSTALL_ARG | UNINSTALL_ARG | START_ARG | STATUS_ARG
    ) {
        return Ok(false);
    }
    if let Some(uid) = parse_allow_uid(args) {
        ALLOWED_UID.set(uid).ok();
    }
    match flag {
        HELPER_ARG => run_helper()?,
        INSTALL_ARG => {
            let message = platform::install()?;
            invalidate_status();
            println!("{message}");
        }
        UNINSTALL_ARG => {
            let message = platform::uninstall()?;
            invalidate_status();
            println!("{message}");
        }
        START_ARG => {
            let message = platform::start()?;
            invalidate_status();
            println!("{message}");
        }
        STATUS_ARG => {
            let status = status();
            println!("{}", serde_json::to_string_pretty(&status_json(&status))?);
        }
        _ => unreachable!(),
    }
    Ok(true)
}

fn status_json(status: &HelperStatus) -> serde_json::Value {
    serde_json::json!({
        "supported": status.supported,
        "installed": status.installed,
        "running": status.running,
        "elevated": status.elevated,
        "label": status.label(),
        "detail": status.detail,
        "executable": status.executable,
    })
}

fn parse_allow_uid(args: &[String]) -> Option<u32> {
    args.iter()
        .position(|arg| arg == ALLOW_UID_ARG)
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
}

fn run_helper() -> Result<()> {
    if !is_elevated() {
        return Err(Error::other(
            "提权助手必须以管理员/root 身份运行（安装后由系统服务自动启动）",
        ));
    }
    if matches!(
        call_with_timeout(&HelperRequest::Ping, Duration::from_millis(500)),
        Ok(response) if response.is_ok()
    ) {
        return Ok(());
    }
    crate::mesh::platform::recover_dns(
        &crate::paths::AppPaths::discover()
            .data_dir
            .join("mesh-state"),
    )?;
    serve(handle_request)
}

/// UID allowed to talk to the helper (Unix). Root is always accepted.
static ALLOWED_UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

#[cfg(unix)]
pub(crate) fn allowed_uid() -> Option<u32> {
    ALLOWED_UID.get().copied().or_else(|| {
        let uid = libc_geteuid();
        (uid != 0).then_some(uid)
    })
}

fn peer_pid(stream: &Stream) -> Option<u32> {
    if let Some(pid) = stream.peer_creds().ok().and_then(|c| c.pid()) {
        return Some(pid as u32);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::{AsFd, AsRawFd};
        #[allow(irrefutable_let_patterns)]
        let Stream::UdSocket(ref socket) = *stream else {
            return None;
        };
        unsafe extern "C" {
            fn getsockopt(
                socket: std::ffi::c_int,
                level: std::ffi::c_int,
                option_name: std::ffi::c_int,
                option_value: *mut std::ffi::c_void,
                option_len: *mut u32,
            ) -> std::ffi::c_int;
        }
        const SOL_LOCAL: std::ffi::c_int = 0;
        const LOCAL_PEERPID: std::ffi::c_int = 2;
        let mut pid: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as u32;
        let rc = unsafe {
            getsockopt(
                socket.as_fd().as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERPID,
                (&mut pid as *mut i32).cast(),
                &mut len,
            )
        };
        if rc == 0 && pid > 0 {
            return Some(pid as u32);
        }
    }
    None
}

fn authorize(stream: &Stream) -> bool {
    #[cfg(unix)]
    {
        match stream.peer_creds() {
            Ok(creds) => match creds.euid() {
                Some(0) => true,
                Some(uid) => {
                    let expected = ALLOWED_UID.get().copied().unwrap_or_else(libc_geteuid);
                    uid == expected
                }
                None => false,
            },
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        stream
            .peer_creds()
            .ok()
            .and_then(|c| c.pid())
            .is_some_and(|pid| crate::mesh::platform::OwnerProcess::open(pid as u32).is_ok())
    }
}

#[cfg(windows)]
fn current_user_sid() -> Result<String> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(Error::other("无法读取助手用户身份"));
        }
        let _token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        let mut data = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(token, TokenUser, data.as_mut_ptr().cast(), size, &mut size) == 0 {
            return Err(Error::other("无法读取助手用户身份"));
        }
        let mut text = std::ptr::null_mut();
        if ConvertSidToStringSidW((*(data.as_ptr() as *const TOKEN_USER)).User.Sid, &mut text) == 0
        {
            return Err(Error::other("无法验证助手用户身份"));
        }
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sid)
    }
}

#[cfg(windows)]
fn helper_security_descriptor()
-> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    let sid = current_user_sid()?;
    // An elevated process's default pipe ACL/label rejects its own unelevated
    // GUI. Explicitly admit that SID at Medium IL, then verify every peer PID.
    let sddl = widestring::U16CString::from_str(format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{sid})S:(ML;;NW;;;ME)"
    ))
    .map_err(|_| Error::other("助手管道安全描述符无效"))?;
    interprocess::os::windows::security_descriptor::SecurityDescriptor::deserialize(&sddl)
        .map_err(|_| Error::other("无法设置助手管道用户访问权限"))
}

#[cfg(unix)]
fn libc_geteuid() -> u32 {
    // Avoid pulling in libc as a direct dependency for this one call.
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

/// UID the installed helper should accept. After elevation the effective UID
/// is 0, so prefer the `--allow-uid` flag recorded by the GUI.
#[cfg(unix)]
fn install_uid() -> u32 {
    ALLOWED_UID
        .get()
        .copied()
        .or_else(|| {
            ["SUDO_UID", "PKEXEC_UID"]
                .iter()
                .find_map(|key| std::env::var(key).ok())
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or_else(libc_geteuid)
}

// ---------------------------------------------------------------------------
// Platform implementations
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use super::{HelperResponse, TASK_NAME};
    use crate::error::{Error, Result};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn is_installed() -> bool {
        command_ok("schtasks", &["/Query", "/TN", TASK_NAME])
    }

    pub fn install() -> Result<String> {
        let source =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        let directory = crate::paths::AppPaths::discover().data_dir.join("helper");
        crate::mesh::platform::secure_directory(&directory)?;
        let exe = directory.join("xross-helper.exe");
        if source != exe {
            let _ = super::request_shutdown();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
            while super::helper_available() && std::time::Instant::now() < deadline {
                super::invalidate_status();
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let bytes = std::fs::read(&source)
                .map_err(|_| Error::other("无法读取助手安装文件，请重新安装 Xross"))?;
            crate::util::atomic_write(&exe, &bytes)
                .map_err(|_| Error::other("无法更新助手程序，请停止提权助手后重新安装"))?;
        }
        // Only the installed helper receives a High label. A Medium-labelled
        // development tree must not cap the task or its worker processes.
        run_collect(
            "icacls",
            &[&exe.to_string_lossy(), "/setintegritylevel", "H"],
        )?;
        let xml = task_xml(&exe, &super::current_user_sid()?);
        let path = std::env::temp_dir().join("xross-helper-task.xml");
        write_utf16(&path, &xml)?;
        let path_text = path.display().to_string();
        let _ = command_ok("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"]);
        let created = run_collect(
            "schtasks",
            &["/Create", "/TN", TASK_NAME, "/XML", &path_text, "/F"],
        );
        let _ = std::fs::remove_file(&path);
        let created = created?;
        if !is_installed() {
            return Err(Error::other(format!(
                "创建计划任务失败：{}",
                created.trim()
            )));
        }
        start()?;
        Ok(format!(
            "已安装提权助手（计划任务 {TASK_NAME}，登录时以最高权限启动）"
        ))
    }

    pub fn uninstall() -> Result<String> {
        let _ = command_ok("schtasks", &["/End", "/TN", TASK_NAME]);
        let out = run_collect("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])?;
        if is_installed() {
            return Err(Error::other(format!("删除计划任务失败：{}", out.trim())));
        }
        Ok("提权助手已卸载（计划任务已删除）".into())
    }

    pub fn start() -> Result<String> {
        run_collect("schtasks", &["/Run", "/TN", TASK_NAME])?;
        Ok("已请求系统启动提权助手".into())
    }

    fn command_ok(program: &str, args: &[&str]) -> bool {
        Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn run_collect(program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| Error::other(format!("无法运行 {program}：{e}")))?;
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        if output.status.success() {
            Ok(text)
        } else {
            Err(Error::other(format!(
                "{program} 执行失败（{}）：{}",
                output.status,
                text.trim()
            )))
        }
    }

    fn write_utf16(path: &Path, text: &str) -> Result<()> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(path, bytes)
            .map_err(|e| Error::other(format!("写入 {} 失败：{e}", path.display())))
    }

    fn xml_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    /// Bind both logon and on-demand activation to the installing user's
    /// highest interactive token; a group principal does not identify the
    /// individual whose GUI and driver operations this helper must serve.
    fn task_xml(exe: &Path, user_sid: &str) -> String {
        let command = xml_escape(&exe.to_string_lossy());
        let user_sid = xml_escape(user_sid);
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Xross privilege helper (TUN / system DNS / transparent rules)</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user_sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user_sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>
      <Arguments>--helper</Arguments>
    </Exec>
  </Actions>
</Task>
"#
        )
    }

    #[cfg(test)]
    #[test]
    fn scheduled_helper_uses_the_installing_users_highest_interactive_token() {
        let sid = "S-1-5-21-111-222-333-1001";
        let xml = task_xml(Path::new(r"C:\Xross & test\xross-app.exe"), sid);
        assert_eq!(xml.matches(&format!("<UserId>{sid}</UserId>")).count(), 2);
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(!xml.contains("<GroupId>"));
        assert!(xml.contains(r"C:\Xross &amp; test\xross-app.exe"));
        assert!(xml.contains("<Arguments>--helper</Arguments>"));
    }

    pub fn run_elevated(args: &[String]) -> Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, INFINITE, WaitForSingleObject,
        };
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let exe =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        let verb: Vec<u16> = "runas\0".encode_utf16().collect();
        let file: Vec<u16> = exe
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let params_text = args.join(" ");
        let params: Vec<u16> = params_text
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        // Hide the helper operation's console, while preserving the UAC prompt.
        info.nShow = SW_HIDE;

        // SAFETY: all pointers refer to live, null-terminated UTF-16 buffers
        // that outlive the call.
        let ok = unsafe { ShellExecuteExW(&mut info) };
        if ok == 0 {
            return Err(Error::other(
                "未能弹出管理员授权（UAC 被取消，或系统策略禁止提权）",
            ));
        }
        let handle = info.hProcess;
        if handle.is_null() {
            return Err(Error::other("提权进程没有返回句柄"));
        }
        // SAFETY: ShellExecuteExW handed us an owned process handle.
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            let mut code: u32 = 1;
            GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            if code != 0 {
                return Err(Error::other(format!("提权操作失败（退出码 {code}）")));
            }
        }
        Ok(())
    }

    pub fn prepare_tun(interface: &str) -> HelperResponse {
        HelperResponse::ok(format!(
            "TUN 接口“{interface}”将由提权助手代启内核创建；驱动由客户端在启动前校验部署"
        ))
    }

    pub fn remove_tun(_interface: &str) -> HelperResponse {
        HelperResponse::ok("TUN 适配器会随 core 退出自动回收")
    }

    pub fn set_system_dns(servers: &[String]) -> HelperResponse {
        let servers = match parse_servers(servers) {
            Ok(servers) => servers,
            Err(err) => return HelperResponse::error(err),
        };
        let list = servers
            .iter()
            .map(|server| format!("'{server}'"))
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "$ErrorActionPreference='Stop'; \
             $idx=(Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1).InterfaceIndex; \
             if(-not $idx){{throw '找不到默认路由接口'}}; \
             Set-DnsClientServerAddress -InterfaceIndex $idx -ServerAddresses @({list}); 'ok'"
        );
        match run_powershell(&script) {
            Ok(_) => HelperResponse::ok(format!("系统 DNS 已设置为 {}", servers.join(", "))),
            Err(err) => HelperResponse::error(format!("设置系统 DNS 失败：{err}")),
        }
    }

    pub fn restore_system_dns() -> HelperResponse {
        let script = "$ErrorActionPreference='Stop'; \
             $idx=(Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric | Select-Object -First 1).InterfaceIndex; \
             if(-not $idx){{throw '找不到默认路由接口'}}; \
             Set-DnsClientServerAddress -InterfaceIndex $idx -ResetServerAddresses; 'ok'";
        match run_powershell(script) {
            Ok(_) => HelperResponse::ok("系统 DNS 已恢复为自动获取"),
            Err(err) => HelperResponse::error(format!("恢复系统 DNS 失败：{err}")),
        }
    }

    pub fn apply_transparent_rules(_port: u16) -> HelperResponse {
        HelperResponse::unsupported("透明代理（tproxy）规则仅支持 Linux + nftables")
    }

    pub fn clear_transparent_rules() -> HelperResponse {
        HelperResponse::unsupported("透明代理（tproxy）规则仅支持 Linux + nftables")
    }

    fn run_powershell(script: &str) -> Result<String> {
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| Error::other(format!("无法运行 PowerShell：{e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let text = String::from_utf8_lossy(&output.stderr);
            Err(Error::other(if text.trim().is_empty() {
                format!("PowerShell 退出码 {}", output.status)
            } else {
                text.trim().to_string()
            }))
        }
    }

    fn parse_servers(servers: &[String]) -> std::result::Result<Vec<String>, String> {
        let mut out = Vec::new();
        for raw in servers {
            let value = raw.trim();
            match value.parse::<std::net::IpAddr>() {
                Ok(ip) => out.push(ip.to_string()),
                Err(_) => return Err(format!("无效的 DNS 地址：{value}")),
            }
        }
        if out.is_empty() {
            return Err("DNS 服务器列表不能为空".into());
        }
        Ok(out)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use super::{HelperResponse, LAUNCHD_LABEL};
    use crate::error::{Error, Result};

    const PLIST: &str = "/Library/LaunchDaemons/io.xross.helper.plist";

    pub fn is_installed() -> bool {
        Path::new(PLIST).exists()
    }

    pub fn install() -> Result<String> {
        let exe =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        let uid = super::install_uid();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>--helper</string>
    <string>--allow-uid</string>
    <string>{uid}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
            xml_escape(&exe.to_string_lossy())
        );
        let _ = super::request_shutdown();
        if let Some(parent) = Path::new(PLIST).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::other(format!("创建 {} 失败：{e}", parent.display())))?;
        }
        std::fs::write(PLIST, plist)
            .map_err(|e| Error::other(format!("写入 {PLIST} 失败：{e}")))?;
        let _ = std::fs::set_permissions(PLIST, std::fs::Permissions::from_mode(0o644));
        let target = format!("system/{LAUNCHD_LABEL}");
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .status();
        let _ = std::fs::remove_file(super::unix_socket_path());
        let _ = Command::new("launchctl").args(["enable", &target]).status();
        let bootstrapped = Command::new("launchctl")
            .args(["bootstrap", "system", PLIST])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !bootstrapped {
            run("launchctl", &["load", "-w", PLIST])?;
        }
        run("launchctl", &["kickstart", "-k", &target])?;
        Ok(format!("已安装提权助手（LaunchDaemon {LAUNCHD_LABEL}）"))
    }

    pub fn start() -> Result<String> {
        let _ = install()?;
        Ok("已请求 launchd 启动提权助手".into())
    }

    pub fn uninstall() -> Result<String> {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("system/{LAUNCHD_LABEL}")])
            .status();
        let _ = Command::new("launchctl")
            .args(["unload", "-w", PLIST])
            .status();
        let _ = std::fs::remove_file(super::unix_socket_path());
        if Path::new(PLIST).exists() {
            std::fs::remove_file(PLIST)
                .map_err(|e| Error::other(format!("删除 {PLIST} 失败：{e}")))?;
        }
        Ok("提权助手已卸载（LaunchDaemon 已删除）".into())
    }

    pub fn run_elevated(args: &[String]) -> Result<()> {
        let exe =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        let mut command = shell_quote(&exe.to_string_lossy());
        for arg in args {
            command.push(' ');
            command.push_str(&shell_quote(arg));
        }
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            script_escape(&command)
        );
        let output = Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| Error::other(format!("无法运行 osascript：{e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            let text = String::from_utf8_lossy(&output.stderr);
            Err(Error::other(format!(
                "管理员授权失败或被取消：{}",
                text.trim()
            )))
        }
    }

    pub fn prepare_tun(interface: &str) -> HelperResponse {
        HelperResponse::ok(format!("TUN 接口“{interface}”将由提权助手代启内核创建"))
    }

    pub fn remove_tun(_interface: &str) -> HelperResponse {
        HelperResponse::ok("utun 会随 core 退出自动回收")
    }

    pub fn set_system_dns(servers: &[String]) -> HelperResponse {
        let servers = match parse_servers(servers) {
            Ok(servers) => servers,
            Err(err) => return HelperResponse::error(err),
        };
        let service = match primary_service() {
            Ok(service) => service,
            Err(err) => return HelperResponse::error(err.to_string()),
        };
        let mut command = Command::new("networksetup");
        command.arg("-setdnsservers").arg(&service);
        for server in &servers {
            command.arg(server);
        }
        match command.output() {
            Ok(output) if output.status.success() => HelperResponse::ok(format!(
                "网络服务“{service}”的 DNS 已设置为 {}",
                servers.join(", ")
            )),
            Ok(output) => HelperResponse::error(format!(
                "networksetup 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(err) => HelperResponse::error(format!("无法运行 networksetup：{err}")),
        }
    }

    pub fn restore_system_dns() -> HelperResponse {
        let service = match primary_service() {
            Ok(service) => service,
            Err(err) => return HelperResponse::error(err.to_string()),
        };
        match Command::new("networksetup")
            .args(["-setdnsservers", &service, "Empty"])
            .output()
        {
            Ok(output) if output.status.success() => {
                HelperResponse::ok(format!("网络服务“{service}”的 DNS 已恢复为自动"))
            }
            Ok(output) => HelperResponse::error(format!(
                "networksetup 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(err) => HelperResponse::error(format!("无法运行 networksetup：{err}")),
        }
    }

    pub fn apply_transparent_rules(_port: u16) -> HelperResponse {
        HelperResponse::unsupported("透明代理（tproxy）规则仅支持 Linux + nftables")
    }

    pub fn clear_transparent_rules() -> HelperResponse {
        HelperResponse::unsupported("透明代理（tproxy）规则仅支持 Linux + nftables")
    }

    fn parse_servers(servers: &[String]) -> std::result::Result<Vec<String>, String> {
        let mut out = Vec::new();
        for raw in servers {
            let value = raw.trim();
            match value.parse::<std::net::IpAddr>() {
                Ok(ip) => out.push(ip.to_string()),
                Err(_) => return Err(format!("无效的 DNS 地址：{value}")),
            }
        }
        if out.is_empty() {
            return Err("DNS 服务器列表不能为空".into());
        }
        Ok(out)
    }

    /// Maps the default-route interface (`en0`) to the networksetup service
    /// name (`Wi-Fi`) by parsing `networksetup -listnetworkserviceorder`.
    fn primary_service() -> Result<String> {
        let route = run("route", &["-n", "get", "default"])?;
        let interface = route
            .lines()
            .find_map(|line| line.trim().strip_prefix("interface:"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::other("找不到默认路由接口"))?;
        let order = run("networksetup", &["-listnetworkserviceorder"])?;
        let mut current: Option<String> = None;
        for line in order.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix('(') {
                if let Some((_, name)) = rest.split_once(") ") {
                    current = Some(name.trim().to_string());
                }
            } else if line.contains("Device:") && line.contains(interface) {
                if let Some(service) = current {
                    return Ok(service);
                }
            }
        }
        Err(Error::other(format!(
            "找不到接口 {interface} 对应的网络服务，请手动设置 DNS"
        )))
    }

    fn run(program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| Error::other(format!("无法运行 {program}：{e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(Error::other(format!(
                "{program} 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn script_escape(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }

    fn xml_escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::io::Write as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use super::{HelperResponse, SYSTEMD_UNIT};
    use crate::error::{Error, Result};

    const UNIT_PATH: &str = "/etc/systemd/system/xross-helper.service";
    const FWMARK: &str = "0x1";

    pub fn is_installed() -> bool {
        Path::new(UNIT_PATH).exists()
    }

    pub fn install() -> Result<String> {
        let exe =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        let uid = super::install_uid();
        let unit = format!(
            "[Unit]\nDescription=Xross privilege helper (TUN / DNS / transparent proxy)\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} --helper --allow-uid {uid}\nRestart=on-failure\nRestartSec=2\nNoNewPrivileges=no\n\n[Install]\nWantedBy=multi-user.target\n",
            exe.display()
        );
        if let Some(parent) = Path::new(UNIT_PATH).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::other(format!("创建 {} 失败：{e}", parent.display())))?;
        }
        std::fs::write(UNIT_PATH, unit)
            .map_err(|e| Error::other(format!("写入 {UNIT_PATH} 失败：{e}")))?;
        run("systemctl", &["daemon-reload"])?;
        run("systemctl", &["enable", SYSTEMD_UNIT])?;
        // Enabling an active unit does not replace its running executable.
        let _ = super::request_shutdown();
        run("systemctl", &["restart", SYSTEMD_UNIT])?;
        Ok(format!("已安装提权助手（systemd 服务 {SYSTEMD_UNIT}）"))
    }

    pub fn start() -> Result<String> {
        let _ = std::fs::remove_file(super::unix_socket_path());
        run("systemctl", &["restart", SYSTEMD_UNIT])?;
        Ok("已请求 systemd 启动提权助手".into())
    }

    pub fn uninstall() -> Result<String> {
        let _ = Command::new("systemctl")
            .args(["disable", "--now", SYSTEMD_UNIT])
            .status();
        let _ = std::fs::remove_file(super::unix_socket_path());
        if Path::new(UNIT_PATH).exists() {
            std::fs::remove_file(UNIT_PATH)
                .map_err(|e| Error::other(format!("删除 {UNIT_PATH} 失败：{e}")))?;
        }
        let _ = Command::new("systemctl").arg("daemon-reload").status();
        let _ = clear_transparent_rules();
        Ok("提权助手已卸载（systemd 服务已删除）".into())
    }

    pub fn run_elevated(args: &[String]) -> Result<()> {
        let exe =
            std::env::current_exe().map_err(|e| Error::other(format!("定位当前程序失败：{e}")))?;
        match Command::new("pkexec").arg(&exe).args(args).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(Error::other(format!(
                "pkexec 退出码 {}：管理员授权被取消或失败",
                status.code().unwrap_or(-1)
            ))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(Error::other(
                "未找到 pkexec：请安装 polkit，或手动以 root 运行 `xross-cli helper install`",
            )),
            Err(err) => Err(Error::other(format!("无法启动 pkexec：{err}"))),
        }
    }

    pub fn prepare_tun(interface: &str) -> HelperResponse {
        let device = Path::new("/dev/net/tun");
        if !device.exists() {
            let _ = Command::new("modprobe").arg("tun").status();
        }
        if device.exists() {
            HelperResponse::ok(format!(
                "/dev/net/tun 已就绪，TUN 接口“{interface}”将由提权助手代启内核创建"
            ))
        } else {
            HelperResponse::error(
                "系统没有 /dev/net/tun：请加载 tun 内核模块（modprobe tun）后重试",
            )
        }
    }

    pub fn remove_tun(_interface: &str) -> HelperResponse {
        HelperResponse::ok("TUN 接口会随 core 退出自动回收")
    }

    pub fn set_system_dns(servers: &[String]) -> HelperResponse {
        let servers = match parse_servers(servers) {
            Ok(servers) => servers,
            Err(err) => return HelperResponse::error(err),
        };
        let interface = match default_interface() {
            Ok(interface) => interface,
            Err(err) => return HelperResponse::error(err.to_string()),
        };
        let mut args = vec!["dns".to_string(), interface.clone()];
        args.extend(servers.iter().cloned());
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        match run("resolvectl", &refs) {
            Ok(_) => HelperResponse::ok(format!(
                "接口 {interface} 的 DNS 已设置为 {}",
                servers.join(", ")
            )),
            Err(err) => HelperResponse::error(format!("设置系统 DNS 失败：{err}")),
        }
    }

    pub fn restore_system_dns() -> HelperResponse {
        let interface = match default_interface() {
            Ok(interface) => interface,
            Err(err) => return HelperResponse::error(err.to_string()),
        };
        match run("resolvectl", &["revert", &interface]) {
            Ok(_) => HelperResponse::ok(format!("接口 {interface} 的 DNS 已恢复")),
            Err(err) => HelperResponse::error(format!("恢复系统 DNS 失败：{err}")),
        }
    }

    pub fn apply_transparent_rules(port: u16) -> HelperResponse {
        if port == 0 {
            return HelperResponse::error("透明代理端口不能为 0");
        }
        if !command_available("nft") {
            return HelperResponse::unsupported(
                "未安装 nftables（nft 命令不可用），无法应用透明代理规则",
            );
        }
        // Replace instead of accumulating duplicates.
        let _ = Command::new("nft")
            .args(["delete", "table", "inet", "xross"])
            .output();
        let _ = Command::new("ip")
            .args(["rule", "del", "fwmark", FWMARK, "lookup", "100"])
            .output();
        let _ = Command::new("ip")
            .args(["route", "flush", "table", "100"])
            .output();
        let script = format!(
            r#"table inet xross {{
  set local_nets {{
    type ipv4_addr
    flags interval
    elements = {{ 0.0.0.0/8, 10.0.0.0/8, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4, 240.0.0.0/4 }}
  }}
  chain prerouting {{
    type filter hook prerouting priority mangle; policy accept;
    ip daddr @local_nets return
    meta l4proto {{ tcp, udp }} tproxy to :{port} meta mark set {FWMARK} accept
  }}
  chain output {{
    type route hook output priority mangle; policy accept;
    ip daddr @local_nets return
    meta l4proto {{ tcp, udp }} meta mark set {FWMARK} accept
  }}
}}
"#
        );
        if let Err(err) = run_stdin("nft", &["-f", "-"], &script) {
            return HelperResponse::error(format!("写入 nft 规则失败：{err}"));
        }
        if let Err(err) = run("ip", &["rule", "add", "fwmark", FWMARK, "lookup", "100"]) {
            let _ = clear_transparent_rules();
            return HelperResponse::error(format!("添加 ip rule 失败：{err}"));
        }
        if let Err(err) = run(
            "ip",
            &[
                "route",
                "add",
                "local",
                "0.0.0.0/0",
                "dev",
                "lo",
                "table",
                "100",
            ],
        ) {
            let _ = clear_transparent_rules();
            return HelperResponse::error(format!("添加本地路由失败：{err}"));
        }
        HelperResponse::ok(format!("透明代理规则已应用（tproxy :{port}）"))
    }

    pub fn clear_transparent_rules() -> HelperResponse {
        let _ = Command::new("nft")
            .args(["delete", "table", "inet", "xross"])
            .output();
        let _ = Command::new("ip")
            .args(["rule", "del", "fwmark", FWMARK, "lookup", "100"])
            .output();
        let _ = Command::new("ip")
            .args(["route", "flush", "table", "100"])
            .output();
        HelperResponse::ok("透明代理规则已清除")
    }

    fn parse_servers(servers: &[String]) -> std::result::Result<Vec<String>, String> {
        let mut out = Vec::new();
        for raw in servers {
            let value = raw.trim();
            match value.parse::<std::net::IpAddr>() {
                Ok(ip) => out.push(ip.to_string()),
                Err(_) => return Err(format!("无效的 DNS 地址：{value}")),
            }
        }
        if out.is_empty() {
            return Err("DNS 服务器列表不能为空".into());
        }
        Ok(out)
    }

    fn default_interface() -> Result<String> {
        let route = run("ip", &["route", "show", "default"])?;
        let words = route.split_whitespace().collect::<Vec<_>>();
        words
            .windows(2)
            .find(|pair| pair[0] == "dev")
            .map(|pair| pair[1].to_string())
            .ok_or_else(|| Error::other("找不到默认路由接口"))
    }

    fn command_available(program: &str) -> bool {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn run(program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| Error::other(format!("无法运行 {program}：{e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(Error::other(format!(
                "{program} 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    fn run_stdin(program: &str, args: &[&str], input: &str) -> Result<String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::other(format!("无法运行 {program}：{e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| Error::other(format!("写入 {program} 标准输入失败：{e}")))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| Error::other(format!("等待 {program} 失败：{e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(Error::other(format!(
                "{program} 失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::HelperResponse;
    use crate::error::{Error, Result};

    pub fn is_installed() -> bool {
        false
    }
    pub fn install() -> Result<String> {
        Err(Error::other("当前平台不支持提权助手"))
    }
    pub fn uninstall() -> Result<String> {
        Err(Error::other("当前平台不支持提权助手"))
    }
    pub fn start() -> Result<String> {
        Err(Error::other("当前平台不支持提权助手"))
    }
    pub fn run_elevated(_args: &[String]) -> Result<()> {
        Err(Error::other("当前平台不支持提权助手"))
    }
    pub fn prepare_tun(_interface: &str) -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
    pub fn remove_tun(_interface: &str) -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
    pub fn set_system_dns(_servers: &[String]) -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
    pub fn restore_system_dns() -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
    pub fn apply_transparent_rules(_port: u16) -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
    pub fn clear_transparent_rules() -> HelperResponse {
        HelperResponse::unsupported("当前平台不支持提权助手")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn scoped_pipe_accepts_and_authenticates_its_user() {
        use interprocess::local_socket::GenericNamespaced;
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        let name = format!("io.xross.helper-test.{}", uuid::Uuid::new_v4())
            .to_ns_name::<GenericNamespaced>()
            .unwrap();
        let listener = ListenerOptions::new()
            .name(name.clone())
            .security_descriptor(helper_security_descriptor().unwrap())
            .create_sync()
            .unwrap();
        let _client = Stream::connect(name).unwrap();
        let accepted = listener.accept().unwrap();
        assert!(authorize(&accepted));
    }

    #[cfg(unix)]
    #[test]
    fn scoped_socket_accepts_and_authenticates_its_user_and_pid() {
        use interprocess::local_socket::GenericFilePath;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.sock");
        let name = path.clone().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new()
            .name(name.clone())
            .try_overwrite(true)
            .create_sync()
            .unwrap();
        let _client = Stream::connect(name).unwrap();
        let accepted = listener.accept().unwrap();
        assert!(authorize(&accepted));
        assert_eq!(peer_pid(&accepted), Some(std::process::id()));
    }

    #[test]
    fn request_wire_format_is_stable() {
        let json = serde_json::to_string(&HelperRequest::PrepareTun {
            interface: "xross-tun".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"op":"prepare_tun","interface":"xross-tun"}"#);

        let parsed: HelperRequest =
            serde_json::from_str(r#"{"op":"set_system_dns","servers":["1.1.1.1"]}"#).unwrap();
        assert_eq!(
            parsed,
            HelperRequest::SetSystemDns {
                servers: vec!["1.1.1.1".into()]
            }
        );
    }

    #[test]
    fn responses_are_typed_and_stop_is_terminal() {
        let ok = HelperResponse::ok("done");
        assert!(ok.is_ok());
        assert_eq!(ok.message(), "done");
        assert!(!HelperResponse::unsupported("no").is_ok());
        let stop = HelperResponse::stop("bye");
        assert_eq!(stop.message(), "bye");
        assert!(!stop.is_ok());

        let json = serde_json::to_string(&stop).unwrap();
        assert_eq!(json, r#"{"status":"stop","message":"bye"}"#);
    }

    #[test]
    fn allow_uid_flag_is_parsed() {
        let args = vec![
            "--helper".to_string(),
            "--allow-uid".to_string(),
            "1000".to_string(),
        ];
        assert_eq!(parse_allow_uid(&args), Some(1000));
        assert_eq!(parse_allow_uid(&["--helper".to_string()]), None);
        assert_eq!(
            parse_allow_uid(&["--helper".into(), "--allow-uid".into(), "nope".into()]),
            None
        );
    }

    #[test]
    fn helper_status_labels_cover_every_state() {
        let base = HelperStatus {
            supported: true,
            installed: false,
            running: false,
            elevated: false,
            detail: String::new(),
            executable: None,
        };
        assert_eq!(base.label(), "未安装");
        assert_eq!(
            HelperStatus {
                installed: true,
                ..base.clone()
            }
            .label(),
            "已安装，未运行"
        );
        assert_eq!(
            HelperStatus {
                installed: true,
                running: true,
                ..base.clone()
            }
            .label(),
            "运行中"
        );
        assert_eq!(
            HelperStatus {
                supported: false,
                ..base
            }
            .label(),
            "当前平台不支持"
        );
    }
}
