//! Session-owned proxy cores for TUN mode, supervised by the privileged helper.
use crate::{Error, Result, core::process::SpawnSpec, paths::AppPaths};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub version: u32,
    pub owner_pid: u32,
    pub session: String,
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Command {
    Start { binary: PathBuf, config: PathBuf },
    Poll,
    Listeners,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    version: u32,
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listeners: Option<Vec<std::net::SocketAddr>>,
    exit_code: Option<u32>,
    lines: Vec<(bool, String)>,
}

struct Running {
    owner_pid: u32,
    child: std::process::Child,
    #[cfg(unix)]
    data_dir: PathBuf,
    tun_addresses: Vec<String>,
    lines: Arc<Mutex<std::collections::VecDeque<(bool, String)>>>,
    // Readers terminate when the core closes its pipes.
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
                fn chown(path: *const std::ffi::c_char, owner: u32, group: u32) -> i32;
            }
            let pid = self.child.id() as i32;
            if pid > 0 {
                unsafe {
                    kill(pid, 15);
                }
                let deadline = std::time::Instant::now() + Duration::from_millis(600);
                while std::time::Instant::now() < deadline {
                    if matches!(self.child.try_wait(), Ok(Some(_))) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(uid) = super::privilege::allowed_uid() {
                use std::os::unix::ffi::OsStrExt;
                let cache = self.data_dir.join("cache.db");
                if let Ok(c_path) = std::ffi::CString::new(cache.as_os_str().as_bytes()) {
                    unsafe {
                        chown(c_path.as_ptr(), uid, u32::MAX);
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

static RUNNING: OnceLock<Mutex<BTreeMap<String, Running>>> = OnceLock::new();

/// Addresses from configurations of cores launched by this helper.
pub fn active_tun_addresses() -> Vec<String> {
    RUNNING
        .get()
        .map(|map| {
            map.lock()
                .values()
                .flat_map(|r| r.tun_addresses.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn tun_addresses(config: &std::path::Path) -> Result<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(config).map_err(|_| Error::config("无法读取 TUN 配置"))?,
    )?;
    Ok(value["inbounds"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|i| {
            if i["type"] == "tun" {
                i["address"].as_array()
            } else if i["protocol"] == "tun" {
                i["settings"]["gateway"].as_array()
            } else {
                None
            }
            .into_iter()
            .flatten()
        })
        .filter_map(|v| {
            v.as_str()?
                .parse::<ipnet::IpNet>()
                .ok()
                .map(|n| n.addr().to_string())
        })
        .collect())
}

pub fn stop_all() {
    if let Some(running) = RUNNING.get() {
        running.lock().clear();
    }
}

fn resolve_paths(binary: &std::path::Path, config: &std::path::Path) -> AppPaths {
    let paths = AppPaths::discover();
    let binary_canon = binary.canonicalize().ok();
    let config_canon = config.canonicalize().ok();
    let Some(bin) = binary_canon.as_deref() else {
        return paths;
    };
    if crate::core::BinaryManager::new(&paths)
        .find_registered(bin)
        .is_some()
        && config_canon
            .as_deref()
            .and_then(|cfg| {
                paths
                    .runtime_dir
                    .canonicalize()
                    .ok()
                    .map(|rt| cfg.starts_with(rt))
            })
            .unwrap_or(false)
    {
        return paths;
    }
    let Some(cfg) = config_canon.as_deref() else {
        return paths;
    };
    #[cfg(unix)]
    let uid_ok = |uid: u32| {
        let allowed = super::privilege::allowed_uid();
        uid == 0 || Some(uid) == allowed
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !std::fs::metadata(cfg)
            .map(|m| uid_ok(m.uid()))
            .unwrap_or(false)
        {
            return paths;
        }
    }
    let mut candidates = Vec::new();
    for ancestor in cfg.ancestors().chain(bin.ancestors()) {
        candidates.push(ancestor.to_path_buf());
        candidates.push(ancestor.join("data"));
        candidates.push(ancestor.join(".local").join("share").join("xrose"));
        candidates.push(
            ancestor
                .join("Library")
                .join("Application Support")
                .join("io.xrose.Xrose"),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut uids = Vec::new();
        if let Some(uid) = super::privilege::allowed_uid() {
            uids.push(uid);
        }
        if let Ok(meta) = std::fs::metadata(cfg)
            && uid_ok(meta.uid())
            && !uids.contains(&meta.uid())
        {
            uids.push(meta.uid());
        }
        if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
            for line in passwd.lines() {
                let mut parts = line.split(':');
                let (Some(_name), Some(_pw), Some(uid_str), Some(_gid), Some(_gecos), Some(home)) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                if let Ok(uid) = uid_str.parse::<u32>()
                    && uids.contains(&uid)
                {
                    let home = std::path::PathBuf::from(home);
                    candidates.push(home.join(".local").join("share").join("xrose"));
                    candidates.push(
                        home.join("Library")
                            .join("Application Support")
                            .join("io.xrose.Xrose"),
                    );
                }
            }
        }
    }
    let mut fallback = None;
    for data_dir in candidates {
        if !data_dir.join("cores").join("manifest.json").is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !std::fs::metadata(&data_dir)
                .map(|m| uid_ok(m.uid()))
                .unwrap_or(false)
            {
                continue;
            }
        }
        let mut resolved = paths.clone();
        resolved.data_dir = data_dir.clone();
        resolved.config_dir = data_dir.clone();
        resolved.cache_dir = data_dir.join("cache");
        let data_run = data_dir.join("run");
        if cfg.starts_with(&data_run) {
            resolved.runtime_dir = data_run;
        } else if let Some(root_run) = data_dir.parent().map(|p| p.join("run"))
            && cfg.starts_with(&root_run)
        {
            resolved.runtime_dir = root_run;
        } else if let Some(parent) = cfg.parent() {
            #[cfg(unix)]
            let parent_ok = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(parent)
                    .map(|m| uid_ok(m.uid()))
                    .unwrap_or(false)
                    && parent
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == "xrose" || n.starts_with("xrose-"))
            };
            #[cfg(not(unix))]
            let parent_ok = parent
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "xrose" || n.starts_with("xrose-") || n == "run");
            if parent_ok {
                resolved.runtime_dir = parent.to_path_buf();
            }
        }
        if crate::core::BinaryManager::new(&resolved)
            .find_registered(bin)
            .is_some()
        {
            return resolved;
        }
        if fallback.is_none() {
            fallback = Some(resolved);
        }
    }
    fallback.unwrap_or(paths)
}

fn validate_start(
    paths: &AppPaths,
    binary: &std::path::Path,
    config: &std::path::Path,
) -> Result<crate::model::CoreKind> {
    let binary = binary
        .canonicalize()
        .map_err(|_| Error::binary("TUN 核心文件不存在"))?;
    let manager = crate::core::BinaryManager::new(paths);
    let record = manager.find_registered(&binary).ok_or_else(|| {
        Error::binary(
            "TUN 代启仅支持 Xrose 已安装并登记的内核，请选择已下载的版本，或先用 xrose-cli core install 登记此文件",
        )
    })?;
    if !manager.verify(&record)? {
        return Err(Error::binary("TUN 核心摘要不匹配，请重新安装内核"));
    }
    let runtime = paths
        .runtime_dir
        .canonicalize()
        .map_err(|_| Error::config("TUN 运行目录不存在"))?;
    let config = config
        .canonicalize()
        .map_err(|_| Error::config("TUN 配置文件不存在"))?;
    if !config.starts_with(runtime) {
        return Err(Error::config("TUN 配置必须位于 Xrose 运行目录"));
    }
    let bytes = std::fs::read(config).map_err(|_| Error::config("无法读取 TUN 配置"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| Error::config("TUN 配置不是有效 JSON"))?;
    if !value["inbounds"].as_array().is_some_and(|items| {
        items.iter().any(|i| match record.core {
            crate::model::CoreKind::SingBox => i["type"] == "tun",
            crate::model::CoreKind::Xray => i["protocol"] == "tun",
        })
    }) {
        return Err(Error::config("提权启动请求缺少 TUN 入站"));
    }
    Ok(record.core)
}

pub fn dispatch(request: &Request) -> Result<Reply> {
    #[cfg(windows)]
    use std::os::windows::{io::AsRawHandle, process::CommandExt};
    if request.version != VERSION || uuid::Uuid::parse_str(&request.session).is_err() {
        return Err(Error::other("TUN 助手协议不兼容，请重新安装提权助手"));
    }
    if !super::privilege::is_elevated() {
        return Err(Error::other("TUN 助手没有管理员权限，请重新安装助手"));
    }
    let owner = crate::mesh::platform::OwnerProcess::open(request.owner_pid)?;
    let running = RUNNING.get_or_init(Default::default);
    let mut map = running.lock();
    if let Some(child) = map.get(&request.session) {
        if child.owner_pid != request.owner_pid {
            return Err(Error::other("TUN 会话不属于当前调用者"));
        }
    }
    match &request.command {
        Command::Start { binary, config } => {
            if map.values().any(|r| r.owner_pid == request.owner_pid) {
                return Err(Error::other("该客户端已有 TUN 核心，请先停止"));
            }
            let paths = resolve_paths(binary, config);
            let core = validate_start(&paths, binary, config)?;
            let tun_addresses = tun_addresses(config)?;
            let mut command = std::process::Command::new(binary);
            command
                .args(crate::core::run_args(core, config))
                .current_dir(&paths.data_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            let mut child = command
                .spawn()
                .map_err(|_| Error::process("提权助手无法启动 TUN 核心"))?;
            #[cfg(windows)]
            if crate::core::job::adopt(child.as_raw_handle()).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::process("无法保护 TUN 核心生命周期，已取消启动"));
            }
            let lines = Arc::new(Mutex::new(std::collections::VecDeque::new()));
            let readers = vec![
                reader(child.stdout.take().unwrap(), false, lines.clone()),
                reader(child.stderr.take().unwrap(), true, lines.clone()),
            ];
            let pid = child.id();
            map.insert(
                request.session.clone(),
                Running {
                    owner_pid: request.owner_pid,
                    child,
                    #[cfg(unix)]
                    data_dir: paths.data_dir.clone(),
                    tun_addresses,
                    lines,
                    readers,
                },
            );
            monitor_owner(request.session.clone(), owner);
            Ok(Reply {
                version: VERSION,
                listeners: None,
                pid,
                exit_code: None,
                lines: vec![],
            })
        }
        Command::Poll => {
            let process = map
                .get_mut(&request.session)
                .ok_or_else(|| Error::process("TUN 助手会话已结束"))?;
            let status = process
                .child
                .try_wait()
                .map_err(|_| Error::process("无法读取 TUN 核心状态"))?;
            // Collect final output only after both pipe readers have reached EOF.
            let exit_code = status
                .filter(|_| process.readers.iter().all(|r| r.is_finished()))
                .map(|s| s.code().unwrap_or(1) as u32);
            let lines = process.lines.lock().drain(..).collect();
            Ok(Reply {
                version: VERSION,
                listeners: None,
                pid: process.child.id(),
                exit_code,
                lines,
            })
        }
        Command::Listeners => {
            let process = map
                .get_mut(&request.session)
                .ok_or_else(|| Error::process("TUN 助手会话已结束"))?;
            if process.child.try_wait()?.is_some() {
                return Err(Error::process("TUN 内核已退出"));
            }
            let pid = process.child.id();
            let listeners = super::port_owner::listeners(pid)
                .map_err(|_| Error::process("无法读取 TUN 内核监听端口"))?;
            Ok(Reply {
                version: VERSION,
                pid,
                listeners: Some(listeners),
                exit_code: None,
                lines: vec![],
            })
        }
        Command::Stop => {
            map.remove(&request.session);
            Ok(Reply {
                version: VERSION,
                listeners: None,
                pid: 0,
                exit_code: Some(1),
                lines: vec![],
            })
        }
    }
}

fn monitor_owner(
    session: String,
    owner: crate::mesh::platform::OwnerProcess,
) -> std::thread::JoinHandle<()> {
    // This monitor is independent of GUI polling and survives GUI crashes.
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let mut map = RUNNING.get_or_init(Default::default).lock();
            if !map.contains_key(&session) {
                break;
            }
            if !owner.alive() {
                map.remove(&session);
                break;
            }
        }
    })
}

fn reader(
    input: impl std::io::Read + Send + 'static,
    stderr: bool,
    lines: Arc<Mutex<std::collections::VecDeque<(bool, String)>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let mut input = std::io::BufReader::new(input);
        loop {
            let mut bytes = Vec::new();
            if input
                .by_ref()
                .take(4096)
                .read_until(b'\n', &mut bytes)
                .unwrap_or(0)
                == 0
            {
                break;
            }
            let mut lines = lines.lock();
            lines.push_back((stderr, String::from_utf8_lossy(&bytes).trim_end().into()));
            while lines.len() > 32 {
                lines.pop_front();
            }
        }
    })
}

fn call(request: Request) -> Result<Reply> {
    let response = super::privilege::call_with_timeout(
        &super::privilege::HelperRequest::TunCore {
            request: Box::new(request),
        },
        Duration::from_secs(20),
    )?;
    if !response.is_ok() {
        return Err(Error::process(format!(
            "TUN 助手：{}；若助手版本过旧，请重新安装助手",
            response.message()
        )));
    }
    let reply: Reply = serde_json::from_str(response.message())
        .map_err(|_| Error::process("TUN 助手响应无效，请重新安装助手"))?;
    if reply.version != VERSION {
        return Err(Error::process("TUN 助手版本不兼容，请重新安装助手"));
    }
    Ok(reply)
}

/// Query only the child of this authenticated session. Older helpers reject
/// the new command without affecting the running child or consuming its logs.
pub(crate) fn session_listeners(session: &str, pid: u32) -> Option<Vec<std::net::SocketAddr>> {
    let reply = call(Request {
        version: VERSION,
        owner_pid: std::process::id(),
        session: session.to_owned(),
        command: Command::Listeners,
    })
    .ok()?;
    (reply.pid == pid).then_some(reply.listeners).flatten()
}

pub struct Client {
    request: Request,
    pid: u32,
    logs: crate::core::LogBus,
    tail: Arc<Mutex<std::collections::VecDeque<String>>>,
    stopped: bool,
}

impl Client {
    pub async fn start(
        spec: &SpawnSpec,
        logs: crate::core::LogBus,
        tail: Arc<Mutex<std::collections::VecDeque<String>>>,
    ) -> Result<Self> {
        let request = Request {
            version: VERSION,
            owner_pid: std::process::id(),
            session: uuid::Uuid::new_v4().to_string(),
            command: Command::Start {
                binary: spec.program.clone(),
                config: spec
                    .config_path
                    .clone()
                    .ok_or_else(|| Error::process("缺少 TUN 配置路径"))?,
            },
        };
        let mut client = Self {
            request,
            pid: 0,
            logs,
            tail,
            stopped: false,
        };
        client.pid = client.send(client.request.command.clone()).await?.pid;
        Ok(client)
    }
    async fn send(&self, command: Command) -> Result<Reply> {
        let request = Request {
            command,
            ..self.request.clone()
        };
        tokio::task::spawn_blocking(move || call(request))
            .await
            .map_err(|_| Error::process("TUN 助手通信任务异常"))?
    }
    pub(crate) fn session(&self) -> &str {
        &self.request.session
    }
    pub fn id(&self) -> u32 {
        self.pid
    }
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if self.stopped {
            return Ok(exit_status_from_code(1));
        }
        loop {
            let reply = self
                .send(Command::Poll)
                .await
                .map_err(std::io::Error::other)?;
            for (stderr, line) in reply.lines {
                self.logs.core_line(
                    if stderr {
                        crate::core::LogStream::Stderr
                    } else {
                        crate::core::LogStream::Stdout
                    },
                    &line,
                );
                let mut tail = self.tail.lock();
                tail.push_back(line);
                while tail.len() > 40 {
                    tail.pop_front();
                }
            }
            if let Some(code) = reply.exit_code {
                return Ok(exit_status_from_code(code));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    pub async fn stop(&mut self) -> std::io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.send(Command::Stop)
            .await
            .map_err(std::io::Error::other)?;
        self.stopped = true;
        Ok(())
    }
}

fn exit_status_from_code(code: u32) -> std::process::ExitStatus {
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(((code & 0xff) as i32) << 8)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !self.stopped {
            let request = Request {
                command: Command::Stop,
                ..self.request.clone()
            };
            std::thread::spawn(move || {
                let _ = call(request);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn xray_tun_uses_registered_core_kind_and_tracks_gateway_addresses() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path());
        paths.ensure().unwrap();
        let binary = paths.data_dir.join("xray-fixture");
        std::fs::write(&binary, b"fake xray").unwrap();
        crate::core::BinaryManager::new(&paths)
            .adopt_existing(crate::model::CoreKind::Xray, "26.10.31", &binary)
            .unwrap();
        let config = paths.runtime_dir.join("xray-config.json");
        std::fs::write(&config, br#"{"inbounds":[{"protocol":"tun","settings":{"gateway":["172.19.0.1/30","fdfe:dcba:9876::1/126"]}}]}"#).unwrap();
        assert_eq!(
            validate_start(&paths, &binary, &config).unwrap(),
            crate::model::CoreKind::Xray
        );
        assert_eq!(
            tun_addresses(&config).unwrap(),
            vec!["172.19.0.1", "fdfe:dcba:9876::1"]
        );
        std::fs::write(
            &config,
            br#"{"inbounds":[{"type":"tun","address":["172.19.0.1/30"]}]}"#,
        )
        .unwrap();
        assert!(validate_start(&paths, &binary, &config).is_err());
        std::fs::write(&binary, b"tampered").unwrap();
        assert!(
            validate_start(&paths, &binary, &config)
                .unwrap_err()
                .to_string()
                .contains("摘要")
        );
    }

    use super::*;

    #[test]
    fn crashed_owner_reaps_its_core_without_client_polling() {
        let launch = || {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                std::process::Command::new("powershell.exe")
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        "Start-Sleep -Seconds 30",
                    ])
                    .creation_flags(0x08000000)
                    .spawn()
                    .unwrap()
            }
            #[cfg(unix)]
            {
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap()
            }
        };
        let mut owner_child = launch();
        let owner = crate::mesh::platform::OwnerProcess::open(owner_child.id()).unwrap();
        let child = launch();
        let core = crate::mesh::platform::OwnerProcess::open(child.id()).unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        RUNNING.get_or_init(Default::default).lock().insert(
            session.clone(),
            Running {
                tun_addresses: vec![],
                owner_pid: owner_child.id(),
                child,
                #[cfg(unix)]
                data_dir: std::env::temp_dir(),
                lines: Default::default(),
                readers: vec![],
            },
        );
        let monitor = monitor_owner(session.clone(), owner);
        owner_child.kill().unwrap();
        owner_child.wait().unwrap();
        monitor.join().unwrap();
        assert!(!core.alive());
        assert!(!RUNNING.get().unwrap().lock().contains_key(&session));
    }
    #[test]
    fn rejects_unregistered_binary_and_foreign_config_before_spawning() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path());
        paths.ensure().unwrap();
        let binary = paths.data_dir.join("sing-box.exe");
        std::fs::write(&binary, b"fake core").unwrap();
        let config = paths.runtime_dir.join("test.json");
        std::fs::write(&config, br#"{"inbounds":[{"type":"tun"}]}"#).unwrap();
        assert!(
            validate_start(&paths, &binary, &config)
                .unwrap_err()
                .to_string()
                .contains("已安装")
        );
        crate::core::BinaryManager::new(&paths)
            .adopt_existing(crate::model::CoreKind::SingBox, "1.14.4", &binary)
            .unwrap();
        assert!(validate_start(&paths, &binary, &config).is_ok());
        let foreign = root.path().join("foreign.json");
        std::fs::copy(&config, &foreign).unwrap();
        assert!(validate_start(&paths, &binary, &foreign).is_err());
        std::fs::write(&config, br#"{"inbounds":[]}"#).unwrap();
        assert!(
            validate_start(&paths, &binary, &config)
                .unwrap_err()
                .to_string()
                .contains("TUN 入站")
        );
        std::fs::write(&binary, b"tampered").unwrap();
        assert!(
            validate_start(&paths, &binary, &config)
                .unwrap_err()
                .to_string()
                .contains("摘要")
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_user_data_and_runtime_dirs_from_registered_binary_and_config() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path());
        paths.ensure().unwrap();
        let bin_dir = paths.cores_dir().join("sing-box").join("1.14.4");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let binary = bin_dir.join("sing-box");
        std::fs::write(&binary, b"fake core").unwrap();
        let config = paths.runtime_dir.join("sing-box-config.json");
        std::fs::write(&config, br#"{"inbounds":[{"type":"tun"}]}"#).unwrap();
        crate::core::BinaryManager::new(&paths)
            .adopt_existing(crate::model::CoreKind::SingBox, "1.14.4", &binary)
            .unwrap();
        let resolved = resolve_paths(&binary, &config);
        assert!(validate_start(&resolved, &binary, &config).is_ok());

        let external_dir = tempfile::tempdir().unwrap();
        let ext = std::env::consts::EXE_SUFFIX;
        let external_bin = external_dir.path().join(format!("sing-box{ext}"));
        std::fs::write(&external_bin, b"custom local core").unwrap();
        crate::util::set_executable(&external_bin).unwrap();
        crate::core::BinaryManager::new(&paths)
            .register_local(crate::model::CoreKind::SingBox, &external_bin)
            .unwrap();
        let resolved_external = resolve_paths(&external_bin, &config);
        assert!(validate_start(&resolved_external, &external_bin, &config).is_ok());
    }
}
