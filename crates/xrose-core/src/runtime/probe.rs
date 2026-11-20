//! Isolated per-node HTTP probes with bounded streaming downloads.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{CoreKind, Node};
use crate::settings::HttpLatencyMode;

const LATENCY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_WARMUP_DRAIN_BYTES: usize = 64 * 1024;

pub const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=99000000";
pub const DOMESTIC_DOWNLOAD_URL: &str = "https://mensura.cdn-apple.com/api/v1/gm/large";
const OVERSEAS_DOWNLOAD_LIMIT: u64 = 99_000_000;
const OVERSEAS_DOWNLOAD_DURATION: Duration = Duration::from_secs(10);
const DOMESTIC_DOWNLOAD_LIMIT: u64 = 1024 * 1024 * 1024;
const DOMESTIC_DOWNLOAD_DURATION: Duration = Duration::from_secs(12);
const AUTO_DOWNLOAD_DURATION: Duration = Duration::from_secs(5);
const DOMESTIC_AUTO_LIMIT: u64 = 8 * 1024 * 1024;
const OVERSEAS_AUTO_LIMIT: u64 = 4 * 1024 * 1024;
const SLOW_START_WARMUP: Duration = Duration::from_secs(2);
const MIN_STEADY_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub bytes_per_second: u64,
    pub checked_at: i64,
    pub error: Option<String>,
}

/// Normal completion reaps the child; cancellation also requests termination.
pub(crate) struct Probe {
    child: tokio::process::Child,
    _directory: tempfile::TempDir,
    pub(crate) client: reqwest::Client,
    pub(crate) port: u16,
}

pub(crate) async fn resolve_probe_node(
    core: CoreKind,
    node: Option<&Node>,
    bypass_tun: bool,
) -> Result<(Option<Node>, Option<String>)> {
    let mut target = node.cloned();
    let loopback_gateway = node.is_some_and(|node| {
        node.server
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    let interface = if bypass_tun && !loopback_gateway {
        let interface = default_uplink().await?;
        // Resolve before timing the request; the probe core must not recurse
        // through a TUN-controlled system resolver while dialing its server.
        if core == CoreKind::Xray {
            if let Some(node) = target
                .as_mut()
                .filter(|node| node.server.parse::<std::net::IpAddr>().is_err())
            {
                let address = tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::net::lookup_host((node.server.as_str(), node.port)),
                )
                .await
                .map_err(|_| Error::other("节点地址解析超时"))??
                .next()
                .ok_or_else(|| Error::other("节点服务器没有可用 IP 地址"))?;
                // Preserve TLS identity when replacing a hostname with an IP.
                if node.tls.server_name.is_none() {
                    node.tls.server_name = Some(node.server.clone());
                }
                match &mut node.transport {
                    crate::model::Transport::WebSocket { host, .. }
                    | crate::model::Transport::Http2 { host, .. }
                    | crate::model::Transport::HttpUpgrade { host, .. }
                        if host.is_none() =>
                    {
                        *host = Some(node.server.clone());
                    }
                    _ => {}
                }
                node.server = address.ip().to_string();
            }
        }
        Some(interface)
    } else {
        None
    };
    Ok((target, interface))
}

pub(crate) async fn spawn_isolated_probe_core(
    binary: &Path,
    core: CoreKind,
    config: &serde_json::Value,
    ready_port: u16,
) -> Result<(tokio::process::Child, tempfile::TempDir)> {
    let directory = tempfile::Builder::new().prefix("xrose-probe-").tempdir()?;
    let path = directory.path().join("config.json");
    crate::util::atomic_write_private(&path, &serde_json::to_vec(config)?)?;
    let mut command = tokio::process::Command::new(std::fs::canonicalize(binary)?);
    command
        .args(crate::core::run_args(core, &path))
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command.spawn()?;
    #[cfg(windows)]
    if let Some(handle) = child.raw_handle() {
        let _ = crate::core::job::adopt(handle);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            return Err(Error::process("测试内核启动失败，请检查节点配置与内核版本"));
        }
        if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, ready_port))
            .await
            .is_ok()
        {
            return Ok((child, directory));
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(Error::process("测试内核启动超时"));
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

impl Probe {
    pub(crate) async fn start_target(
        binary: &Path,
        core: CoreKind,
        node: Option<&Node>,
        bypass_tun: bool,
    ) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let (target, interface) = resolve_probe_node(core, node, bypass_tun).await?;
        let mut config = match target.as_ref() {
            Some(node) => crate::adapters::compile_probe(core, node, port)?,
            None => crate::adapters::compile_direct_probe(core, port),
        };
        if let Some(interface) = interface {
            match core {
                CoreKind::Xray => {
                    crate::adapters::xray::bind_probe_interface(&mut config, &interface);
                }
                CoreKind::SingBox => {
                    crate::adapters::singbox::bind_probe_interface(
                        &mut config,
                        &interface,
                        target.is_none(),
                    );
                }
            }
        }
        drop(listener);
        let client = probe_client(port)?;
        let (child, _directory) = spawn_isolated_probe_core(binary, core, &config, port).await?;
        Ok(Self {
            child,
            _directory,
            client,
            port,
        })
    }

    pub(crate) async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// Read the existing default uplink; this never mutates routes or interfaces.
async fn default_uplink() -> Result<String> {
    #[cfg(windows)]
    let (program, args) = (
        "powershell.exe",
        vec![
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$r=Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Where-Object { $_.NextHop -ne '0.0.0.0' } | Sort-Object @{Expression={$_.RouteMetric + $_.InterfaceMetric}} | Select-Object -First 1; if ($r) { [Console]::OutputEncoding=[Text.Encoding]::UTF8; $r.InterfaceAlias }",
        ],
    );
    #[cfg(target_os = "linux")]
    let (program, args) = ("ip", vec!["-j", "-4", "route", "show", "default"]);
    #[cfg(target_os = "macos")]
    let (program, args) = ("/sbin/route", vec!["-n", "get", "default"]);
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    return Err(Error::other("当前平台不支持 TUN 下的独立节点测试"));
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    {
        let (code, output) = crate::core::process::run_capture(
            Path::new(program),
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            Duration::from_secs(5),
        )
        .await?;
        if code != 0 {
            return Err(Error::other("无法检测默认网络接口，请检查系统路由"));
        }
        #[cfg(windows)]
        let name = output.trim().trim_start_matches('\u{feff}').to_string();
        #[cfg(target_os = "linux")]
        let name = serde_json::from_str::<serde_json::Value>(&output)
            .ok()
            .and_then(|v| {
                v.as_array()
                    .and_then(|r| {
                        r.iter()
                            .filter(|r| r.get("gateway").is_some())
                            .min_by_key(|r| r["metric"].as_u64().unwrap_or(0))
                    })
                    .and_then(|r| r["dev"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        #[cfg(target_os = "macos")]
        let name = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("interface:"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            return Err(Error::other("未找到默认网络接口，已取消节点测试"));
        }
        Ok(name)
    }
}

pub(crate) fn probe_client(port: u16) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))?)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Xrose-Probe/", env!("CARGO_PKG_VERSION")))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?)
}

pub async fn http_latency(
    binary: &Path,
    core: CoreKind,
    node: &Node,
    url: &str,
    bypass_tun: bool,
    mode: HttpLatencyMode,
) -> Result<u32> {
    http_target_latency(binary, core, Some(node), url, bypass_tun, mode).await
}

pub async fn http_target_latency(
    binary: &Path,
    core: CoreKind,
    node: Option<&Node>,
    url: &str,
    bypass_tun: bool,
    mode: HttpLatencyMode,
) -> Result<u32> {
    let mut probe = Probe::start_target(binary, core, node, bypass_tun).await?;
    let result = measure_http_latency(&probe.client, url, mode).await;
    probe.stop().await;
    result
}

enum LatencyRequestError {
    Transport,
    BadStatus(reqwest::StatusCode),
}

impl LatencyRequestError {
    fn into_error(self) -> Error {
        match self {
            Self::Transport => Error::other("HTTP 测速失败，请检查出口连接或测试地址"),
            Self::BadStatus(status) => Error::other(format!("测速地址返回 HTTP {status}")),
        }
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u32::MAX as u128) as u32
}

async fn send_latency_request(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<reqwest::Response, LatencyRequestError> {
    let response = client
        .get(url)
        .timeout(LATENCY_TIMEOUT)
        .send()
        .await
        .map_err(|_| LatencyRequestError::Transport)?;
    if !response.status().is_success() {
        return Err(LatencyRequestError::BadStatus(response.status()));
    }
    Ok(response)
}

fn response_requests_close(response: &reqwest::Response) -> bool {
    for name in [
        reqwest::header::CONNECTION,
        reqwest::header::HeaderName::from_static("proxy-connection"),
    ] {
        for value in response.headers().get_all(name) {
            if let Ok(text) = value.to_str()
                && text
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("close"))
            {
                return true;
            }
        }
    }
    false
}

/// Drain a small warm-up response body so HTTP/1.1 keep-alive connections are
/// returned cleanly to the client pool before the timed follow-up request.
async fn drain_response_body(mut response: reqwest::Response) {
    let deadline = tokio::time::Instant::now() + LATENCY_TIMEOUT;
    let mut total = 0usize;
    while total < MAX_WARMUP_DRAIN_BYTES {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(Some(chunk))) => total = total.saturating_add(chunk.len()),
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
}

pub(crate) async fn measure_http_latency(
    client: &reqwest::Client,
    url: &str,
    mode: HttpLatencyMode,
) -> Result<u32> {
    let first_started = Instant::now();
    let first = send_latency_request(client, url)
        .await
        .map_err(LatencyRequestError::into_error)?;
    let first_ms = elapsed_ms(first_started);
    if mode == HttpLatencyMode::FullHandshake {
        return Ok(first_ms);
    }

    if response_requests_close(&first) {
        drop(first);
    } else {
        drain_response_body(first).await;
    }
    // Yield once so hyper's background connection task returns the idle
    // socket to the keep-alive pool before starting the timed request.
    tokio::task::yield_now().await;

    let second_started = Instant::now();
    match send_latency_request(client, url).await {
        Ok(_) => Ok(elapsed_ms(second_started)),
        Err(err @ LatencyRequestError::BadStatus(_)) => Err(err.into_error()),
        Err(LatencyRequestError::Transport) => Ok(first_ms),
    }
}

pub async fn download(
    binary: &Path,
    core: CoreKind,
    node: &Node,
    bypass_tun: bool,
) -> Result<DownloadResult> {
    download_target(binary, core, Some(node), bypass_tun).await
}

pub async fn download_target(
    binary: &Path,
    core: CoreKind,
    node: Option<&Node>,
    bypass_tun: bool,
) -> Result<DownloadResult> {
    download_target_sample(binary, core, node, bypass_tun, false).await
}

/// Automatic rounds use a small response at the source, not a truncated 50 MiB request.
pub(crate) async fn download_sample(
    binary: &Path,
    core: CoreKind,
    node: &Node,
    bypass_tun: bool,
    automatic: bool,
) -> Result<DownloadResult> {
    download_target_sample(binary, core, Some(node), bypass_tun, automatic).await
}

fn download_sample_spec(is_direct: bool, automatic: bool) -> (&'static str, Duration, u64) {
    match (is_direct, automatic) {
        (true, true) => (
            DOMESTIC_DOWNLOAD_URL,
            AUTO_DOWNLOAD_DURATION,
            DOMESTIC_AUTO_LIMIT,
        ),
        (true, false) => (
            DOMESTIC_DOWNLOAD_URL,
            DOMESTIC_DOWNLOAD_DURATION,
            DOMESTIC_DOWNLOAD_LIMIT,
        ),
        (false, true) => (
            "https://speed.cloudflare.com/__down?bytes=4194304",
            AUTO_DOWNLOAD_DURATION,
            OVERSEAS_AUTO_LIMIT,
        ),
        (false, false) => (
            DOWNLOAD_URL,
            OVERSEAS_DOWNLOAD_DURATION,
            OVERSEAS_DOWNLOAD_LIMIT,
        ),
    }
}

fn compute_steady_bps(
    total_bytes: u64,
    elapsed: Duration,
    warmup_mark: Option<(Duration, u64)>,
) -> u64 {
    if let Some((w_elapsed, w_bytes)) = warmup_mark
        && elapsed.saturating_sub(w_elapsed) >= MIN_STEADY_WINDOW
        && total_bytes > w_bytes
    {
        let steady_secs = (elapsed - w_elapsed).as_secs_f64();
        return ((total_bytes - w_bytes) as f64 / steady_secs) as u64;
    }
    (total_bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64
}

async fn download_target_sample(
    binary: &Path,
    core: CoreKind,
    node: Option<&Node>,
    bypass_tun: bool,
    automatic: bool,
) -> Result<DownloadResult> {
    let mut probe = Probe::start_target(binary, core, node, bypass_tun).await?;
    let (url, duration, limit) = download_sample_spec(node.is_none(), automatic);
    let result = measure_download(&probe.client, url, duration, limit).await;
    probe.stop().await;
    result
}

async fn measure_download(
    client: &reqwest::Client,
    url: &str,
    duration: Duration,
    limit: u64,
) -> Result<DownloadResult> {
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(|_| Error::other("下载测试连接失败，请检查节点"))?;
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "下载测试地址返回 HTTP {}",
            response.status()
        )));
    }
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + duration;
    let track_warmup = duration >= AUTO_DOWNLOAD_DURATION;
    let mut warmup_mark: Option<(Duration, u64)> = None;
    let mut bytes = 0;
    while bytes < limit {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                bytes += (chunk.len() as u64).min(limit - bytes);
                if track_warmup && warmup_mark.is_none() {
                    let now_elapsed = started.elapsed();
                    if now_elapsed >= SLOW_START_WARMUP {
                        warmup_mark = Some((now_elapsed, bytes));
                    }
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(_)) => return Err(Error::other("下载测试连接中断，请重试")),
            Err(_) => break,
        }
    }
    if bytes == 0 {
        return Err(Error::other("下载测试未收到数据"));
    }
    let elapsed = started.elapsed();
    Ok(DownloadResult {
        bytes,
        elapsed_ms: elapsed.as_millis().max(1) as u64,
        bytes_per_second: compute_steady_bps(bytes, elapsed, warmup_mark),
        checked_at: crate::util::now_unix(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_socks5_connect(socket: &mut tokio::net::TcpStream) -> String {
        let mut head = [0u8; 2];
        socket.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x05);
        let mut methods = vec![0u8; head[1] as usize];
        socket.read_exact(&mut methods).await.unwrap();
        socket.write_all(&[0x05, 0x00]).await.unwrap();

        let mut req = [0u8; 4];
        socket.read_exact(&mut req).await.unwrap();
        assert_eq!(req[0], 0x05);
        assert_eq!(req[1], 0x01); // CONNECT
        let host = match req[3] {
            0x01 => {
                let mut ip = [0u8; 4];
                socket.read_exact(&mut ip).await.unwrap();
                std::net::Ipv4Addr::from(ip).to_string()
            }
            0x03 => {
                let len = socket.read_u8().await.unwrap() as usize;
                let mut domain = vec![0u8; len];
                socket.read_exact(&mut domain).await.unwrap();
                String::from_utf8(domain).unwrap()
            }
            other => panic!("unexpected SOCKS5 atyp {other}"),
        };
        let mut port = [0u8; 2];
        socket.read_exact(&mut port).await.unwrap();
        socket
            .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        host
    }

    async fn read_http_head(socket: &mut tokio::net::TcpStream) -> Option<String> {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            match socket.read_u8().await {
                Ok(byte) => request.push(byte),
                Err(_) if request.is_empty() => return None,
                Err(_) => return None,
            }
        }
        Some(String::from_utf8(request).unwrap())
    }

    struct FakeSocksReport {
        host: String,
        requests: Vec<String>,
        connections: usize,
    }

    async fn fake_socks5_proxy(
        response: &'static [u8],
        max_requests_per_conn: usize,
        handshake_delay: Duration,
        stall: bool,
    ) -> (u16, tokio::task::JoinHandle<FakeSocksReport>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let connections_task = connections.clone();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            connections_task.fetch_add(1, Ordering::SeqCst);
            if !handshake_delay.is_zero() {
                tokio::time::sleep(handshake_delay).await;
            }
            let host = read_socks5_connect(&mut socket).await;
            let mut requests = Vec::new();
            for _ in 0..max_requests_per_conn {
                let Some(request) = read_http_head(&mut socket).await else {
                    break;
                };
                requests.push(request);
                socket.write_all(response).await.unwrap();
                if stall {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            FakeSocksReport {
                host,
                requests,
                connections: connections_task.load(Ordering::SeqCst),
            }
        });
        (port, task)
    }

    #[tokio::test]
    async fn http_latency_measures_successful_requests_and_rejects_error_statuses() {
        for mode in [HttpLatencyMode::Unified, HttpLatencyMode::FullHandshake] {
            let expected_reqs = if mode == HttpLatencyMode::Unified {
                2
            } else {
                1
            };
            for response in [
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice(),
            ] {
                let (port, task) =
                    fake_socks5_proxy(response, expected_reqs, Duration::ZERO, false).await;
                assert!(
                    measure_http_latency(
                        &probe_client(port).unwrap(),
                        "http://probe.invalid/test",
                        mode,
                    )
                    .await
                    .is_ok()
                );
                let report = task.await.unwrap();
                assert_eq!(report.host, "probe.invalid");
                assert_eq!(report.connections, 1);
                assert_eq!(report.requests.len(), expected_reqs);
                assert!(report.requests[0].starts_with("GET /test HTTP/1.1"));
            }
            for response in [
                b"HTTP/1.1 500 Server Error\r\nContent-Length: 0\r\n\r\n".as_slice(),
                b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".as_slice(),
            ] {
                let (port, task) = fake_socks5_proxy(response, 1, Duration::ZERO, false).await;
                assert!(
                    measure_http_latency(
                        &probe_client(port).unwrap(),
                        "http://probe.invalid/test",
                        mode,
                    )
                    .await
                    .is_err()
                );
                task.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn unified_mode_excludes_cold_handshake_while_full_handshake_includes_it() {
        let response = b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".as_slice();
        let handshake_delay = Duration::from_millis(140);

        let (unified_port, unified_task) =
            fake_socks5_proxy(response, 2, handshake_delay, false).await;
        let unified_ms = measure_http_latency(
            &probe_client(unified_port).unwrap(),
            "http://probe.invalid/generate_204",
            HttpLatencyMode::Unified,
        )
        .await
        .unwrap();
        let unified_report = unified_task.await.unwrap();
        assert_eq!(unified_report.connections, 1);
        assert_eq!(unified_report.requests.len(), 2);
        assert!(
            unified_ms < 100,
            "expected unified latency to exclude 140ms handshake, got {unified_ms}ms"
        );

        let (cold_port, cold_task) = fake_socks5_proxy(response, 1, handshake_delay, false).await;
        let cold_ms = measure_http_latency(
            &probe_client(cold_port).unwrap(),
            "http://probe.invalid/generate_204",
            HttpLatencyMode::FullHandshake,
        )
        .await
        .unwrap();
        let cold_report = cold_task.await.unwrap();
        assert_eq!(cold_report.connections, 1);
        assert_eq!(cold_report.requests.len(), 1);
        assert!(
            cold_ms >= 120,
            "expected full-handshake latency to include 140ms handshake, got {cold_ms}ms"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unified_mode_consistently_reuses_single_connection_on_multithreaded_runtime() {
        let response = b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".as_slice();
        for _ in 0..10 {
            let (port, task) = fake_socks5_proxy(response, 2, Duration::ZERO, false).await;
            let result = measure_http_latency(
                &probe_client(port).unwrap(),
                "http://probe.invalid/generate_204",
                HttpLatencyMode::Unified,
            )
            .await;
            assert!(result.is_ok());
            let report = task.await.unwrap();
            assert_eq!(report.connections, 1);
            assert_eq!(report.requests.len(), 2);
        }
    }

    #[tokio::test]
    async fn unified_mode_falls_back_gracefully_when_server_closes_after_first_response() {
        let response =
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".as_slice();
        let (port, task) = fake_socks5_proxy(response, 1, Duration::ZERO, false).await;
        let ms = measure_http_latency(
            &probe_client(port).unwrap(),
            "http://probe.invalid/generate_204",
            HttpLatencyMode::Unified,
        )
        .await
        .unwrap();
        assert!(ms < 2000);
        let report = task.await.unwrap();
        assert_eq!(report.requests.len(), 1);
    }

    #[tokio::test]
    async fn download_uses_explicit_proxy_and_caps_bytes() {
        let (port, task) = fake_socks5_proxy(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nabcdefgh",
            1,
            Duration::ZERO,
            false,
        )
        .await;
        let result = measure_download(
            &probe_client(port).unwrap(),
            "http://node-probe.invalid/file",
            Duration::from_secs(1),
            4,
        )
        .await
        .unwrap();
        assert_eq!(result.bytes, 4);
        assert!(result.bytes_per_second > 0);
        let report = task.await.unwrap();
        assert_eq!(report.host, "node-probe.invalid");
        let request = &report.requests[0];
        assert!(request.starts_with("GET /file HTTP/1.1"));
        assert!(request.to_lowercase().contains("accept-encoding: identity"));
        assert!(request.to_lowercase().contains("user-agent: xrose-probe/"));
    }

    #[tokio::test]
    async fn stalled_download_finishes_at_deadline_with_received_bytes() {
        let (port, task) = fake_socks5_proxy(
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n1234",
            1,
            Duration::ZERO,
            true,
        )
        .await;
        let started = Instant::now();
        let result = measure_download(
            &probe_client(port).unwrap(),
            "http://node-probe.invalid/file",
            Duration::from_millis(80),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(result.bytes, 4);
        assert!(started.elapsed() < Duration::from_millis(800));
        assert!(result.elapsed_ms >= 70);
        task.abort();
    }

    #[tokio::test]
    async fn errors_empty_bodies_redirects_and_truncation_are_not_speed_results() {
        for response in [
            &b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n1234"[..],
        ] {
            let (port, task) = fake_socks5_proxy(response, 1, Duration::ZERO, false).await;
            assert!(
                measure_download(
                    &probe_client(port).unwrap(),
                    "http://node-probe.invalid/file",
                    Duration::from_secs(1),
                    1000
                )
                .await
                .is_err()
            );
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn failed_proxy_never_falls_back_to_direct() {
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = proxy.local_addr().unwrap().port();
        drop(proxy);
        let url = format!("http://{}/", origin.local_addr().unwrap());
        assert!(probe_client(port).unwrap().get(url).send().await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), origin.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn direct_download_uses_domestic_cdn_while_proxy_uses_cloudflare() {
        let (manual_direct_url, manual_direct_dur, manual_limit) =
            download_sample_spec(true, false);
        assert_eq!(manual_direct_url, DOMESTIC_DOWNLOAD_URL);
        assert_eq!(manual_direct_dur, DOMESTIC_DOWNLOAD_DURATION);
        assert_eq!(manual_limit, DOMESTIC_DOWNLOAD_LIMIT);

        let (auto_direct_url, auto_direct_dur, auto_limit) = download_sample_spec(true, true);
        assert_eq!(auto_direct_url, DOMESTIC_DOWNLOAD_URL);
        assert_eq!(auto_direct_dur, AUTO_DOWNLOAD_DURATION);
        assert_eq!(auto_limit, DOMESTIC_AUTO_LIMIT);

        let (manual_proxy_url, manual_proxy_dur, manual_proxy_limit) =
            download_sample_spec(false, false);
        assert_eq!(manual_proxy_url, DOWNLOAD_URL);
        assert_eq!(manual_proxy_dur, OVERSEAS_DOWNLOAD_DURATION);
        assert_eq!(manual_proxy_limit, OVERSEAS_DOWNLOAD_LIMIT);
    }

    #[test]
    fn steady_state_bps_discards_slow_start_warmup_and_falls_back_when_short() {
        // 2 MiB during 2s slow start, then 80 MiB over the next 8s = 10 MiB/s steady state.
        let bps = compute_steady_bps(
            82 * 1024 * 1024,
            Duration::from_secs(10),
            Some((Duration::from_secs(2), 2 * 1024 * 1024)),
        );
        assert_eq!(bps, 10 * 1024 * 1024);

        // Fewer than MIN_STEADY_WINDOW (1s) remaining after warmup: fall back to overall average.
        let short_bps = compute_steady_bps(
            5 * 1024 * 1024,
            Duration::from_millis(2500),
            Some((Duration::from_secs(2), 3 * 1024 * 1024)),
        );
        assert_eq!(short_bps, 2 * 1024 * 1024);
    }
}
