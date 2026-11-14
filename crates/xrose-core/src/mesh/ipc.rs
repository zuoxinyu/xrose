//! Versioned helper messages and session-owned worker supervision.
use super::{MeshPhase, MeshProfile, MeshProvider, MeshRuntimeView, MeshSecret, PROTOCOL_VERSION};
use crate::{Error, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Once, OnceLock},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshRequest {
    pub version: u32,
    pub owner_pid: u32,
    pub session: MeshSecret,
    pub command: MeshCommand,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MeshCommand {
    Start {
        profile: MeshProfile,
        tailscale_binary: Option<PathBuf>,
        driver_archive: PathBuf,
        #[serde(default)]
        sync_port: u16,
        #[serde(default)]
        sync_password: MeshSecret,
        #[serde(default)]
        exit_bridge: Option<super::ExitBridge>,
    },
    Inspect,
    ProbeProfile {
        profile_id: String,
        ip: String,
    },
    DisconnectProfile {
        profile_id: String,
        logout: bool,
    },
    Status,
    Stop {
        provider: MeshProvider,
    },
    Logout {
        provider: MeshProvider,
    },
    SetForwards {
        profile_id: String,
        id: String,
        rules: super::ForwardRules,
    },
    Probe {
        provider: MeshProvider,
        ip: String,
    },
    StopAll,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct MeshReply {
    pub version: u32,
    pub networks: Vec<MeshRuntimeView>,
    /// Private runtime metadata; never copy into the normal mesh snapshot.
    #[serde(default)]
    pub helper_executable: Option<String>,
}
impl std::fmt::Debug for MeshReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshReply")
            .field("version", &self.version)
            .field("networks", &self.networks)
            .field(
                "helper_executable",
                &self.helper_executable.as_ref().map(|_| "[PRIVATE]"),
            )
            .finish()
    }
}
fn helper_executable() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    path.is_absolute()
        .then(|| path.to_str().map(str::to_owned))
        .flatten()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    #[serde(default)]
    pub exit_bridge: Option<super::ExitBridge>,
    #[serde(default)]
    pub sync_port: u16,
    pub sync_password: MeshSecret,
    pub profile: MeshProfile,
    pub directory: PathBuf,
    pub tailscale_binary: Option<PathBuf>,
    pub baseline: super::platform::NetworkBaseline,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkerCommand {
    Stop,
    Logout,
    Probe {
        ip: String,
    },
    SetForwards {
        id: String,
        rules: super::ForwardRules,
    },
}

struct Worker {
    child: Mutex<Child>,
    input: std::sync::mpsc::SyncSender<WorkerCommand>,
    state: Mutex<MeshRuntimeView>,
    directory: PathBuf,
    owner: super::platform::OwnerProcess,
    stopped: Once,
    user_stopped: std::sync::atomic::AtomicBool,
}
impl Worker {
    fn probe(&self, ip: &str) -> Result<()> {
        let address = ip
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::config("探测目标必须是组网设备 IP"))?;
        if !self.state.lock().peers.iter().any(|p| {
            p.addresses
                .iter()
                .any(|a| a.parse::<std::net::IpAddr>().ok() == Some(address))
        }) {
            return Err(Error::config("只能探测当前组网设备列表中的地址"));
        }
        self.state.lock().probe = None;
        self.send(WorkerCommand::Probe { ip: ip.into() })
    }
    fn send(&self, value: WorkerCommand) -> Result<()> {
        self.input
            .try_send(value)
            .map_err(|_| Error::other("组网工作进程繁忙或已退出，请稍后重试"))
    }
    fn stop(&self) {
        self.stopped.call_once(|| self.stop_child());
    }
    fn stop_child(&self) {
        let _ = self.send(WorkerCommand::Stop);
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if self.child.lock().try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut child = self.child.lock();
        #[cfg(unix)]
        super::platform::kill_worker_group(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let _ = super::platform::cleanup_dns(&self.directory);
        let mut state = self.state.lock();
        let error = if state.phase == MeshPhase::Failed {
            state.error.take()
        } else {
            None
        };
        state.finish(error);
    }
}
struct Session {
    pid: u32,
    workers: BTreeMap<MeshProvider, Arc<Worker>>,
}
impl Session {
    fn stop_all(&mut self) {
        std::thread::scope(|scope| {
            for (_, worker) in std::mem::take(&mut self.workers) {
                scope.spawn(move || worker.stop());
            }
        });
    }
}
static SESSIONS: OnceLock<Mutex<BTreeMap<String, Session>>> = OnceLock::new();

pub fn dispatch(request: &MeshRequest) -> Result<MeshReply> {
    if request.version != PROTOCOL_VERSION {
        return Err(Error::other("组网助手协议不兼容，请在设置中更新提权助手"));
    }
    if request.session.0.len() != 64 || !request.session.0.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::other("组网会话凭证无效"));
    }
    let owner = super::platform::OwnerProcess::open(request.owner_pid)?;
    let mut sessions = SESSIONS.get_or_init(Default::default).lock();
    if let MeshCommand::ProbeProfile { profile_id, ip } = &request.command {
        let worker = sessions
            .values()
            .flat_map(|s| s.workers.values())
            .find(|w| {
                !w.user_stopped.load(std::sync::atomic::Ordering::Acquire)
                    && w.state.lock().profile_id == *profile_id
            })
            .ok_or_else(|| Error::other("组网未连接"))?;
        worker.probe(ip)?;
        let mut state = worker.state.lock().clone();
        state.login_url = None;
        return Ok(MeshReply {
            version: PROTOCOL_VERSION,
            networks: vec![state],
            helper_executable: helper_executable(),
        });
    }
    if matches!(request.command, MeshCommand::Inspect) {
        return Ok(MeshReply {
            version: PROTOCOL_VERSION,
            helper_executable: helper_executable(),
            networks: sessions
                .values()
                .flat_map(|s| s.workers.values())
                .filter(|w| !w.user_stopped.load(std::sync::atomic::Ordering::Acquire))
                .map(|w| {
                    let mut s = w.state.lock().clone();
                    s.login_url = None;
                    s
                })
                .collect(),
        });
    }
    if let MeshCommand::DisconnectProfile { profile_id, logout } = &request.command {
        for session in sessions.values_mut() {
            let provider = session
                .workers
                .iter()
                .find(|(_, w)| {
                    !w.user_stopped.load(std::sync::atomic::Ordering::Acquire)
                        && w.state.lock().profile_id == *profile_id
                })
                .map(|(p, _)| *p);
            if let Some(provider) = provider {
                let worker = &session.workers[&provider];
                if *logout {
                    worker.send(WorkerCommand::Logout)?;
                } else {
                    worker.stop();
                }
                worker
                    .user_stopped
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        return Ok(MeshReply {
            version: PROTOCOL_VERSION,
            networks: Vec::new(),
            helper_executable: helper_executable(),
        });
    }
    if let MeshCommand::Start { profile, .. } = &request.command {
        if sessions
            .values()
            .flat_map(|s| s.workers.iter())
            .any(|(p, w)| {
                *p == profile.provider && w.child.lock().try_wait().ok().flatten().is_none()
            })
        {
            return Err(Error::other(
                "该组网后端已在另一个 Xrose 会话运行，请先断开",
            ));
        }
    }
    sessions.retain(|_, s| s.workers.values().any(|w| w.owner.alive()));
    let other_routes: Vec<String> = sessions
        .values()
        .flat_map(|s| s.workers.values())
        .filter(|w| w.child.lock().try_wait().ok().flatten().is_none())
        .flat_map(|w| w.state.lock().routes.clone())
        .collect();
    let session = sessions
        .entry(request.session.0.clone())
        .or_insert_with(|| Session {
            pid: request.owner_pid,
            workers: BTreeMap::new(),
        });
    if session.pid != request.owner_pid {
        return Err(Error::other("组网会话不属于调用进程"));
    }
    match &request.command {
        MeshCommand::Start {
            profile,
            tailscale_binary,
            driver_archive,
            sync_port,
            sync_password,
            exit_bridge,
        } => {
            profile.validate()?;
            if profile.advertise_exit_node && profile.exit_routing == super::ExitRoutingMode::Rules
            {
                exit_bridge
                    .as_ref()
                    .ok_or_else(|| Error::config("出口分流服务未就绪，请重新连接组网"))?
                    .validate()?;
            }
            if session
                .workers
                .get(&profile.provider)
                .is_some_and(|w| w.child.lock().try_wait().ok().flatten().is_none())
            {
                return Err(Error::other("该组网后端已经运行，请先断开"));
            }
            #[cfg(windows)]
            let baseline = super::platform::baseline_with_proxy_tun(
                &crate::system::tun_process::active_tun_addresses(),
            )?;
            #[cfg(not(windows))]
            let baseline = super::platform::baseline()?;
            let mut wanted = profile.remote_subnets.clone();
            if !profile.ipv4.is_empty() {
                wanted.push(profile.ipv4.clone());
            }
            if profile.provider == MeshProvider::Tailscale {
                wanted.extend([
                    "100.64.0.0/10".into(),
                    "fd7a:115c:a1e0::/48".into(),
                    super::TAILSCALE_TUN_NETWORK.into(),
                    super::TAILSCALE_TUN_IPV6_NETWORK.into(),
                ]);
            }
            let mut protected = baseline.protected.clone();
            protected.extend(other_routes);
            super::coexist::check_startup_networks(profile.provider, &wanted, &protected)?;
            let directory = crate::paths::AppPaths::discover()
                .data_dir
                .join("mesh-state")
                .join(&profile.id);
            super::platform::secure_directory(&directory)?;
            super::platform::cleanup_dns(&directory)?;
            #[cfg(windows)]
            super::driver::unpack(driver_archive, &directory)?;
            #[cfg(not(windows))]
            let _ = driver_archive;
            if profile.provider == MeshProvider::Tailscale {
                super::tailscale::verify_binary(
                    tailscale_binary
                        .as_deref()
                        .ok_or_else(|| Error::other("请先安装 Tailscale 组网组件"))?,
                )?;
            }
            let mut command = Command::new(
                std::env::current_exe().map_err(|_| Error::other("无法定位 Xrose 工作进程"))?,
            );
            command
                .arg("--mesh-worker")
                .current_dir(&directory)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .spawn()
                .map_err(|_| Error::other("启动组网工作进程失败"))?;
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                if crate::core::job::adopt(child.as_raw_handle()).is_err() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Error::other("无法保护组网进程生命周期，已取消启动"));
                }
            }
            let mut input = child
                .stdin
                .take()
                .ok_or_else(|| Error::other("组网 IPC 不可用"))?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| Error::other("组网 IPC 不可用"))?;
            let bootstrap = Bootstrap {
                exit_bridge: exit_bridge.clone(),
                sync_port: *sync_port,
                sync_password: sync_password.clone(),
                profile: profile.clone(),
                directory: directory.clone(),
                tailscale_binary: tailscale_binary.clone(),
                baseline,
            };
            let wire = serde_json::to_vec(&bootstrap)?;
            if input
                .write_all(&wire)
                .and_then(|_| input.write_all(b"\n"))
                .and_then(|_| input.flush())
                .is_err()
            {
                let _ = child.kill();
                return Err(Error::other("发送组网启动配置失败"));
            }
            let worker = Arc::new(Worker {
                stopped: Once::new(),
                user_stopped: Default::default(),
                child: Mutex::new(child),
                input: command_pipe(input),
                state: Mutex::new(MeshRuntimeView {
                    profile_id: profile.id.clone(),
                    provider: profile.provider,
                    phase: MeshPhase::Starting,
                    ..Default::default()
                }),
                directory,
                owner,
            });
            let reader = worker.clone();
            std::thread::spawn(move || {
                let mut output = BufReader::new(output);
                loop {
                    let mut line = String::new();
                    if output
                        .by_ref()
                        .take(1_048_577)
                        .read_line(&mut line)
                        .unwrap_or(0)
                        == 0
                    {
                        break;
                    }
                    if line.len() > 1_048_576 {
                        break;
                    }
                    if let Some(json) = line.strip_prefix("XROSE_MESH:") {
                        if let Ok(mut state) = serde_json::from_str::<MeshRuntimeView>(json) {
                            state.peers.truncate(1024);
                            *reader.state.lock() = state;
                        }
                    }
                }
            });
            let monitor = worker.clone();
            std::thread::spawn(move || {
                while monitor.owner.alive() {
                    if monitor.child.lock().try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                monitor.stopped.call_once(|| {
                    let expected = matches!(
                        monitor.state.lock().phase,
                        MeshPhase::Disconnected | MeshPhase::Stopping
                    );
                    monitor.stop_child();
                    if !expected {
                        let mut s = monitor.state.lock();
                        s.phase = MeshPhase::Failed;
                        if s.error.is_none() {
                            s.error = Some("组网工作进程已退出".into());
                        }
                    }
                });
            });
            session.workers.insert(profile.provider, worker);
        }
        MeshCommand::Status => {}
        MeshCommand::Inspect
        | MeshCommand::DisconnectProfile { .. }
        | MeshCommand::ProbeProfile { .. } => unreachable!(),
        MeshCommand::Stop { provider } => {
            if let Some(w) = session.workers.remove(provider) {
                w.stop();
            }
        }
        MeshCommand::StopAll => session.stop_all(),
        MeshCommand::Logout { provider } => {
            if let Some(w) = session.workers.get(provider) {
                w.send(WorkerCommand::Logout)?;
            } else {
                return Err(Error::other("请先连接后再注销"));
            }
        }
        MeshCommand::SetForwards {
            profile_id,
            id,
            rules,
        } => {
            if (rules.local.len() > 64 || rules.services.len() > 64)
                || uuid::Uuid::parse_str(id).is_err()
            {
                return Err(Error::config("端口转发请求无效"));
            }
            let worker = session
                .workers
                .values()
                .find(|w| w.state.lock().profile_id == *profile_id)
                .ok_or_else(|| Error::other("组网连接已变更，请重试"))?;
            worker.send(WorkerCommand::SetForwards {
                id: id.clone(),
                rules: rules.clone(),
            })?;
        }
        MeshCommand::Probe { provider, ip } => {
            let w = session
                .workers
                .get(provider)
                .ok_or_else(|| Error::other("组网未连接"))?;
            w.probe(ip)?;
        }
    }
    Ok(MeshReply {
        version: PROTOCOL_VERSION,
        helper_executable: helper_executable(),
        networks: session
            .workers
            .values()
            .map(|w| {
                let mut state = w.state.lock().clone();
                state.user_stopped = w.user_stopped.load(std::sync::atomic::Ordering::Acquire);
                state
            })
            .collect(),
    })
}

fn command_pipe(mut input: ChildStdin) -> std::sync::mpsc::SyncSender<WorkerCommand> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<WorkerCommand>(8);
    std::thread::spawn(move || {
        while let Ok(command) = receiver.recv() {
            let Ok(wire) = serde_json::to_vec(&command) else {
                break;
            };
            if input
                .write_all(&wire)
                .and_then(|_| input.write_all(b"\n"))
                .and_then(|_| input.flush())
                .is_err()
            {
                break;
            }
        }
    });
    sender
}

pub fn call(request: MeshRequest) -> Result<MeshReply> {
    use crate::system::privilege::{self, HelperRequest, HelperResponse};
    match privilege::call_with_timeout(
        &HelperRequest::Mesh {
            request: Box::new(request),
        },
        Duration::from_secs(20),
    )? {
        HelperResponse::Ok { message } => {
            serde_json::from_str(&message).map_err(|_| Error::other("组网助手响应无效，请更新助手"))
        }
        other => Err(Error::other(other.message().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_path_is_optional_private_runtime_metadata() {
        let legacy: MeshReply = serde_json::from_value(serde_json::json!({
            "version": PROTOCOL_VERSION,
            "networks": []
        }))
        .unwrap();
        assert!(legacy.helper_executable.is_none());
        let reply = MeshReply {
            version: PROTOCOL_VERSION,
            networks: Vec::new(),
            helper_executable: Some("/fixture/private-helper".into()),
        };
        assert!(!format!("{reply:?}").contains("/fixture/private-helper"));
        let wire = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            serde_json::from_slice::<MeshReply>(&wire)
                .unwrap()
                .helper_executable,
            reply.helper_executable
        );
        assert!(std::path::Path::new(&helper_executable().unwrap()).is_absolute());
    }

    #[test]
    fn incompatible_or_uncredentialed_request_never_starts_a_worker() {
        let mut request = MeshRequest {
            version: PROTOCOL_VERSION + 1,
            owner_pid: 0,
            session: MeshSecret("fake-key".into()),
            command: MeshCommand::Status,
        };
        assert!(dispatch(&request).unwrap_err().to_string().contains("协议"));
        request.version = PROTOCOL_VERSION;
        assert!(dispatch(&request).unwrap_err().to_string().contains("凭证"));
        assert!(!format!("{request:?}").contains("fake-key"));
    }

    fn test_worker(
        directory: &std::path::Path,
        provider: MeshProvider,
        script: &str,
    ) -> Arc<Worker> {
        #[cfg(unix)]
        let mut command = {
            use std::os::unix::process::CommandExt;
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script]).process_group(0);
            cmd
        };
        #[cfg(windows)]
        let mut command = {
            use std::os::windows::process::CommandExt;
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
                .creation_flags(0x08000000);
            cmd
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = command_pipe(child.stdin.take().unwrap());
        Arc::new(Worker {
            child: Mutex::new(child),
            input,
            state: Mutex::new(MeshRuntimeView {
                provider,
                phase: MeshPhase::Connected,
                ..Default::default()
            }),
            directory: directory.to_path_buf(),
            owner: super::super::platform::OwnerProcess::open(std::process::id()).unwrap(),
            stopped: Once::new(),
            user_stopped: Default::default(),
        })
    }

    fn stop_two_workers(script: &str, graceful: bool) -> Duration {
        let dir = tempfile::tempdir().unwrap();
        let workers = [MeshProvider::EasyTier, MeshProvider::Tailscale].map(|provider| {
            (
                provider,
                test_worker(&dir.path().join(format!("{provider:?}")), provider, script),
            )
        });
        let mut session = Session {
            pid: std::process::id(),
            workers: workers.iter().cloned().collect(),
        };
        let started = Instant::now();
        session.stop_all();
        let elapsed = started.elapsed();
        assert!(session.workers.is_empty());
        for (_, worker) in workers {
            let status = worker
                .child
                .lock()
                .try_wait()
                .unwrap()
                .expect("worker reaped");
            assert_eq!(status.success(), graceful);
            worker.stop();
            assert_eq!(worker.state.lock().phase, MeshPhase::Disconnected);
        }
        elapsed
    }

    #[test]
    fn stop_all_reaps_two_stuck_workers_within_one_grace_period() {
        #[cfg(unix)]
        let script = "sleep 30 & wait";
        #[cfg(windows)]
        let script = "Start-Sleep -Seconds 30";
        let elapsed = stop_two_workers(script, false);
        assert!(
            elapsed < Duration::from_secs(7),
            "stop_all took {elapsed:?}"
        );
    }

    #[test]
    fn stop_all_reaps_responsive_workers_gracefully() {
        #[cfg(unix)]
        let script = r#"IFS= read -r cmd && [ "$cmd" = '{"action":"stop"}' ]"#;
        #[cfg(windows)]
        let script = r#"if ([Console]::ReadLine() -ne '{"action":"stop"}') { exit 1 }"#;
        stop_two_workers(script, true);
    }

    #[cfg(windows)]
    #[test]
    fn owner_handle_detects_exit_without_pid_reuse() {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let owner = super::super::platform::OwnerProcess::open(child.id()).unwrap();
        assert!(owner.alive());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!owner.alive());
    }
}
