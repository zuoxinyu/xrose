//! Passive, non-intrusive reachability & UDP capability probing through an
//! isolated probe core (AI, streaming, general web cleanliness, and SOCKS5 STUN).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::LOCATION;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{Error, Result};
use crate::model::{CoreKind, Node};
use crate::runtime::health::{ReachabilityCheckStatus, ReachabilityReport, ServiceCheckResult};
use crate::runtime::probe::{probe_client, resolve_probe_node, spawn_isolated_probe_core};
use crate::util::now_unix;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SNIPPET_BYTES: usize = 64 * 1024;
const STUN_MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

#[derive(Debug, Clone)]
pub struct ReachabilityOutcome {
    pub egress_ip: Option<String>,
    pub report: ReachabilityReport,
    pub bytes_used: u64,
    pub trace_rtt_ms: Option<u32>,
    pub latency_ms: Option<u32>,
    pub from_cache: bool,
}

#[derive(Debug, Clone)]
struct HttpObservation {
    status: u16,
    location: Option<String>,
    cf_mitigated: bool,
    snippet: String,
    bytes: u64,
    rtt_ms: u32,
}

struct IsolatedDualProbe {
    child: tokio::process::Child,
    _directory: tempfile::TempDir,
    client: reqwest::Client,
    socks_port: u16,
}

impl IsolatedDualProbe {
    async fn start(
        binary: &Path,
        core: CoreKind,
        node: Option<&Node>,
        bypass_tun: bool,
    ) -> Result<Self> {
        let http_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let http_port = http_listener.local_addr()?.port();
        let socks_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let socks_port = socks_listener.local_addr()?.port();
        let (target, interface) = resolve_probe_node(core, node, bypass_tun).await?;
        let mut config = crate::adapters::compile_reachability_probe(
            core,
            target.as_ref(),
            http_port,
            socks_port,
        )?;
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
        drop(http_listener);
        drop(socks_listener);
        let client = probe_client(http_port)?;
        let (child, _directory) =
            spawn_isolated_probe_core(binary, core, &config, http_port).await?;
        Ok(Self {
            child,
            _directory,
            client,
            socks_port,
        })
    }

    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

pub async fn probe_reachability<F>(
    binary: &Path,
    core: CoreKind,
    node: Option<&Node>,
    bypass_tun: bool,
    udp_enabled: bool,
    force_refresh: bool,
    latency_url: Option<&str>,
    lookup_cached: F,
) -> Result<ReachabilityOutcome>
where
    F: FnOnce(&str) -> Option<ReachabilityReport>,
{
    let mut probe = IsolatedDualProbe::start(binary, core, node, bypass_tun).await?;
    let outcome = run_reachability_suite(
        &probe.client,
        probe.socks_port,
        udp_enabled,
        force_refresh,
        latency_url,
        lookup_cached,
    )
    .await;
    probe.stop().await;
    outcome
}

async fn run_reachability_suite<F>(
    client: &reqwest::Client,
    socks_port: u16,
    udp_enabled: bool,
    force_refresh: bool,
    latency_url: Option<&str>,
    lookup_cached: F,
) -> Result<ReachabilityOutcome>
where
    F: FnOnce(&str) -> Option<ReachabilityReport>,
{
    let udp_fut = async {
        if udp_enabled {
            socks5_stun_probe(
                socks_port,
                "stun.cloudflare.com",
                3478,
                Duration::from_secs(4),
            )
            .await
        } else {
            ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("节点未启用 UDP")
        }
    };
    let trace_fut = fetch_egress_trace(client, "https://www.cloudflare.com/cdn-cgi/trace");
    let latency_fut = async {
        match latency_url {
            Some(url) if !url.trim().is_empty() => {
                crate::runtime::probe::measure_http_latency(client, url)
                    .await
                    .ok()
            }
            _ => None,
        }
    };
    let (udp_stun, trace_result, latency_ms) = tokio::join!(udp_fut, trace_fut, latency_fut);

    let (egress_ip, egress_region, mut bytes_used, trace_rtt_ms, trace_cf_mitigated) =
        match trace_result {
            Ok((ip, loc, bytes, rtt, mitigated)) => (ip, loc, bytes, Some(rtt), mitigated),
            Err(_) => (None, None, 0, None, false),
        };
    if latency_ms.is_some() {
        bytes_used = bytes_used.saturating_add(512);
    }

    if !force_refresh
        && let Some(ip) = egress_ip.as_deref()
        && let Some(mut cached) = lookup_cached(ip)
        && cached.scholar.status != ReachabilityCheckStatus::Untested
    {
        cached.udp_stun = udp_stun;
        return Ok(ReachabilityOutcome {
            egress_ip,
            report: cached,
            bytes_used,
            trace_rtt_ms,
            latency_ms,
            from_cache: true,
        });
    }

    let (
        chatgpt_obs,
        claude_obs,
        gemini_obs,
        copilot_obs,
        nf_main_obs,
        youtube_obs,
        disney_obs,
        tiktok_obs,
        google_obs,
        scholar_obs,
        github_obs,
    ) = tokio::join!(
        fetch_observation(client, "https://chatgpt.com/"),
        fetch_observation(client, "https://claude.ai/login"),
        fetch_observation(client, "https://gemini.google.com/"),
        fetch_observation(client, "https://copilot-proxy.githubusercontent.com/_ping"),
        fetch_observation(client, "https://www.netflix.com/title/81280792"),
        fetch_observation(client, "https://www.youtube.com/premium"),
        fetch_observation(client, "https://www.disneyplus.com/"),
        fetch_observation(client, "https://www.tiktok.com/"),
        fetch_observation(client, "https://www.google.com/generate_204"),
        fetch_observation(client, "https://scholar.google.com/"),
        fetch_observation(client, "https://github.com/robots.txt"),
    );

    let nf_orig_obs = match &nf_main_obs {
        Ok(obs) if obs.status == 200 => None,
        _ => Some(fetch_observation(client, "https://www.netflix.com/title/80018499").await),
    };

    for obs in [
        &chatgpt_obs,
        &claude_obs,
        &gemini_obs,
        &copilot_obs,
        &nf_main_obs,
        &youtube_obs,
        &disney_obs,
        &tiktok_obs,
        &google_obs,
        &scholar_obs,
        &github_obs,
    ] {
        if let Ok(o) = obs {
            bytes_used = bytes_used.saturating_add(o.bytes);
        }
    }
    if let Some(Ok(o)) = &nf_orig_obs {
        bytes_used = bytes_used.saturating_add(o.bytes);
    }

    let report = ReachabilityReport {
        checked_at: now_unix(),
        egress_region: egress_region.clone(),
        chatgpt: evaluate_chatgpt(chatgpt_obs.ok().as_ref()),
        claude: evaluate_claude(claude_obs.ok().as_ref()),
        gemini: evaluate_gemini(gemini_obs.ok().as_ref()),
        copilot: evaluate_simple_api(copilot_obs.ok().as_ref(), "Copilot"),
        netflix: evaluate_netflix(
            nf_main_obs.ok().as_ref(),
            nf_orig_obs.and_then(std::result::Result::ok).as_ref(),
            egress_region.as_deref(),
        ),
        youtube: evaluate_youtube(youtube_obs.ok().as_ref(), egress_region.as_deref()),
        disney: evaluate_disney(disney_obs.ok().as_ref(), egress_region.as_deref()),
        tiktok: evaluate_tiktok(tiktok_obs.ok().as_ref(), egress_region.as_deref()),
        web_clean: evaluate_web_clean(google_obs.ok().as_ref(), trace_cf_mitigated),
        scholar: evaluate_scholar(scholar_obs.ok().as_ref()),
        dev_hub: evaluate_simple_api(github_obs.ok().as_ref(), "GitHub"),
        udp_stun,
    };

    Ok(ReachabilityOutcome {
        egress_ip,
        report,
        bytes_used,
        trace_rtt_ms,
        latency_ms,
        from_cache: false,
    })
}

async fn fetch_egress_trace(
    client: &reqwest::Client,
    url: &str,
) -> Result<(Option<String>, Option<String>, u64, u32, bool)> {
    let obs = fetch_observation(client, url).await?;
    if obs.status != 200 {
        return Err(Error::other(format!("Trace HTTP {}", obs.status)));
    }
    let mut ip = None;
    let mut loc = None;
    for line in obs.snippet.lines() {
        if let Some(v) = line.strip_prefix("ip=") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                ip = Some(trimmed.to_owned());
            }
        } else if let Some(v) = line.strip_prefix("loc=") {
            let trimmed = v.trim();
            if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("XX") {
                loc = Some(trimmed.to_ascii_uppercase());
            }
        }
    }
    Ok((ip, loc, obs.bytes, obs.rtt_ms, obs.cf_mitigated))
}

async fn fetch_observation(client: &reqwest::Client, url: &str) -> Result<HttpObservation> {
    let started = Instant::now();
    let response = client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|err| Error::other(format!("请求失败：{err}")))?;
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cf_mitigated = response
        .headers()
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.is_empty());

    let mut bytes_buf = Vec::new();
    let mut total_bytes = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        total_bytes = total_bytes.saturating_add(chunk.len() as u64);
        if bytes_buf.len() < MAX_SNIPPET_BYTES {
            let take = (MAX_SNIPPET_BYTES - bytes_buf.len()).min(chunk.len());
            bytes_buf.extend_from_slice(&chunk[..take]);
        }
        if bytes_buf.len() >= MAX_SNIPPET_BYTES {
            break;
        }
    }
    let rtt_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let snippet = String::from_utf8_lossy(&bytes_buf).into_owned();
    Ok(HttpObservation {
        status,
        location,
        cf_mitigated,
        snippet,
        bytes: total_bytes,
        rtt_ms,
    })
}

fn evaluate_chatgpt(obs: Option<&HttpObservation>) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let lower = obs.snippet.to_ascii_lowercase();
    if lower.contains("unsupported_country")
        || lower.contains("not available in your country")
        || lower.contains("region_not_supported")
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("地区不支持");
    }
    if obs.status == 429 {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail("请求限流 (429)");
    }
    if obs.cf_mitigated {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail(format!("需人机验证 ({})", obs.status));
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else if obs.status == 403 {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("HTTP 403 拒绝")
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_claude(obs: Option<&HttpObservation>) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    let lower = obs.snippet.to_ascii_lowercase();
    if loc.contains("app-unavailable")
        || lower.contains("app-unavailable")
        || lower.contains("claude is only available in certain regions")
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("地区不支持");
    }
    if obs.status == 429 {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail("请求限流 (429)");
    }
    if obs.cf_mitigated {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail(format!("需人机验证 ({})", obs.status));
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_gemini(obs: Option<&HttpObservation>) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    if loc.contains("/sorry") || obs.status == 429 || obs.cf_mitigated {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited).with_detail("需人机验证");
    }
    if loc.contains("unsupported") || loc.contains("/faq") || obs.status == 403 {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("地区不支持");
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_simple_api(obs: Option<&HttpObservation>, label: &str) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("{label} 不可达"));
    };
    let lower = obs.snippet.to_ascii_lowercase();
    if obs.cf_mitigated || obs.status == 429 || (obs.status == 403 && lower.contains("rate limit"))
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail(format!("触发限流 ({})", obs.status));
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_netflix(
    licensed: Option<&HttpObservation>,
    original: Option<&HttpObservation>,
    egress_region: Option<&str>,
) -> ServiceCheckResult {
    let region = egress_region.map(str::to_owned);
    if let Some(obs) = licensed {
        if obs.status == 200 {
            return ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
                .with_region(region)
                .with_detail("完整解锁");
        }
        if matches!(obs.status, 301 | 302 | 307 | 308) {
            let loc = obs.location.as_deref().unwrap_or("");
            if !loc.to_ascii_lowercase().contains("unavailable") {
                return ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
                    .with_region(region)
                    .with_detail("完整解锁");
            }
        }
    }
    if let Some(obs) = original
        && (200..400).contains(&obs.status)
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_region(region)
            .with_detail("仅自制剧");
    }
    ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("未解锁")
}

fn evaluate_youtube(
    obs: Option<&HttpObservation>,
    egress_region: Option<&str>,
) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let lower = obs.snippet.to_ascii_lowercase();
    if lower.contains("youtube premium is not available in your country")
        || lower.contains("this offer is not available")
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_region(egress_region.map(str::to_owned))
            .with_detail("区域不支持 Premium");
    }
    if (200..400).contains(&obs.status) && !obs.cf_mitigated {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
            .with_region(egress_region.map(str::to_owned))
    } else if obs.status == 429 || obs.cf_mitigated {
        ServiceCheckResult::new(ReachabilityCheckStatus::Limited).with_detail("需人机验证")
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_disney(
    obs: Option<&HttpObservation>,
    egress_region: Option<&str>,
) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    if loc.contains("unavailable") || obs.status == 403 || obs.cf_mitigated {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("不可用");
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
            .with_region(egress_region.map(str::to_owned))
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_tiktok(
    obs: Option<&HttpObservation>,
    egress_region: Option<&str>,
) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    if obs.status == 403 || obs.cf_mitigated || loc.contains("unavailable") {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("受限");
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
            .with_region(egress_region.map(str::to_owned))
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_web_clean(
    google: Option<&HttpObservation>,
    trace_cf_mitigated: bool,
) -> ServiceCheckResult {
    let Some(obs) = google else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    if loc.contains("/sorry") || obs.status == 429 || obs.cf_mitigated || trace_cf_mitigated {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited).with_detail("需人机验证");
    }
    if obs.status == 204 || (200..300).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Limited)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

fn evaluate_scholar(obs: Option<&HttpObservation>) -> ServiceCheckResult {
    let Some(obs) = obs else {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("连接失败");
    };
    let loc = obs.location.as_deref().unwrap_or("").to_ascii_lowercase();
    let lower = obs.snippet.to_ascii_lowercase();
    if loc.contains("/sorry")
        || obs.status == 429
        || obs.cf_mitigated
        || lower.contains("unusual traffic")
        || (obs.status == 403 && lower.contains("captcha"))
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Limited).with_detail("需人机验证");
    }
    if obs.status == 403
        || lower.contains("automated queries")
        || lower.contains("can't process your request")
    {
        return ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail("访问受限 (403)");
    }
    if (200..400).contains(&obs.status) {
        ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
    } else {
        ServiceCheckResult::new(ReachabilityCheckStatus::Blocked)
            .with_detail(format!("HTTP {}", obs.status))
    }
}

/// Sends a single RFC 5389 STUN Binding Request via SOCKS5 `UDP ASSOCIATE`
/// (`127.0.0.1:socks_port`) without bypassing the proxy.
pub async fn socks5_stun_probe(
    socks_port: u16,
    stun_host: &str,
    stun_port: u16,
    timeout: Duration,
) -> ServiceCheckResult {
    match tokio::time::timeout(timeout, socks5_stun_inner(socks_port, stun_host, stun_port)).await {
        Ok(Ok(rtt_ms)) => ServiceCheckResult::new(ReachabilityCheckStatus::Pass)
            .with_detail(format!("UDP 可达 ({rtt_ms} ms)")),
        Ok(Err(err)) => {
            ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail(err.to_string())
        }
        Err(_) => {
            ServiceCheckResult::new(ReachabilityCheckStatus::Blocked).with_detail("UDP 探测超时")
        }
    }
}

async fn socks5_stun_inner(socks_port: u16, stun_host: &str, stun_port: u16) -> Result<u32> {
    let mut tcp = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, socks_port)).await?;
    // Greeting: SOCKS5, 1 method, No Auth (0x00)
    tcp.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut greeting_reply = [0u8; 2];
    tcp.read_exact(&mut greeting_reply).await?;
    if greeting_reply != [0x05, 0x00] {
        return Err(Error::other("SOCKS5 握手拒绝"));
    }

    // Request: UDP ASSOCIATE (0x03) with 0.0.0.0:0
    tcp.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let mut hdr = [0u8; 4];
    tcp.read_exact(&mut hdr).await?;
    if hdr[0] != 0x05 || hdr[1] != 0x00 {
        return Err(Error::other(format!("SOCKS5 UDP 关联失败 ({})", hdr[1])));
    }
    let relay_addr = read_socks5_addr(&mut tcp, hdr[3]).await?;
    let relay_ip = if relay_addr.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        relay_addr.ip()
    };
    let relay = SocketAddr::new(relay_ip, relay_addr.port());

    let bind_addr: SocketAddr = match relay {
        SocketAddr::V4(_) => (Ipv4Addr::LOCALHOST, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::LOCALHOST, 0).into(),
    };
    let udp = tokio::net::UdpSocket::bind(bind_addr).await?;
    udp.connect(relay).await?;

    let tx_id: [u8; 12] = uuid::Uuid::new_v4().as_bytes()[..12]
        .try_into()
        .expect("12 bytes");
    let mut stun_req = [0u8; 20];
    stun_req[0..2].copy_from_slice(&[0x00, 0x01]); // Binding Request
    stun_req[2..4].copy_from_slice(&[0x00, 0x00]); // Length = 0
    stun_req[4..8].copy_from_slice(&STUN_MAGIC_COOKIE);
    stun_req[8..20].copy_from_slice(&tx_id);

    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&[0x00, 0x00, 0x00]); // RSV + FRAG=0
    encode_socks5_target(&mut packet, stun_host, stun_port)?;
    packet.extend_from_slice(&stun_req);

    let started = Instant::now();
    udp.send(&packet).await?;

    let mut buf = [0u8; 1500];
    let n = udp.recv(&mut buf).await?;
    let rtt = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    let payload = strip_socks5_udp_header(&buf[..n])?;
    if payload.len() < 20
        || payload[0..2] != [0x01, 0x01]
        || payload[4..8] != STUN_MAGIC_COOKIE
        || payload[8..20] != tx_id
    {
        return Err(Error::other("STUN 响应无效"));
    }
    Ok(rtt)
}

async fn read_socks5_addr(tcp: &mut tokio::net::TcpStream, atyp: u8) -> Result<SocketAddr> {
    let ip = match atyp {
        0x01 => {
            let mut b = [0u8; 4];
            tcp.read_exact(&mut b).await?;
            IpAddr::V4(Ipv4Addr::from(b))
        }
        0x04 => {
            let mut b = [0u8; 16];
            tcp.read_exact(&mut b).await?;
            IpAddr::V6(Ipv6Addr::from(b))
        }
        0x03 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            tcp.read_exact(&mut domain).await?;
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        }
        _ => return Err(Error::other("SOCKS5 地址类型无效")),
    };
    let mut port_bytes = [0u8; 2];
    tcp.read_exact(&mut port_bytes).await?;
    Ok(SocketAddr::new(ip, u16::from_be_bytes(port_bytes)))
}

fn encode_socks5_target(out: &mut Vec<u8>, host: &str, port: u16) -> Result<()> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) => {
                out.push(0x01);
                out.extend_from_slice(&v4.octets());
            }
            IpAddr::V6(v6) => {
                out.push(0x04);
                out.extend_from_slice(&v6.octets());
            }
        }
    } else {
        let bytes = host.as_bytes();
        if bytes.is_empty() || bytes.len() > 255 {
            return Err(Error::other("STUN 主机名长度无效"));
        }
        out.push(0x03);
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&port.to_be_bytes());
    Ok(())
}

fn strip_socks5_udp_header(packet: &[u8]) -> Result<&[u8]> {
    if packet.len() < 4 || packet[2] != 0x00 {
        return Err(Error::other("SOCKS5 UDP 包头无效"));
    }
    let offset = match packet[3] {
        0x01 => 4 + 4 + 2,
        0x04 => 4 + 16 + 2,
        0x03 => {
            if packet.len() < 5 {
                return Err(Error::other("SOCKS5 UDP 域名长度缺失"));
            }
            4 + 1 + packet[4] as usize + 2
        }
        _ => return Err(Error::other("SOCKS5 UDP 地址类型无效")),
    };
    if packet.len() < offset {
        return Err(Error::other("SOCKS5 UDP 包截断"));
    }
    Ok(&packet[offset..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifiers_handle_pass_challenge_and_region_blocks_without_retries() {
        let ok = HttpObservation {
            status: 200,
            location: None,
            cf_mitigated: false,
            snippet: "ok".into(),
            bytes: 2,
            rtt_ms: 45,
        };
        assert_eq!(
            evaluate_chatgpt(Some(&ok)).status,
            ReachabilityCheckStatus::Pass
        );

        let challenge = HttpObservation {
            status: 403,
            location: None,
            cf_mitigated: true,
            snippet: String::new(),
            bytes: 0,
            rtt_ms: 30,
        };
        assert_eq!(
            evaluate_chatgpt(Some(&challenge)).status,
            ReachabilityCheckStatus::Limited
        );
        let claude_challenge = evaluate_claude(Some(&challenge));
        assert_eq!(claude_challenge.status, ReachabilityCheckStatus::Limited);
        assert_eq!(claude_challenge.detail.as_deref(), Some("需人机验证 (403)"));

        let geo_blocked = HttpObservation {
            status: 403,
            location: None,
            cf_mitigated: false,
            snippet: r#"{"error":"unsupported_country"}"#.into(),
            bytes: 31,
            rtt_ms: 25,
        };
        assert_eq!(
            evaluate_chatgpt(Some(&geo_blocked)).status,
            ReachabilityCheckStatus::Blocked
        );

        let google_sorry = HttpObservation {
            status: 302,
            location: Some("https://www.google.com/sorry/index".into()),
            cf_mitigated: false,
            snippet: String::new(),
            bytes: 0,
            rtt_ms: 40,
        };
        assert_eq!(
            evaluate_gemini(Some(&google_sorry)).status,
            ReachabilityCheckStatus::Limited
        );
        assert_eq!(
            evaluate_web_clean(Some(&google_sorry), false).status,
            ReachabilityCheckStatus::Limited
        );
        assert_eq!(
            evaluate_scholar(Some(&ok)).status,
            ReachabilityCheckStatus::Pass
        );
        assert_eq!(
            evaluate_scholar(Some(&google_sorry)).status,
            ReachabilityCheckStatus::Limited
        );
        let scholar_blocked = HttpObservation {
            status: 403,
            location: None,
            cf_mitigated: false,
            snippet: "We're sorry... automated queries. We can't process your request right now."
                .into(),
            bytes: 74,
            rtt_ms: 40,
        };
        assert_eq!(
            evaluate_scholar(Some(&scholar_blocked)).status,
            ReachabilityCheckStatus::Blocked
        );

        let not_found = HttpObservation {
            status: 404,
            location: None,
            cf_mitigated: false,
            snippet: String::new(),
            bytes: 0,
            rtt_ms: 30,
        };
        assert_eq!(
            evaluate_netflix(Some(&not_found), Some(&ok), Some("SG")).status,
            ReachabilityCheckStatus::Limited
        );
        assert_eq!(
            evaluate_netflix(Some(&ok), None, Some("JP")).status,
            ReachabilityCheckStatus::Pass
        );

        let gh_rate_limit = HttpObservation {
            status: 403,
            location: None,
            cf_mitigated: false,
            snippet: r#"{"message":"API rate limit exceeded"}"#.into(),
            bytes: 37,
            rtt_ms: 40,
        };
        assert_eq!(
            evaluate_simple_api(Some(&gh_rate_limit), "GitHub").status,
            ReachabilityCheckStatus::Limited
        );
    }

    #[tokio::test]
    async fn socks5_udp_associate_completes_stun_binding_against_local_mock() {
        let udp_relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_port = udp_relay.local_addr().unwrap().port();
        let tcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks_port = tcp_listener.local_addr().unwrap().port();

        let tcp_task = tokio::spawn(async move {
            let (mut socket, _) = tcp_listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            socket.read_exact(&mut greet).await.unwrap();
            assert_eq!(greet, [0x05, 0x01, 0x00]);
            socket.write_all(&[0x05, 0x00]).await.unwrap();
            let mut req = [0u8; 10];
            socket.read_exact(&mut req).await.unwrap();
            assert_eq!(req[1], 0x03);
            let mut reply = vec![0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1];
            reply.extend_from_slice(&relay_port.to_be_bytes());
            socket.write_all(&reply).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let udp_task = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            let (n, peer) = udp_relay.recv_from(&mut buf).await.unwrap();
            let req = strip_socks5_udp_header(&buf[..n]).unwrap();
            assert_eq!(&req[0..2], &[0x00, 0x01]);
            assert_eq!(&req[4..8], &STUN_MAGIC_COOKIE);
            let mut resp = Vec::new();
            resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0x0D, 0x96]);
            resp.extend_from_slice(&[0x01, 0x01, 0x00, 0x00]);
            resp.extend_from_slice(&STUN_MAGIC_COOKIE);
            resp.extend_from_slice(&req[8..20]);
            udp_relay.send_to(&resp, peer).await.unwrap();
        });

        let result = socks5_stun_probe(socks_port, "127.0.0.1", 3478, Duration::from_secs(2)).await;
        assert_eq!(result.status, ReachabilityCheckStatus::Pass);
        tcp_task.await.unwrap();
        udp_task.await.unwrap();
    }
}
