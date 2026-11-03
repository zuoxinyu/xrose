//! Pinned sing-box mesh runtime and its typed, authenticated gRPC API.
use super::{MeshPeer, MeshPhase, MeshProfile, MeshProvider, MeshRuntimeView};
use crate::{Error, Result};
use parking_lot::Mutex;
use prost::Message;
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tonic::{client::Grpc, transport::Channel, Request};
use tonic_prost::ProstCodec;

fn release() -> Result<(&'static str, &'static str)> {
    if !cfg!(windows) {
        return Err(Error::binary("Tailscale 组网组件当前仅在 Windows 启用"));
    }
    match std::env::consts::ARCH {
        "x86_64" => Ok((
            "sing-box-1.14.4-windows-amd64.zip",
            "99185d75d314a07dbba4a130c8ad986cbeb629a07927975636b341ca61f991df",
        )),
        "aarch64" => Ok((
            "sing-box-1.14.4-windows-arm64.zip",
            "8cd0a4d0cd8cd480be5731ac31f17038969a42d12add9feb6e031fd43d85bbae",
        )),
        _ => Err(Error::binary(
            "Tailscale 组网组件仅提供 Windows x64/ARM64 构建",
        )),
    }
}
pub fn archive_path(paths: &crate::paths::AppPaths) -> PathBuf {
    paths
        .data_dir
        .join("mesh-binaries")
        .join("sing-box-1.14.4.zip")
}
fn verified_bytes(path: &Path) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).map_err(|_| Error::binary("Tailscale 组网组件尚未安装"))?;
    if hex::encode(Sha256::digest(&data)) != release()?.1 {
        return Err(Error::binary("Tailscale 组件摘要不匹配，请重新下载"));
    }
    Ok(data)
}
pub fn verify_binary(path: &Path) -> Result<()> {
    verified_bytes(path).map(|_| ())
}
pub async fn install(paths: &crate::paths::AppPaths) -> Result<()> {
    let (asset, hash) = release()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let response = client
        .get(format!(
            "https://github.com/SagerNet/sing-box/releases/download/v1.14.4/{asset}"
        ))
        .send()
        .await?
        .error_for_status()?;
    let data = response.bytes().await?;
    use sha2::{Digest, Sha256};
    if hex::encode(Sha256::digest(&data)) != hash {
        return Err(Error::binary("下载的组网组件未通过 SHA-256 校验"));
    }
    let target = archive_path(paths);
    std::fs::create_dir_all(target.parent().unwrap())
        .map_err(|_| Error::binary("无法创建组网组件目录"))?;
    crate::util::atomic_write(&target, &data)
}
fn unpack(archive: &Path, directory: &Path) -> Result<PathBuf> {
    let data = verified_bytes(archive)?;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))
        .map_err(|_| Error::binary("组网组件压缩包无效"))?;
    let executable = directory.join("sing-box.exe");
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|_| Error::binary("读取组网组件失败"))?;
        if f.name()
            .map(|name| name.ends_with("/sing-box.exe"))
            .unwrap_or(false)
        {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut bytes)
                .map_err(|_| Error::binary("解压组网组件失败"))?;
            crate::util::atomic_write(&executable, &bytes)?;
            return Ok(executable);
        }
    }
    Err(Error::binary("组网组件缺少可执行文件"))
}

/// Syntax validation only: check never starts an endpoint or interface.
pub async fn check_configuration(paths: &crate::paths::AppPaths) -> Result<()> {
    let directory = paths.runtime_dir.join("mesh-check");
    std::fs::create_dir_all(&directory).map_err(|_| Error::other("无法创建配置校验目录"))?;
    let binary = unpack(&archive_path(paths), &directory)?;
    super::driver::unpack(&super::driver::archive_path(paths), &directory)?;
    let profile = MeshProfile {
        provider: MeshProvider::Tailscale,
        ..Default::default()
    };
    let config =
        crate::adapters::singbox::compile_mesh(&profile, &directory, 19090, "offline-test-secret")?;
    let path = directory.join("config.json");
    crate::util::atomic_write_json(&path, &config)?;
    let report = crate::adapters::validate(crate::model::CoreKind::SingBox, &binary, &path).await?;
    if !report.ok {
        return Err(Error::config(format!(
            "组网配置校验失败：{}",
            report.summary()
        )));
    }
    // Validate the proxy side of coexistence too, without starting a TUN.
    use crate::{
        adapters::CompileContext,
        model::{AppModel, CoreKind, InboundKind, Node, Protocol},
        settings::Settings,
    };
    let mut model = AppModel::default();
    model.add_nodes(vec![Node::new(
        "offline-test",
        Protocol::Socks,
        "127.0.0.1",
        19081,
    )]);
    let tun = model
        .inbounds
        .iter_mut()
        .find(|i| i.kind == InboundKind::Tun)
        .expect("default TUN");
    tun.enabled = true;
    tun.tun.strict_route = true;
    let settings = Settings::default();
    let mut ctx = CompileContext::new(&settings, paths.data_dir.clone(), directory.clone());
    ctx.mesh = super::coexist::CoexistencePlan {
        active: true,
        routes: vec![super::coexist::MeshRoute {
            cidr: "100.64.0.0/10".into(),
            interface: "xross-mesh-offline-test".into(),
        }],
        dns: vec![super::coexist::MeshDns {
            suffix: "offline.ts.net".into(),
            address: "127.0.0.153".into(),
        }],
    };
    let compiled =
        crate::adapters::adapter_for(CoreKind::SingBox).compile(&model, &settings, &ctx)?;
    let proxy_path = directory.join("proxy-coexistence.json");
    crate::util::atomic_write_json(&proxy_path, &compiled.json)?;
    let report = crate::adapters::validate(CoreKind::SingBox, &binary, &proxy_path).await?;
    if !report.ok {
        return Err(Error::config(format!(
            "代理共存配置校验失败：{}",
            report.summary()
        )));
    }
    Ok(())
}

/// Real gRPC contract check without TUN, DNS listeners or OS changes.
pub async fn check_api(paths: &crate::paths::AppPaths) -> Result<()> {
    let directory = paths.runtime_dir.join("mesh-api-check");
    super::platform::secure_directory(&directory)?;
    let profile = MeshProfile {
        provider: MeshProvider::Tailscale,
        hostname: "xross-api-check".into(),
        ..Default::default()
    };
    let backend = Backend::start_inner(profile, &directory, &archive_path(paths), false).await?;
    let denied = Client {
        channel: backend.client.channel.clone(),
        secret: "invalid-test-credential".into(),
    };
    if denied.verify_version().await.is_ok() {
        return Err(Error::other("组网 API 未拒绝无效凭证，已停止验证"));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if backend.state.lock().updated_at > 0 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(Error::other("组网 API 状态订阅超时"))
}

// Wire field numbers are pinned to sing-box v1.14.4 daemon/started_service.proto.
// Unknown fields are intentionally ignored for forward-compatible reads.
#[derive(Clone, PartialEq, Message)]
struct Empty {}
#[derive(Clone, PartialEq, Message)]
struct Version {
    #[prost(string, tag = "1")]
    version: String,
    #[prost(int32, tag = "2")]
    api_version: i32,
}
#[derive(Clone, PartialEq, Message)]
struct StatusUpdate {
    #[prost(message, repeated, tag = "1")]
    endpoints: Vec<EndpointStatus>,
}
#[derive(Clone, PartialEq, Message)]
struct EndpointStatus {
    #[prost(string, tag = "1")]
    tag: String,
    #[prost(string, tag = "2")]
    state: String,
    #[prost(string, tag = "4")]
    auth_url: String,
    #[prost(string, tag = "6")]
    suffix: String,
    #[prost(message, optional, tag = "7")]
    self_peer: Option<Peer>,
    #[prost(message, repeated, tag = "8")]
    groups: Vec<Group>,
}
#[derive(Clone, PartialEq, Message)]
struct Group {
    #[prost(message, repeated, tag = "5")]
    peers: Vec<Peer>,
}
#[derive(Clone, PartialEq, Message)]
struct Peer {
    #[prost(string, tag = "1")]
    hostname: String,
    #[prost(string, tag = "2")]
    dns_name: String,
    #[prost(string, repeated, tag = "4")]
    ips: Vec<String>,
    #[prost(bool, tag = "5")]
    online: bool,
    #[prost(string, tag = "12")]
    id: String,
}
#[derive(Clone, PartialEq, Message)]
struct EndpointRequest {
    #[prost(string, tag = "1")]
    tag: String,
}
#[derive(Clone, PartialEq, Message)]
struct PingRequest {
    #[prost(string, tag = "1")]
    tag: String,
    #[prost(string, tag = "2")]
    ip: String,
}
#[derive(Clone, PartialEq, Message)]
struct PingResponse {
    #[prost(double, tag = "1")]
    latency: f64,
    #[prost(bool, tag = "2")]
    direct: bool,
    #[prost(string, tag = "6")]
    error: String,
}
#[derive(Clone)]
struct Client {
    channel: Channel,
    secret: String,
}
impl Client {
    async fn verify_version(&self) -> Result<()> {
        let mut g = Grpc::new(self.channel.clone());
        g.ready()
            .await
            .map_err(|_| Error::other("组网 API 不可用"))?;
        let response: tonic::Response<Version> = g
            .unary(
                self.request(Empty {})?,
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/daemon.StartedService/GetVersion",
                ),
                ProstCodec::default(),
            )
            .await
            .map_err(|_| Error::other("组网 API 版本协商失败"))?;
        if response.get_ref().version != super::TAILSCALE_CORE_VERSION {
            return Err(Error::other("Tailscale 组件版本不匹配，请重新安装组件"));
        }
        Ok(())
    }
    fn request<T>(&self, message: T) -> Result<Request<T>> {
        let mut r = Request::new(message);
        r.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.secret)
                .parse()
                .map_err(|_| Error::other("组网 API 凭证无效"))?,
        );
        Ok(r)
    }
    async fn status(&self) -> Result<tonic::Streaming<StatusUpdate>> {
        let mut g = Grpc::new(self.channel.clone());
        g.ready()
            .await
            .map_err(|_| Error::other("组网 API 尚未就绪"))?;
        g.server_streaming(
            self.request(Empty {})?,
            tonic::codegen::http::uri::PathAndQuery::from_static(
                "/daemon.StartedService/SubscribeTailscaleStatus",
            ),
            ProstCodec::default(),
        )
        .await
        .map(|r| r.into_inner())
        .map_err(|_| Error::other("订阅 Tailscale 状态失败，请检查组件版本"))
    }
    async fn logout(&self) -> Result<()> {
        let mut g = Grpc::new(self.channel.clone());
        g.ready()
            .await
            .map_err(|_| Error::other("组网 API 不可用"))?;
        let _: tonic::Response<Empty> = g
            .unary(
                self.request(EndpointRequest {
                    tag: "mesh-ts".into(),
                })?,
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/daemon.StartedService/TailscaleLogout",
                ),
                ProstCodec::default(),
            )
            .await
            .map_err(|_| Error::other("Tailscale 注销失败"))?;
        Ok(())
    }
    async fn ping(&self, ip: String) -> Result<PingResponse> {
        let mut g = Grpc::new(self.channel.clone());
        g.ready()
            .await
            .map_err(|_| Error::other("组网 API 不可用"))?;
        let r: tonic::Response<tonic::Streaming<PingResponse>> = g
            .server_streaming(
                self.request(PingRequest {
                    tag: "mesh-ts".into(),
                    ip,
                })?,
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/daemon.StartedService/StartTailscalePing",
                ),
                ProstCodec::default(),
            )
            .await
            .map_err(|_| Error::other("组网探测失败"))?;
        r.into_inner()
            .message()
            .await
            .map_err(|_| Error::other("组网探测失败"))?
            .ok_or_else(|| Error::other("组网探测没有结果"))
    }
}
pub struct Backend {
    child: Child,
    client: Client,
    state: Arc<Mutex<MeshRuntimeView>>,
    task: tokio::task::JoinHandle<()>,
    profile: MeshProfile,
}

/// Only fixed diagnoses leave the stderr reader. Never expose login URLs,
/// identity keys, control URLs or arbitrary upstream error text to logs/UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupFailure {
    DnsPort,
    ApiPort,
    Driver,
    DriverPermission,
    DriverPrivilege,
    DriverConflict,
    DriverParameter,
    DriverMissing,
    Permission,
    Identity,
    Network,
}
impl StartupFailure {
    fn classify(line: &str) -> Option<Self> {
        let line = line.to_ascii_lowercase();
        if line.contains("bind") || line.contains("listen") {
            if line.contains("127.0.0.153:53") || line.contains("inbound/direct[mesh-dns]") {
                return Some(Self::DnsPort);
            }
            if line.contains("service/api") {
                return Some(Self::ApiPort);
            }
        }
        if line.contains("wintun") || line.contains("inbound/tun[mesh-tun]") {
            if line.contains("access is denied")
                || line.contains("access denied")
                || line.contains("permission denied")
                || line.contains("拒绝访问")
            {
                return Some(Self::DriverPermission);
            }
            if line.contains("privilege") || line.contains("特权") {
                return Some(Self::DriverPrivilege);
            }
            if line.contains("already exists")
                || line.contains("already in use")
                || line.contains("已经存在")
                || line.contains("已存在")
            {
                return Some(Self::DriverConflict);
            }
            if line.contains("parameter is incorrect")
                || line.contains("invalid argument")
                || line.contains("参数错误")
                || line.contains("参数不正确")
            {
                return Some(Self::DriverParameter);
            }
            if line.contains("cannot find") || line.contains("not found") || line.contains("找不到")
            {
                return Some(Self::DriverMissing);
            }
            return Some(Self::Driver);
        }
        if line.contains("access is denied") || line.contains("permission denied") {
            return Some(Self::Permission);
        }
        if line.contains("state") || line.contains("identity") || line.contains("file is locked") {
            return Some(Self::Identity);
        }
        if line.contains("timeout")
            || line.contains("no such host")
            || line.contains("network is unreachable")
        {
            return Some(Self::Network);
        }
        None
    }
    fn message(self) -> &'static str {
        match self {
            Self::DnsPort => "本地 DNS 监听 127.0.0.153:53 失败，请检查端口占用",
            Self::ApiPort => "本地管理 API 端口监听失败，请重试连接",
            Self::Driver => "虚拟网卡初始化失败，请检查 Wintun 驱动和助手管理员权限",
            Self::DriverPermission => "Windows 拒绝虚拟网卡访问，请检查助手权限和驱动访问策略",
            Self::DriverPrivilege => "创建虚拟网卡所需的 Windows 特权不可用，请重新安装提权助手",
            Self::DriverConflict => "虚拟网卡、地址或路由已存在，请先断开重复运行的组网实例",
            Self::DriverParameter => "Windows 拒绝虚拟网卡参数，请检查接口地址及系统网络驱动状态",
            Self::DriverMissing => "Windows 无法找到虚拟网卡驱动或接口，请重新初始化 Wintun",
            Self::Permission => "访问被拒绝，请检查助手权限及组网身份目录权限",
            Self::Identity => "设备身份状态无法打开，请检查目录权限或是否有重复运行的实例",
            Self::Network => "连接控制服务器失败，请检查当前联网和控制服务器地址",
        }
    }
}

fn read_startup_failure(input: impl Read) -> Option<StartupFailure> {
    let mut reader = BufReader::new(input);
    let mut failure = None;
    loop {
        let mut line = Vec::new();
        if reader
            .by_ref()
            .take(4097)
            .read_until(b'\n', &mut line)
            .unwrap_or(0)
            == 0
        {
            return failure;
        }
        if let Some(next) = StartupFailure::classify(&String::from_utf8_lossy(&line)) {
            failure = Some(next);
        }
    }
}

impl Backend {
    pub async fn start(p: MeshProfile, directory: &Path, archive: &Path) -> Result<Self> {
        Self::start_inner(p, directory, archive, true).await
    }

    async fn start_inner(
        p: MeshProfile,
        directory: &Path,
        archive: &Path,
        with_interface: bool,
    ) -> Result<Self> {
        let executable = unpack(archive, directory)?;
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|_| Error::other("无法分配组网 API 端口"))?
            .local_addr()
            .map_err(|_| Error::other("无法分配端口"))?
            .port();
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut config = crate::adapters::singbox::compile_mesh(&p, directory, port, &secret)?;
        if !with_interface {
            config["inbounds"] = serde_json::json!([]);
            config["route"]["rules"] = serde_json::json!([]);
        }
        let path = directory.join("config.json");
        crate::util::atomic_write_json(&path, &config)?;
        let report =
            crate::adapters::validate(crate::model::CoreKind::SingBox, &executable, &path).await?;
        if !report.ok {
            return Err(Error::config(
                "Tailscale 组网配置校验失败，组件需包含 Tailscale 和 gVisor 支持",
            ));
        }
        let mut cmd = Command::new(&executable);
        cmd.args(["run", "-c"])
            .arg(&path)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|_| Error::other("Tailscale 组网进程启动失败"))?;
        let stderr = child.stderr.take().expect("piped stderr");
        let diagnostics = std::thread::spawn(move || read_startup_failure(stderr));
        let stdout = child.stdout.take().expect("piped stdout");
        let output_diagnostics = std::thread::spawn(move || read_startup_failure(stdout));
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if crate::core::job::adopt(child.as_raw_handle()).is_err() {
                let _ = child.kill();
                return Err(Error::other("无法保护 Tailscale 进程生命周期"));
            }
        }
        let endpoint = Channel::from_shared(format!("http://127.0.0.1:{port}"))
            .map_err(|_| Error::other("组网 API 地址无效"))?
            .connect_timeout(Duration::from_secs(2));
        let mut channel = None;
        let mut exited = None;
        // Windows may spend several seconds installing the first Wintun
        // adapter. A fixed nine-second window misreports this as an API fault.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if let Some(status) = child.try_wait().ok().flatten() {
                exited = Some(status);
                break;
            }
            if let Ok(c) = endpoint.connect().await {
                channel = Some(c);
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let Some(channel) = channel else {
            let _ = child.kill();
            let _ = child.wait();
            let detail = diagnostics
                .join()
                .ok()
                .flatten()
                .or_else(|| output_diagnostics.join().ok().flatten())
                .map(StartupFailure::message);
            return Err(Error::other(match (exited, detail) {
                (Some(status), Some(detail)) => {
                    format!("Tailscale 进程启动失败（{status}）：{detail}")
                }
                (Some(status), None) => {
                    format!("Tailscale 进程在 API 就绪前退出（{status}），请检查组件完整性并重试")
                }
                (None, Some(detail)) => format!("Tailscale API 启动超时：{detail}"),
                (None, None) => "Tailscale 进程仍在运行，但管理 API 未就绪，请重试连接".into(),
            }));
        };
        let client = Client { channel, secret };
        if let Err(error) = client.verify_version().await {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let state = Arc::new(Mutex::new(MeshRuntimeView {
            profile_id: p.id.clone(),
            provider: MeshProvider::Tailscale,
            phase: MeshPhase::Starting,
            interface: format!("xross-mesh-ts-{}", &p.id[..8]),
            ..Default::default()
        }));
        let observed = state.clone();
        let watcher = client.clone();
        let task = tokio::spawn(async move {
            loop {
                match watcher.status().await {
                    Ok(mut stream) => {
                        while let Ok(Some(update)) = stream.message().await {
                            if let Some(e) =
                                update.endpoints.into_iter().find(|e| e.tag == "mesh-ts")
                            {
                                let mut s = observed.lock();
                                s.phase = match e.state.as_str() {
                                    "Running" => MeshPhase::Connected,
                                    "NeedsLogin" | "NeedsMachineAuth" => MeshPhase::NeedsLogin,
                                    _ => MeshPhase::Starting,
                                };
                                s.login_url = url::Url::parse(&e.auth_url)
                                    .ok()
                                    .filter(|u| {
                                        u.scheme() == "https"
                                            && u.username().is_empty()
                                            && u.password().is_none()
                                    })
                                    .map(|u| u.to_string());
                                s.addresses = e.self_peer.map(|p| p.ips).unwrap_or_default();
                                s.dns_suffix = (!e.suffix.is_empty())
                                    .then(|| e.suffix.trim_end_matches('.').to_string());
                                s.dns_address = Some("127.0.0.153".into());
                                s.peers = e
                                    .groups
                                    .into_iter()
                                    .flat_map(|g| g.peers)
                                    .take(1024)
                                    .map(|p| MeshPeer {
                                        id: p.id,
                                        name: if p.dns_name.is_empty() {
                                            p.hostname
                                        } else {
                                            p.dns_name.trim_end_matches('.').into()
                                        },
                                        addresses: p.ips,
                                        online: p.online,
                                        ..Default::default()
                                    })
                                    .collect();
                                s.updated_at = crate::util::now_unix();
                                s.error = None;
                            }
                        }
                    }
                    Err(_) => {
                        observed.lock().error = Some("Tailscale 状态连接中断，正在重试".into());
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        Ok(Self {
            child,
            client,
            state,
            task,
            profile: p,
        })
    }
    pub fn snapshot(&mut self) -> Result<MeshRuntimeView> {
        if self
            .child
            .try_wait()
            .map_err(|_| Error::other("读取 Tailscale 进程失败"))?
            .is_some()
        {
            return Err(Error::other("Tailscale 组网进程已退出"));
        }
        let mut s = self.state.lock().clone();
        s.routes = s
            .addresses
            .iter()
            .filter_map(|a| a.parse::<std::net::IpAddr>().ok())
            .map(|a| format!("{a}/{}", if a.is_ipv4() { 32 } else { 128 }))
            .collect();
        for p in &s.peers {
            s.routes.extend(
                p.addresses
                    .iter()
                    .filter_map(|a| a.parse::<std::net::IpAddr>().ok())
                    .map(|a| format!("{a}/{}", if a.is_ipv4() { 32 } else { 128 })),
            );
        }
        s.routes.extend(self.profile.remote_subnets.clone());
        s.routes.sort();
        s.routes.dedup();
        Ok(s)
    }
    pub async fn logout(&self) -> Result<()> {
        self.client.logout().await
    }
    pub async fn probe(&self, ip: String) -> Result<Option<f64>> {
        let p = tokio::time::timeout(Duration::from_secs(12), self.client.ping(ip.clone()))
            .await
            .map_err(|_| Error::other("组网探测超时"))??;
        if !p.error.is_empty() {
            return Err(Error::other("组网设备不可达"));
        }
        if let Some(peer) = self
            .state
            .lock()
            .peers
            .iter_mut()
            .find(|p| p.addresses.contains(&ip))
        {
            peer.latency_ms = Some(p.latency);
            peer.connection = Some(if p.direct { "direct" } else { "relay" }.into());
        }
        Ok(Some(p.latency))
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_diagnostics_expose_only_fixed_messages() {
        let sensitive = "https://example.test/auth?token=fake-private-key";
        let log = format!("login at {sensitive}\nFATAL start inbound/direct[mesh-dns]: listen udp 127.0.0.153:53: bind failed {sensitive}\n");
        let failure = read_startup_failure(log.as_bytes()).unwrap();
        assert_eq!(failure, StartupFailure::DnsPort);
        assert!(!failure.message().contains("fake-private-key"));
        assert!(!failure.message().contains("example.test"));
        assert_eq!(read_startup_failure(sensitive.as_bytes()), None);
        assert_eq!(
            read_startup_failure(
                "FATAL start inbound/tun[mesh-tun]: create Wintun adapter: Access is denied.\n"
                    .as_bytes()
            ),
            Some(StartupFailure::DriverPermission)
        );
        assert_eq!(
            read_startup_failure("FATAL start inbound/tun[mesh-tun]: create adapter: A required privilege is not held by the client.\n".as_bytes()),
            Some(StartupFailure::DriverPrivilege)
        );
        assert_eq!(
            read_startup_failure("FATAL start inbound/tun[mesh-tun]: configure interface: The object already exists.\n".as_bytes()),
            Some(StartupFailure::DriverConflict)
        );
    }

    #[test]
    fn startup_diagnostics_drain_long_lines_and_keep_the_final_cause() {
        let log = format!(
            "{}\nFATAL start inbound/tun[mesh-tun]: Wintun initialization failed\n",
            "x".repeat(100_000)
        );
        assert_eq!(
            read_startup_failure(log.as_bytes()),
            Some(StartupFailure::Driver)
        );
    }
}
