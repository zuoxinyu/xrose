//! "Test this link" tracer.
//!
//! Executes a real request through the local proxy when the core is running,
//! extracts the matched Clash rule and outbound chain for TCP/UDP traffic, and
//! publishes each stage for the animated graph.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use url::Url;

use crate::model::{AppModel, CoreKind, DnsServer, MeshDnsMode, Node, RouteActionKind, WorkMode};
use crate::route::{self, ConnectionInfo};
use crate::runtime::clash::{ClashClient, ClashConnection};
use crate::service::AppService;
use crate::settings::Settings;
use crate::util::{format_bytes, now_unix};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceProtocol {
    Http,
    Https,
    Tcp,
    Udp,
    Icmp,
}

impl TraceProtocol {
    pub fn label(&self) -> &'static str {
        match self {
            TraceProtocol::Http => "HTTP",
            TraceProtocol::Https => "HTTPS",
            TraceProtocol::Tcp => "TCP",
            TraceProtocol::Udp => "UDP",
            TraceProtocol::Icmp => "ICMP",
        }
    }
    pub fn icon(&self) -> &'static str {
        match self {
            TraceProtocol::Http | TraceProtocol::Https => "⇄",
            TraceProtocol::Tcp => "◇",
            TraceProtocol::Udp => "≋",
            TraceProtocol::Icmp => "◉",
        }
    }
    fn is_stream(&self) -> bool {
        matches!(
            self,
            TraceProtocol::Tcp | TraceProtocol::Http | TraceProtocol::Https
        )
    }
    fn scheme(&self) -> &'static str {
        match self {
            TraceProtocol::Http => "http",
            TraceProtocol::Https => "https",
            TraceProtocol::Tcp => "tcp",
            TraceProtocol::Udp => "udp",
            TraceProtocol::Icmp => "icmp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceTarget {
    pub protocol: TraceProtocol,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub url: Option<String>,
    pub original: String,
}

impl TraceTarget {
    pub fn display(&self) -> String {
        match self.protocol {
            TraceProtocol::Icmp => format!("icmp://{}", self.host),
            _ => format!(
                "{}://{}:{}{}",
                self.protocol.scheme(),
                self.host,
                self.port,
                self.path
            ),
        }
    }
}

pub fn parse_target(input: &str) -> Result<TraceTarget, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("请输入要测试的链接".into());
    }
    let prepared = if trimmed.contains("://") {
        trimmed.to_string()
    } else if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.contains(':') {
        format!("tcp://{trimmed}")
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&prepared).map_err(|err| format!("链接格式无效：{err}"))?;
    let protocol = match url.scheme().to_ascii_lowercase().as_str() {
        "http" => TraceProtocol::Http,
        "https" => TraceProtocol::Https,
        "tcp" | "tcping" => TraceProtocol::Tcp,
        "udp" => TraceProtocol::Udp,
        "icmp" | "ping" => TraceProtocol::Icmp,
        other => return Err(format!("暂不支持 {other}，请使用 https/tcp/udp/icmp")),
    };
    let host = url
        .host_str()
        .ok_or_else(|| "链接缺少主机名".to_string())?
        .trim_matches(['[', ']'])
        .to_string();
    if host.is_empty() {
        return Err("链接缺少主机名".into());
    }
    let default_port = match protocol {
        TraceProtocol::Http => 80,
        TraceProtocol::Https => 443,
        TraceProtocol::Tcp => 443,
        TraceProtocol::Udp => 53,
        TraceProtocol::Icmp => 0,
    };
    let port = url.port().unwrap_or(default_port);
    if protocol != TraceProtocol::Icmp && port == 0 {
        return Err("端口不能为 0".into());
    }
    let path = if protocol == TraceProtocol::Icmp {
        String::new()
    } else {
        let mut path = url.path().to_string();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        path
    };
    let http_url = matches!(protocol, TraceProtocol::Http | TraceProtocol::Https).then(|| {
        let mut value = url.clone();
        value.set_fragment(None);
        value.to_string()
    });
    Ok(TraceTarget {
        protocol,
        host,
        port,
        path,
        url: http_url,
        original: trimmed.to_string(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStepKind {
    Source,
    Inbound,
    Dns,
    Route,
    Outbound,
    Remote,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Pending,
    Active,
    Ok,
    Warn,
    Error,
    Skipped,
}

impl TraceStatus {
    pub fn is_done(&self) -> bool {
        !matches!(self, TraceStatus::Pending | TraceStatus::Active)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    pub kind: TraceStepKind,
    pub title: String,
    pub detail: String,
    pub status: TraceStatus,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePhase {
    Request,
    Response,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSession {
    pub id: u64,
    pub target: String,
    pub protocol: TraceProtocol,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub steps: Vec<TraceStep>,
    pub phase: TracePhase,
    pub success: Option<bool>,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub request_ms: Option<u64>,
    pub response_ms: Option<u64>,
    pub resolved_ips: Vec<String>,
    pub dns_server: Option<String>,
    pub node_name: Option<String>,
    pub group_name: Option<String>,
    pub rule: Option<String>,
    #[serde(default)]
    pub blocked_rule_id: Option<String>,
}

impl TraceSession {
    pub fn new(id: u64, target: &TraceTarget) -> Self {
        let step = |kind, title: &str| TraceStep {
            kind,
            title: title.into(),
            detail: String::new(),
            status: TraceStatus::Pending,
            duration_ms: None,
        };
        Self {
            id,
            target: target.original.clone(),
            protocol: target.protocol,
            host: target.host.clone(),
            port: target.port,
            path: target.path.clone(),
            steps: vec![
                step(TraceStepKind::Source, "请求源"),
                step(TraceStepKind::Inbound, "入站"),
                step(TraceStepKind::Dns, "DNS 解析"),
                step(TraceStepKind::Route, "路由 / 分流"),
                step(TraceStepKind::Outbound, "出站节点"),
                step(TraceStepKind::Remote, "目标服务"),
                step(TraceStepKind::Response, "响应返回"),
            ],
            phase: TracePhase::Request,
            success: None,
            error: None,
            summary: None,
            started_at: now_unix(),
            finished_at: None,
            request_ms: None,
            response_ms: None,
            resolved_ips: Vec::new(),
            dns_server: None,
            node_name: None,
            group_name: None,
            rule: None,
            blocked_rule_id: None,
        }
    }

    pub fn step(&self, kind: TraceStepKind) -> Option<&TraceStep> {
        self.steps.iter().find(|step| step.kind == kind)
    }

    fn step_mut(&mut self, kind: TraceStepKind) -> &mut TraceStep {
        self.steps
            .iter_mut()
            .find(|step| step.kind == kind)
            .expect("trace session has every step")
    }

    fn activate(&mut self, kind: TraceStepKind, detail: impl Into<String>) {
        let step = self.step_mut(kind);
        step.status = TraceStatus::Active;
        step.detail = detail.into();
    }

    fn set(
        &mut self,
        kind: TraceStepKind,
        status: TraceStatus,
        detail: impl Into<String>,
        duration_ms: Option<u64>,
    ) {
        let step = self.step_mut(kind);
        step.status = status;
        step.detail = detail.into();
        step.duration_ms = duration_ms;
    }

    fn finish_failure(&mut self, error: String) {
        if let Some(step) = self
            .steps
            .iter_mut()
            .rev()
            .find(|step| step.status == TraceStatus::Active)
        {
            step.status = TraceStatus::Error;
            step.detail = error.clone();
        }
        for step in &mut self.steps {
            if !step.status.is_done() {
                step.status = TraceStatus::Skipped;
                if step.detail.is_empty() {
                    step.detail = "未执行".into();
                }
            }
        }
        self.phase = TracePhase::Failed;
        self.success = Some(false);
        self.error = Some(error.clone());
        self.summary = Some(format!("测试失败：{error}"));
        self.finished_at = Some(now_unix());
    }
}

pub async fn run(service: AppService, mut session: TraceSession) {
    if let Err(err) = run_inner(&service, &mut session).await {
        // Cancellation is not a user-visible failure.
        if err == "cancelled" {
            return;
        }
        session.finish_failure(err);
        service.publish_trace(&session);
    }
}

async fn run_inner(service: &AppService, session: &mut TraceSession) -> Result<(), String> {
    let target = parse_target(&session.target)?;
    let core = service.core();
    let running = service.is_running();
    let inbound_kind = match target.protocol {
        TraceProtocol::Http | TraceProtocol::Https => "http",
        TraceProtocol::Icmp => "tun",
        _ => "socks",
    };
    let (model, settings, mut active_inbound) = running
        .then(|| core.trace_context(inbound_kind))
        .flatten()
        .unwrap_or_else(|| (service.model(), service.settings(), None));
    let mesh_mode = model.work_mode == WorkMode::Mesh;
    if mesh_mode {
        active_inbound = core
            .trace_context("tun")
            .and_then(|(_, _, inbound)| inbound);
    }
    let id = session.id;
    let overall = Instant::now();

    // Source -----------------------------------------------------------
    session.activate(
        TraceStepKind::Source,
        format!("Xrose 测试链接 · {}", target.display()),
    );
    publish(service, session)?;
    sleep(120).await;
    session.set(
        TraceStepKind::Source,
        TraceStatus::Ok,
        format!(
            "Xrose · PID {} · 手动请求 · {}",
            std::process::id(),
            target.protocol.label()
        ),
        None,
    );

    // Inbound ----------------------------------------------------------
    let inbound = match active_inbound.as_ref() {
        Some(i) if mesh_mode => (true, format!("组网模式 · 系统 TUN · {}", i.tag)),
        Some(i) => (true, format!("{} · {} :{}", i.kind, i.tag, i.port)),
        None if !running => (false, "内核未运行 · 无活动入站".into()),
        None => (false, format!("没有启用的 {inbound_kind} 入站")),
    };
    session.activate(TraceStepKind::Inbound, inbound.1.clone());
    publish(service, session)?;
    let dns_config = if mesh_mode {
        service
            .mesh_diagnostic_config(&model, &settings)
            .map_err(|e| e.to_string())?
            .0
            .dns
    } else {
        model.dns.clone()
    };
    sleep(100).await;
    session.set(
        TraceStepKind::Inbound,
        if inbound.0 {
            TraceStatus::Ok
        } else {
            TraceStatus::Warn
        },
        inbound.1.clone(),
        None,
    );

    // DNS --------------------------------------------------------------
    let dns_started = Instant::now();
    session.activate(TraceStepKind::Dns, "正在解析域名…");
    publish(service, session)?;
    let host_ip = target.host.parse::<IpAddr>().ok();
    let mut resolved_ip = host_ip;
    let mut dns_failure = None;
    if let Some(ip) = host_ip {
        session.resolved_ips = vec![ip.to_string()];
        session.set(
            TraceStepKind::Dns,
            TraceStatus::Skipped,
            format!("{ip} 是 IP，无需 DNS 解析"),
            Some(0),
        );
    } else {
        let lookup = tokio::time::timeout(Duration::from_secs(5), async {
            if mesh_mode {
                crate::dns::probe::mesh_lookup(&target.host)
                    .await
                    .map_err(|e| std::io::Error::other(e.to_string()))
            } else {
                tokio::net::lookup_host((target.host.as_str(), target.port.max(1)))
                    .await
                    .map(|iter| iter.map(|addr| addr.ip()).collect())
            }
        })
        .await;
        match lookup {
            Ok(Ok(mut ips)) => {
                ips.dedup();
                if ips.is_empty() {
                    let msg = "DNS 未返回任何地址".to_string();
                    session.set(
                        TraceStepKind::Dns,
                        TraceStatus::Error,
                        &msg,
                        Some(dns_started.elapsed().as_millis() as u64),
                    );
                    return Err(msg);
                }
                resolved_ip = ips.first().copied();
                session.resolved_ips = ips.iter().map(|ip| ip.to_string()).collect();
                let server = (!mesh_mode)
                    .then(|| choose_dns_server(&model, &target.host))
                    .flatten();
                let resolver = if mesh_mode {
                    match model.mesh_mode.dns {
                        MeshDnsMode::Exit => "出口 DNS（经组网）",
                        MeshDnsMode::Custom => "自定义 DNS（经组网）",
                    }
                } else {
                    "系统解析器"
                };
                session.dns_server = Some(resolver.into());
                let fake = if dns_config.fake_ip {
                    " · 已启用 Fake-IP"
                } else {
                    ""
                };
                let detail = format!(
                    "{resolver}：{} → {}{}{}",
                    target.host,
                    ips.iter()
                        .take(3)
                        .map(|ip| ip.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    server
                        .map(|s| format!(" · 匹配服务器：{} ({})", s.tag, s.display_address()))
                        .unwrap_or_default(),
                    fake
                );
                session.set(
                    TraceStepKind::Dns,
                    TraceStatus::Ok,
                    detail,
                    Some(dns_started.elapsed().as_millis() as u64),
                );
            }
            Ok(Err(err)) => {
                let msg = format!("DNS 解析失败：{err}");
                if mesh_mode {
                    return Err(msg);
                }
                session.set(
                    TraceStepKind::Dns,
                    if running {
                        TraceStatus::Warn
                    } else {
                        TraceStatus::Error
                    },
                    if running {
                        format!("本机预解析失败：{err}；继续交由 core 解析")
                    } else {
                        msg.clone()
                    },
                    Some(dns_started.elapsed().as_millis() as u64),
                );
                if !running {
                    dns_failure = Some(msg);
                }
            }
            Err(_) => {
                let msg = "DNS 解析超时（5 秒）".to_string();
                if mesh_mode {
                    return Err(msg);
                }
                session.set(
                    TraceStepKind::Dns,
                    if running {
                        TraceStatus::Warn
                    } else {
                        TraceStatus::Error
                    },
                    if running {
                        "本机预解析超时；继续交由 core 解析"
                    } else {
                        &msg
                    },
                    Some(5000),
                );
                if !running {
                    dns_failure = Some(msg);
                }
            }
        }
    }
    publish(service, session)?;

    // Route preview ----------------------------------------------------
    let network = if target.protocol == TraceProtocol::Udp {
        crate::model::Network::Udp
    } else {
        crate::model::Network::Tcp
    };
    let process = std::env::current_exe().ok();
    let protocol = match target.protocol {
        TraceProtocol::Http => Some("http"),
        TraceProtocol::Https => Some("tls"),
        TraceProtocol::Udp if target.port == 53 => Some("dns"),
        _ => None,
    };
    let info = ConnectionInfo {
        domain: Some(target.host.clone()),
        destination_ip: resolved_ip,
        destination_port: Some(target.port),
        network: (target.protocol != TraceProtocol::Icmp).then_some(network),
        process_name: process
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned()),
        process_path: process.map(|p| p.to_string_lossy().into_owned()),
        protocol: protocol.map(str::to_owned),
        inbound_tag: active_inbound.as_ref().map(|i| i.tag.clone()),
        ..Default::default()
    };
    let plan = core.mesh_plan();
    let mesh = info.destination_ip.and_then(|ip| {
        plan.routes
            .iter()
            .find(|route| route::cidr_contains(&route.cidr, ip))
    });
    let mut preview = explain_route(&model, &info, mesh);
    if preview.needs_core
        && model.work_mode == crate::model::WorkMode::Direct
        && settings.core == CoreKind::SingBox
    {
        // The installed binary can inspect a cached rule set without running the core.
        // A missing cache or incompatible binary must remain an unknown decision.
        if let Ok(binary) = core.resolve_binary(CoreKind::SingBox, &settings) {
            let dir = service.paths().rule_sets_dir();
            let mut matches = std::collections::BTreeMap::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Some(rule) = preview.rule.as_ref().filter(|_| preview.needs_core) {
                let Ok(Some(hit)) = tokio::time::timeout_at(
                    deadline,
                    cached_set_match(&binary, &dir, &model, &rule.matchers, &info),
                )
                .await
                else {
                    break;
                };
                matches.insert(rule.id.clone(), hit);
                preview = route::evaluate_with(&model, &info, |r| matches.get(&r.id).copied());
            }
        }
    }
    let effective_action = if preview.needs_core {
        route::effective_action(&model, &model.routing.final_action)
    } else {
        preview.action.clone()
    };
    let action_label = if mesh_mode {
        "组网转发".into()
    } else {
        route::action_label(&model, &effective_action)
    };
    let group = effective_action
        .group
        .clone()
        .and_then(|gid| model.group(&gid).cloned())
        .or_else(|| {
            model
                .default_group_id()
                .and_then(|gid| model.group(&gid).cloned())
        });
    if let Some(group) = group
        .as_ref()
        .filter(|_| !mesh_mode && effective_action.kind == RouteActionKind::Proxy)
    {
        session.group_name = Some(group.name.clone());
        if let Some(node_id) = group.active_node() {
            session.node_name = model.node(node_id).map(|n| n.name.clone());
        }
    }
    if mesh_mode {
        session.node_name = model
            .mesh_mode
            .target
            .as_ref()
            .map(|target| target.name.clone());
    }
    session.activate(TraceStepKind::Route, "正在匹配路由规则…");
    publish(service, session)?;
    sleep(110).await;
    let route_detail = if preview.needs_core && !running {
        format!("依赖规则集数据（内核未运行），按默认出站预览 · {action_label}")
    } else {
        format!("规则预览：{} · {}", preview.explanation, action_label)
    };
    session.rule = preview.rule.as_ref().map(|r| r.name.clone());
    session.set(
        TraceStepKind::Route,
        if preview.needs_core {
            TraceStatus::Warn
        } else if effective_action.kind == RouteActionKind::Block {
            TraceStatus::Error
        } else {
            TraceStatus::Ok
        },
        route_detail.clone(),
        None,
    );

    if !preview.needs_core
        && effective_action.kind == RouteActionKind::Block
        && (!running || model.work_mode == crate::model::WorkMode::Direct)
    {
        session.set(
            TraceStepKind::Outbound,
            TraceStatus::Skipped,
            "规则阻止，未发起出站连接",
            None,
        );
        session.set(
            TraceStepKind::Remote,
            TraceStatus::Skipped,
            "请求被路由规则拦截",
            None,
        );
        session.blocked_rule_id = preview.rule.as_ref().map(|rule| rule.id.clone());
        session.finish_failure(format!("{} · 已阻止", preview.explanation));
        publish(service, session)?;
        service.logs().push(
            crate::core::logs::LogStream::App,
            crate::settings::LogLevel::Info,
            format!("测试链接：{} → 已按规则阻止", target.display()),
        );
        return Ok(());
    }
    publish(service, session)?;

    if let Some(failure) = dns_failure {
        return Err(failure);
    }
    if !running && preview.needs_core && model.work_mode == crate::model::WorkMode::Direct {
        return Err(format!(
            "无法确认拦截规则“{}”，请下载对应规则集后重试",
            session.rule.as_deref().unwrap_or("")
        ));
    }

    // Outbound ---------------------------------------------------------
    let direct = effective_action.kind == RouteActionKind::Direct;
    let outbound_label = if mesh_mode {
        model
            .mesh_mode
            .target
            .as_ref()
            .map(|target| {
                format!(
                    "{} · {} 组网出口（配置）",
                    target.name,
                    target.provider.label()
                )
            })
            .unwrap_or_default()
    } else if let Some(route) = mesh {
        format!("组网接口 {} · {}", route.interface, route.cidr)
    } else if direct {
        "本机直连".to_string()
    } else {
        match (&session.group_name, &session.node_name) {
            (Some(group), Some(node)) => format!("{group} → {node}"),
            (Some(group), None) => group.clone(),
            _ => "代理出站".into(),
        }
    };
    session.activate(TraceStepKind::Outbound, outbound_label.clone());
    publish(service, session)?;
    if mesh_mode {
        service
            .mesh_diagnostic_config(&model, &settings)
            .map_err(|e| e.to_string())?;
        if target.protocol == TraceProtocol::Icmp {
            return Err(
                "当前组网出口转发链路不提供可验证的 ICMP 探测，请使用 TCP、HTTPS 或 UDP".into(),
            );
        }
        if !core.mesh_exit_runtime().is_some_and(|runtime| runtime.ipv6)
            && !session
                .resolved_ips
                .iter()
                .any(|ip| ip.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv4()))
        {
            return Err("当前组网出口仅支持 IPv4，目标没有可用的 IPv4 地址".into());
        }
        session.set(
            TraceStepKind::Outbound,
            TraceStatus::Ok,
            format!("{outbound_label} · 请求经系统 TUN 发出；远端分流结果不在本机展示"),
            None,
        );
    }

    if !running {
        session.set(
            TraceStepKind::Outbound,
            TraceStatus::Skipped,
            format!("{outbound_label} · 内核未运行，本次请求由本机发出"),
            None,
        );
        session.set(
            TraceStepKind::Remote,
            TraceStatus::Active,
            "core 未运行，执行本机直连测试…",
            None,
        );
    }

    let http = core.active_proxy("http");
    let socks = core.active_proxy("socks");
    // A running core must evaluate its own applied rules, including direct and
    // reject actions. The editable model may contain unapplied changes.
    let use_proxy = running && !mesh_mode && target.protocol != TraceProtocol::Icmp;
    let used_direct = !use_proxy && !mesh_mode;
    if use_proxy && socks.is_none() && target.protocol.is_stream() {
        session.set(
            TraceStepKind::Outbound,
            TraceStatus::Warn,
            "没有可用的 SOCKS/mixed 入站，无法通过 core 建立 TCP 测试连接",
            None,
        );
    }

    // Remote -----------------------------------------------------------
    if !running {
        session.activate(TraceStepKind::Remote, "core 未运行，本机直连…");
    } else {
        session.activate(
            TraceStepKind::Remote,
            format!(
                "通过 {outbound_label} 访问 {}:{}…",
                target.host, target.port
            ),
        );
    }
    publish(service, session)?;
    sleep(90).await;

    let mesh_addresses: Vec<IpAddr> = if mesh_mode {
        session
            .resolved_ips
            .iter()
            .filter_map(|ip| ip.parse::<IpAddr>().ok())
            .filter(|ip| {
                ip.is_ipv4() || core.mesh_exit_runtime().is_some_and(|runtime| runtime.ipv6)
            })
            .collect()
    } else {
        Vec::new()
    };
    let resolved = mesh_mode.then_some(mesh_addresses.as_slice());
    let remote = match target.protocol {
        TraceProtocol::Http | TraceProtocol::Https => {
            if use_proxy && http.is_none() {
                return Err(
                    "没有启用的 HTTP/mixed 入站，无法经 core 测试 HTTP；请启用入站后重试".into(),
                );
            }
            let http_proxy = if use_proxy { http.as_ref() } else { None };
            http_test(&settings, &target, http_proxy, resolved).await
        }
        TraceProtocol::Tcp => {
            if use_proxy {
                match socks.as_ref() {
                    Some(socks) => {
                        tcp_probe(
                            service,
                            &settings,
                            socks,
                            &target,
                            running && settings.core == CoreKind::SingBox,
                        )
                        .await
                    }
                    None => return Err("没有启用的 SOCKS/mixed 入站，无法经 core 测试 TCP".into()),
                }
            } else {
                tcp_direct(&target, resolved).await
            }
        }
        TraceProtocol::Udp => {
            if use_proxy {
                match socks.as_ref() {
                    Some(socks) => {
                        udp_probe(
                            service,
                            &settings,
                            socks,
                            &target,
                            running && settings.core == CoreKind::SingBox,
                        )
                        .await
                    }
                    None => return Err("没有启用的 SOCKS/mixed 入站，无法经 core 测试 UDP".into()),
                }
            } else {
                udp_direct(&target, resolved).await
            }
        }
        TraceProtocol::Icmp => icmp_test(&target, running, &model).await,
    };
    publish(service, session)?;
    if !service.is_trace_current(id) {
        return Err("cancelled".into());
    }

    // Merge live Clash metadata (rule + chains) when available.
    if use_proxy && remote.clash.is_none() {
        session.set(
            TraceStepKind::Outbound,
            TraceStatus::Warn,
            format!("请求已交给 core；未取得关联出站元数据。配置预览：{outbound_label}"),
            None,
        );
    }
    if let Some(connection) = &remote.clash {
        let rule_text = if connection.rule.is_empty() {
            None
        } else if connection.rule_payload.is_empty() {
            Some(connection.rule.clone())
        } else {
            Some(format!("{}({})", connection.rule, connection.rule_payload))
        };
        if let Some(rule) = rule_text {
            session.rule = Some(rule.clone());
            if let Some(step) = session
                .steps
                .iter_mut()
                .find(|s| s.kind == TraceStepKind::Route)
            {
                step.status = TraceStatus::Ok;
                step.detail = format!(
                    "实际匹配：{rule} → {}",
                    session
                        .group_name
                        .clone()
                        .unwrap_or_else(|| action_label.clone())
                );
            }
        }
        let (group_name, node) = chain_info(&model, &connection.chains);
        session.group_name = group_name;
        session.node_name = node.as_ref().map(|node| node.name.clone());
        let chain = connection
            .chains
            .iter()
            .map(|tag| {
                model
                    .nodes
                    .iter()
                    .find(|node| node.outbound_tag() == *tag)
                    .map(|node| format!("{} ({})", node.name, node.protocol.display_name()))
                    .or_else(|| {
                        model
                            .groups
                            .iter()
                            .find(|group| group.tag() == *tag)
                            .map(|group| group.name.clone())
                    })
                    .unwrap_or_else(|| match tag.as_str() {
                        "direct" => "本机直连".into(),
                        tag => tag
                            .strip_prefix("xrose-mesh-")
                            .and_then(|index| plan.routes.get(index.parse::<usize>().ok()?))
                            .map_or_else(
                                || tag.to_owned(),
                                |r| format!("组网接口 {}", r.interface),
                            ),
                    })
            })
            .collect::<Vec<_>>()
            .join(" → ");
        session.set(
            TraceStepKind::Outbound,
            if chain.is_empty() {
                TraceStatus::Warn
            } else {
                TraceStatus::Ok
            },
            if chain.is_empty() {
                "内核未提供出站链".into()
            } else {
                format!("实际出站：{chain}")
            },
            None,
        );
    }

    let remote_duration = remote.duration_ms;
    let remote_detail = remote.detail.clone();
    session.set(
        TraceStepKind::Remote,
        remote.status,
        remote_detail.clone(),
        remote_duration,
    );
    if remote.status == TraceStatus::Ok || remote.status == TraceStatus::Warn {
        session.success = Some(true);
    } else {
        session.success = Some(false);
    }
    if remote.status == TraceStatus::Error {
        session.error = Some(remote_detail.clone());
    }
    session.request_ms = remote_duration;
    publish(service, session)?;

    // Response ---------------------------------------------------------
    session.phase = TracePhase::Response;
    session.activate(TraceStepKind::Response, "响应沿原链路返回 Xrose…");
    publish(service, session)?;
    sleep(220).await;
    let response_ms = remote.duration_ms.unwrap_or(0);
    // The probe measures an end-to-end duration, not a separate return leg.
    session.response_ms = None;
    let actual_direct = used_direct
        || remote
            .clash
            .as_ref()
            .is_some_and(|c| c.chains.iter().any(|tag| tag == "direct"));
    let endpoint = if mesh_mode {
        "组网链路".into()
    } else if actual_direct {
        "直连".to_string()
    } else {
        session
            .node_name
            .clone()
            .map(|node| format!("节点 {node}"))
            .unwrap_or_else(|| "core（实际出站未观测）".into())
    };
    if session.success == Some(true) {
        session.set(
            TraceStepKind::Response,
            TraceStatus::Ok,
            format!(
                "{} → Xrose · 耗时 {response_ms} ms",
                if actual_direct {
                    "目标 → 直连".into()
                } else {
                    format!("目标 → {endpoint}")
                }
            ),
            None,
        );
        session.phase = TracePhase::Done;
        session.summary = Some(format!(
            "{} · {} · 诊断总耗时 {} ms{}",
            session.target,
            remote_detail,
            overall.elapsed().as_millis(),
            if remote.warning.is_some() {
                format!(" · {}", remote.warning.clone().unwrap_or_default())
            } else {
                String::new()
            }
        ));
        session.finished_at = Some(now_unix());
        service.logs().push(
            crate::core::logs::LogStream::App,
            crate::settings::LogLevel::Info,
            format!(
                "测试链接：{} → {}（{} ms）",
                target.display(),
                remote_detail,
                overall.elapsed().as_millis()
            ),
        );
    } else {
        session.phase = TracePhase::Failed;
        session.set(
            TraceStepKind::Response,
            TraceStatus::Error,
            format!("未能建立完整返回路径：{remote_detail}"),
            None,
        );
        session.finished_at = Some(now_unix());
        session.summary = Some(format!("{} · 失败：{remote_detail}", session.target));
        service.logs().push(
            crate::core::logs::LogStream::App,
            crate::settings::LogLevel::Warn,
            format!("测试链接：{} → 失败：{remote_detail}", target.display()),
        );
    }
    publish(service, session)?;
    Ok(())
}

fn trace_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    command.kill_on_drop(true);
    command
}

/// Use sing-box's own rule-set decoder and matcher. Only a bounded status
/// prefix is needed, even when one matching rule contains thousands of domains.
async fn cached_set_match(
    binary: &std::path::Path,
    dir: &std::path::Path,
    model: &AppModel,
    matchers: &crate::model::Matchers,
    info: &ConnectionInfo,
) -> Option<bool> {
    let mut targets = Vec::new();
    if let Some(domain) = &info.domain {
        targets.push(domain.clone());
    }
    if let Some(ip) = info.destination_ip {
        let ip = ip.to_string();
        if !targets.contains(&ip) {
            targets.push(ip);
        }
    }
    let tags = matchers
        .rule_sets
        .iter()
        .cloned()
        .chain(matchers.geosites.iter().map(|s| format!("geosite-{s}")))
        .chain(matchers.geoips.iter().map(|s| format!("geoip-{s}")));
    let mut complete = !targets.is_empty();
    for tag in tags {
        let Some(set) = crate::adapters::resolve_rule_set(model, &tag) else {
            complete = false;
            continue;
        };
        if tag.contains(['/', '\\']) {
            return None;
        }
        let path = dir.join(format!("{tag}.srs"));
        if !path.is_file() {
            complete = false;
            continue;
        }
        for target in &targets {
            use std::process::Stdio;
            let mut child = trace_command(binary)
                .args(["rule-set", "match", "--format", set.format.as_str(), "--"])
                .arg(&path)
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;
            let mut prefix = [0; 12];
            let mut stderr = child.stderr.take()?;
            if stderr.read_exact(&mut prefix).await.is_ok() && &prefix == b"match rules." {
                return Some(true);
            }
            drop(stderr);
            complete &= child.wait().await.ok()?.success();
        }
    }
    complete.then_some(false)
}

fn explain_route(
    model: &AppModel,
    info: &ConnectionInfo,
    mesh: Option<&crate::mesh::coexist::MeshRoute>,
) -> route::RulePreview {
    if model.work_mode == WorkMode::Mesh {
        let mut preview = route::evaluate(model, info);
        preview.explanation.push_str(if model.mesh_mode.allow_lan {
            "；允许本地局域网绕过出口"
        } else {
            "；关闭本地局域网绕过"
        });
        if let Some(mesh) = mesh {
            preview
                .explanation
                .push_str(&format!("；目标命中组网网段 {}", mesh.cidr));
        }
        return preview;
    }
    if let Some(mesh) = mesh {
        return route::RulePreview {
            rule: None,
            action: crate::model::RouteAction::direct(),
            explanation: format!("命中组网网段 {} → {}", mesh.cidr, mesh.interface),
            needs_core: false,
        };
    }
    route::evaluate(model, info)
}

fn publish(service: &AppService, session: &TraceSession) -> Result<(), String> {
    if !service.is_trace_current(session.id) {
        return Err("cancelled".into());
    }
    service.publish_trace(session);
    Ok(())
}

async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn choose_dns_server<'a>(model: &'a AppModel, host: &str) -> Option<&'a DnsServer> {
    let lower = host.to_ascii_lowercase();
    let eligible = |s: &&DnsServer| {
        !s.is_fakeip() && (model.work_mode != crate::model::WorkMode::Direct || !s.requires_proxy())
    };
    let servers = || model.dns.servers.iter().filter(eligible);
    servers()
        .find(|s| s.domains.iter().any(|r| domain_rule_matches(r, &lower)))
        .or_else(|| servers().find(|s| Some(&s.tag) == model.dns.final_server.as_ref()))
        .or_else(|| servers().next())
}

fn domain_rule_matches(rule: &str, host: &str) -> bool {
    let rule = rule.trim();
    if let Some(geo) = rule.strip_prefix("geosite:") {
        return match geo {
            "cn" => host.ends_with(".cn"),
            "geolocation-!cn" => !host.ends_with(".cn"),
            _ => false,
        };
    }
    if let Some(value) = rule
        .strip_prefix("domain:")
        .or_else(|| rule.strip_prefix("full:"))
    {
        return host == value;
    }
    if let Some(value) = rule.strip_prefix("suffix:") {
        return host == value || host.ends_with(&format!(".{value}"));
    }
    if let Some(value) = rule.strip_prefix("keyword:") {
        return host.contains(value);
    }
    if let Some(value) = rule.strip_prefix('.').or(Some(rule)) {
        return host == value || host.ends_with(&format!(".{value}"));
    }
    false
}

fn chain_info(model: &AppModel, chains: &[String]) -> (Option<String>, Option<Node>) {
    let mut group = None;
    let mut node = None;
    for tag in chains {
        if let Some(id) = tag.strip_prefix("group-") {
            if let Some(found) = model.group(id) {
                group = Some(found.name.clone());
            }
        } else if let Some(id) = tag.strip_prefix("node-") {
            if let Some(found) = model.node(id) {
                node = Some(found.clone());
            }
        } else if let Some(found) = model.group(tag) {
            group = Some(found.name.clone());
        } else if let Some(found) = model.node(tag) {
            node = Some(found.clone());
        }
    }
    (group, node)
}

struct RemoteResult {
    status: TraceStatus,
    detail: String,
    duration_ms: Option<u64>,
    warning: Option<String>,
    clash: Option<ClashConnection>,
}

async fn http_test(
    settings: &Settings,
    target: &TraceTarget,
    proxy: Option<&crate::updates::LocalProxy>,
    resolved: Option<&[IpAddr]>,
) -> RemoteResult {
    let Some(url) = &target.url else {
        return RemoteResult {
            status: TraceStatus::Error,
            detail: "缺少 HTTP URL".into(),
            duration_ms: None,
            warning: None,
            clash: None,
        };
    };
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Xrose-Trace/0.1");
    if let Some(ips) = resolved {
        let addresses: Vec<_> = ips
            .iter()
            .map(|ip| std::net::SocketAddr::new(*ip, target.port))
            .collect();
        builder = builder.resolve_to_addrs(&target.host, &addresses);
    }
    if let Some(proxy) = proxy {
        match proxy.http_proxy() {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(err) => {
                return RemoteResult {
                    status: TraceStatus::Error,
                    detail: format!("HTTP 代理配置失败：{err}"),
                    duration_ms: None,
                    warning: None,
                    clash: None,
                };
            }
        }
    }
    let client = match builder.build() {
        Ok(client) => client,
        Err(err) => {
            return RemoteResult {
                status: TraceStatus::Error,
                detail: format!("HTTP 客户端创建失败：{err}"),
                duration_ms: None,
                warning: None,
                clash: None,
            };
        }
    };
    let excluded = if proxy.is_some() && settings.core == CoreKind::SingBox {
        ClashClient::new(settings.api_port, "")
            .connections()
            .await
            .ok()
            .map(|response| response.connections.into_iter().map(|c| c.id).collect())
            .unwrap_or_default()
    } else {
        std::collections::BTreeSet::new()
    };
    let observe = async {
        if proxy.is_some() && settings.core == CoreKind::SingBox {
            poll_clash(
                settings.api_port,
                &target.host,
                target.port,
                1800,
                None,
                &excluded,
            )
            .await
        } else {
            None
        }
    };
    let request = async {
        let started = Instant::now();
        let result = client.get(url).send().await;
        (result, started.elapsed().as_millis() as u64)
    };
    let ((result, elapsed), clash) = tokio::join!(request, observe);
    match result {
        Ok(response) => {
            let status = response.status();
            let len = response.content_length();
            let text = format!(
                "HTTP {} · {}{} · {} ms",
                status.as_u16(),
                if proxy.is_some() {
                    "经 core 代理"
                } else if resolved.is_some() {
                    "经组网"
                } else {
                    "直连"
                },
                len.map(|len| format!(" · {}", format_bytes(len)))
                    .unwrap_or_default(),
                elapsed
            );
            RemoteResult {
                status: if status.is_success() || status.is_redirection() {
                    TraceStatus::Ok
                } else {
                    TraceStatus::Warn
                },
                detail: text,
                duration_ms: Some(elapsed),
                warning: None,
                clash,
            }
        }
        Err(err) => RemoteResult {
            status: TraceStatus::Error,
            detail: format!("HTTP 请求失败：{err}"),
            duration_ms: Some(elapsed),
            warning: None,
            clash,
        },
    }
}

async fn tcp_direct(target: &TraceTarget, resolved: Option<&[IpAddr]>) -> RemoteResult {
    let started = Instant::now();
    match tokio::time::timeout(Duration::from_secs(8), async {
        if let Some(ips) = resolved {
            let addresses: Vec<_> = ips
                .iter()
                .map(|ip| std::net::SocketAddr::new(*ip, target.port))
                .collect();
            TcpStream::connect(addresses.as_slice()).await
        } else {
            TcpStream::connect((target.host.as_str(), target.port)).await
        }
    })
    .await
    {
        Ok(Ok(stream)) => {
            let elapsed = started.elapsed().as_millis() as u64;
            drop(stream);
            RemoteResult {
                status: TraceStatus::Ok,
                detail: format!(
                    "TCP 连接 {}:{} 成功 · {elapsed} ms",
                    target.host, target.port
                ),
                duration_ms: Some(elapsed),
                warning: None,
                clash: None,
            }
        }
        Ok(Err(err)) => RemoteResult {
            status: TraceStatus::Error,
            detail: format!("TCP 连接失败：{err}"),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            warning: None,
            clash: None,
        },
        Err(_) => RemoteResult {
            status: TraceStatus::Error,
            detail: "TCP 连接超时（8 秒）".into(),
            duration_ms: Some(8000),
            warning: None,
            clash: None,
        },
    }
}

async fn tcp_probe(
    _service: &AppService,
    settings: &Settings,
    socks: &crate::updates::LocalProxy,
    target: &TraceTarget,
    use_clash: bool,
) -> RemoteResult {
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        socks5_connect(socks, &target.host, target.port),
    )
    .await;
    let mut stream = match result {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            return RemoteResult {
                status: TraceStatus::Error,
                detail: format!("经 core SOCKS5 连接失败：{err}"),
                duration_ms: Some(started.elapsed().as_millis() as u64),
                warning: None,
                clash: None,
            };
        }
        Err(_) => {
            return RemoteResult {
                status: TraceStatus::Error,
                detail: "经 core 建立 TCP 连接超时（8 秒）".into(),
                duration_ms: Some(8000),
                warning: None,
                clash: None,
            };
        }
    };
    let elapsed = started.elapsed().as_millis() as u64;
    let mut clash = None;
    if use_clash {
        let api_port = settings.api_port;
        let host = target.host.clone();
        let port = target.port;
        let source_port = stream.local_addr().ok().map(|addr| addr.port());
        let poll = tokio::spawn(async move {
            poll_clash(
                api_port,
                &host,
                port,
                1800,
                source_port,
                &Default::default(),
            )
            .await
        });
        clash = tokio::time::timeout(Duration::from_millis(2000), poll)
            .await
            .ok()
            .and_then(|result| result.ok())
            .flatten();
    }
    // Keep the connection open long enough for the core's connection table to
    // report it, then send nothing: this is a pure TCP reachability probe.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _ = stream.shutdown().await;
    drop(stream);
    RemoteResult {
        status: TraceStatus::Ok,
        detail: format!("经 core 建立 TCP 连接成功 · {elapsed} ms · 已关闭"),
        duration_ms: Some(elapsed),
        warning: None,
        clash,
    }
}

async fn udp_direct(target: &TraceTarget, resolved: Option<&[IpAddr]>) -> RemoteResult {
    let started = Instant::now();
    let port = target.port;
    let payload = udp_payload(port);
    let host = target.host.clone();
    let address = resolved
        .and_then(|ips| ips.first())
        .map(|ip| std::net::SocketAddr::new(*ip, port));
    let sock = match UdpSocket::bind(if address.is_some_and(|address| address.is_ipv6()) {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .await
    {
        Ok(sock) => sock,
        Err(err) => {
            return RemoteResult {
                status: TraceStatus::Error,
                detail: format!("创建 UDP socket 失败：{err}"),
                duration_ms: None,
                warning: None,
                clash: None,
            };
        }
    };
    let sent = if let Some(address) = address {
        sock.send_to(&payload, address).await
    } else {
        sock.send_to(&payload, (host.as_str(), port)).await
    };
    match sent {
        Ok(_) => {
            let mut buf = [0u8; 2048];
            match tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) => RemoteResult {
                    status: TraceStatus::Ok,
                    detail: format!(
                        "UDP 发送成功，收到 {n} 字节响应（来自 {from}）· {} ms",
                        started.elapsed().as_millis()
                    ),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    warning: None,
                    clash: None,
                },
                _ => RemoteResult {
                    status: TraceStatus::Warn,
                    detail: format!(
                        "UDP 数据包已发出，3 秒内未收到响应（服务可能不回复）· {} ms",
                        started.elapsed().as_millis()
                    ),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    warning: Some("UDP 无响应".into()),
                    clash: None,
                },
            }
        }
        Err(err) => RemoteResult {
            status: TraceStatus::Error,
            detail: format!("UDP 发送失败：{err}"),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            warning: None,
            clash: None,
        },
    }
}

async fn udp_probe(
    _service: &AppService,
    _settings: &Settings,
    socks: &crate::updates::LocalProxy,
    target: &TraceTarget,
    _use_clash: bool,
) -> RemoteResult {
    let started = Instant::now();
    let payload = udp_payload(target.port);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        socks5_udp_associate(socks, &target.host, target.port, &payload),
    )
    .await;
    match result {
        Ok(Ok(Some(bytes))) => RemoteResult {
            status: TraceStatus::Ok,
            detail: format!(
                "经 core 发送 UDP，收到 {bytes} 字节响应 · {} ms",
                started.elapsed().as_millis()
            ),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            warning: None,
            clash: None,
        },
        Ok(Ok(None)) => RemoteResult {
            status: TraceStatus::Warn,
            detail: format!(
                "经 core 发送 UDP 成功，未收到响应（服务可能不回复）· {} ms",
                started.elapsed().as_millis()
            ),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            warning: Some("UDP 无响应".into()),
            clash: None,
        },
        Ok(Err(err)) => RemoteResult {
            status: TraceStatus::Error,
            detail: format!("经 core 的 UDP ASSOCIATE 失败：{err}"),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            warning: None,
            clash: None,
        },
        Err(_) => RemoteResult {
            status: TraceStatus::Error,
            detail: "经 core 的 UDP 测试超时".into(),
            duration_ms: Some(3000),
            warning: None,
            clash: None,
        },
    }
}

fn udp_payload(port: u16) -> Vec<u8> {
    if port == 53 {
        // A minimal DNS A query for example.com.
        let mut query = vec![
            0x58, 0x52, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        query.extend_from_slice(&[7, b'e', b'x', b'a', b'm', b'p', b'l', b'e']);
        query.extend_from_slice(&[3, b'c', b'o', b'm']);
        query.extend_from_slice(&[0x00, 0x00, 0x01, 0x00, 0x01]);
        query
    } else {
        b"XROSE UDP TEST\n".to_vec()
    }
}

async fn icmp_test(target: &TraceTarget, running: bool, model: &AppModel) -> RemoteResult {
    let started = Instant::now();
    let ipv6 = target
        .host
        .parse::<IpAddr>()
        .map(|ip| ip.is_ipv6())
        .unwrap_or(false);
    let mut command = trace_command("ping");
    #[cfg(target_os = "windows")]
    {
        command.args(["-n", "1", "-w", "3000"]);
        if ipv6 {
            command.arg("-6");
        }
    }
    #[cfg(target_os = "macos")]
    {
        command.args(["-c", "1", "-W", "3000"]);
        if ipv6 {
            command.arg("-6");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        command.args(["-c", "1", "-W", "3"]);
        if ipv6 {
            command.arg("-6");
        }
    }
    command.arg(&target.host);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(6), command.output()).await;
    let elapsed = started.elapsed().as_millis() as u64;
    let warning = if running {
        Some("ICMP 不经过 SOCKS/HTTP；只有启用 TUN auto_route 时才会被接管".to_string())
    } else if !model
        .inbounds
        .iter()
        .any(|i| i.enabled && i.kind == crate::model::InboundKind::Tun)
    {
        Some("ICMP 天生绕过代理，除非使用 TUN".to_string())
    } else {
        None
    };
    match output {
        Ok(Ok(output)) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let time = regex::Regex::new(r"time[=<]\s*([0-9.]+)\s*ms")
                .ok()
                .and_then(|re| re.captures(&text))
                .and_then(|caps| caps.get(1))
                .and_then(|m| m.as_str().parse::<f64>().ok())
                .map(|ms| ms as u64)
                .unwrap_or(elapsed);
            RemoteResult {
                status: TraceStatus::Ok,
                detail: format!("ICMP Echo 成功 · {time} ms"),
                duration_ms: Some(time),
                warning,
                clash: None,
            }
        }
        Ok(Ok(output)) => {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            RemoteResult {
                status: TraceStatus::Error,
                detail: if err.is_empty() {
                    format!("ICMP Echo 失败（退出码 {:?}）", output.status.code())
                } else {
                    format!("ICMP Echo 失败：{err}")
                },
                duration_ms: Some(elapsed),
                warning,
                clash: None,
            }
        }
        Ok(Err(err)) => RemoteResult {
            status: TraceStatus::Error,
            detail: format!("无法执行 ping：{err}"),
            duration_ms: Some(elapsed),
            warning,
            clash: None,
        },
        Err(_) => RemoteResult {
            status: TraceStatus::Error,
            detail: "ICMP 测试超时".into(),
            duration_ms: Some(elapsed),
            warning,
            clash: None,
        },
    }
}

async fn socks5_login(proxy: &crate::updates::LocalProxy) -> Result<TcpStream, String> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|err| format!("连接 SOCKS 入站失败：{err}"))?;
    let method = if proxy.account.is_some() { 2 } else { 0 };
    stream
        .write_all(&[5, 1, method])
        .await
        .map_err(|e| e.to_string())?;
    let mut reply = [0u8; 2];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|e| e.to_string())?;
    if reply != [5, method] {
        return Err("SOCKS 入站不接受所选认证方式".into());
    }
    if let Some(a) = &proxy.account {
        if !a.valid() {
            return Err("SOCKS 账号无效，请检查局域网认证设置".into());
        }
        let mut auth = vec![1, a.username.len() as u8];
        auth.extend_from_slice(a.username.as_bytes());
        auth.push(a.password.len() as u8);
        auth.extend_from_slice(a.password.as_bytes());
        stream.write_all(&auth).await.map_err(|e| e.to_string())?;
        stream
            .read_exact(&mut reply)
            .await
            .map_err(|e| e.to_string())?;
        if reply != [1, 0] {
            return Err("SOCKS 入站认证失败，请检查账号密码".into());
        }
    }
    Ok(stream)
}

async fn socks5_connect(
    proxy: &crate::updates::LocalProxy,
    host: &str,
    port: u16,
) -> Result<TcpStream, String> {
    let mut stream = socks5_login(proxy).await?;
    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(ip) => {
                request.push(0x01);
                request.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                request.push(0x04);
                request.extend_from_slice(&ip.octets());
            }
        }
    } else {
        let bytes = host.as_bytes();
        if bytes.len() > 255 {
            return Err("域名过长".into());
        }
        request.push(0x03);
        request.push(bytes.len() as u8);
        request.extend_from_slice(bytes);
    }
    request.push((port >> 8) as u8);
    request.push(port as u8);
    stream
        .write_all(&request)
        .await
        .map_err(|e| e.to_string())?;
    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|e| e.to_string())?;
    if head[0] != 0x05 || head[1] != 0x00 {
        return Err(format!("SOCKS5 CONNECT 失败（代码 {}）", head[1]));
    }
    let skip = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .map_err(|e| e.to_string())?;
            len[0] as usize + 2
        }
        _ => return Err("SOCKS5 返回未知地址类型".into()),
    };
    let mut tail = vec![0u8; skip];
    stream
        .read_exact(&mut tail)
        .await
        .map_err(|e| e.to_string())?;
    Ok(stream)
}

async fn socks5_udp_associate(
    proxy: &crate::updates::LocalProxy,
    host: &str,
    port: u16,
    payload: &[u8],
) -> Result<Option<usize>, String> {
    let mut control = socks5_login(proxy).await?;
    // UDP ASSOCIATE with 0.0.0.0:0.
    control
        .write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .map_err(|e| e.to_string())?;
    let mut head = [0u8; 4];
    control
        .read_exact(&mut head)
        .await
        .map_err(|e| e.to_string())?;
    if head[0] != 0x05 || head[1] != 0x00 {
        return Err(format!("SOCKS5 UDP ASSOCIATE 失败（代码 {}）", head[1]));
    }
    let relay = match head[3] {
        0x01 => {
            let mut buf = [0u8; 6];
            control
                .read_exact(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            std::net::Ipv4Addr::new(buf[0], buf[1], buf[2], buf[3])
        }
        0x04 => {
            let mut buf = [0u8; 18];
            control
                .read_exact(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[..16]);
            let port = u16::from_be_bytes([buf[16], buf[17]]);
            let addr =
                std::net::SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::from(octets)), port);
            let socket = UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|e| e.to_string())?;
            let mut packet = vec![0, 0, 0];
            append_socks_target(&mut packet, host, port);
            packet.extend_from_slice(payload);
            socket
                .send_to(&packet, addr)
                .await
                .map_err(|e| e.to_string())?;
            return recv_socks_udp(&socket).await;
        }
        _ => return Err("SOCKS5 返回未知中继地址".into()),
    };
    let mut port_buf = [0u8; 2];
    control
        .read_exact(&mut port_buf)
        .await
        .map_err(|e| e.to_string())?;
    let relay_port = u16::from_be_bytes(port_buf);
    let mut relay_ip = relay;
    if relay_ip.is_unspecified() {
        relay_ip = std::net::Ipv4Addr::LOCALHOST;
    }
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let mut packet = vec![0, 0, 0];
    append_socks_target(&mut packet, host, port);
    packet.extend_from_slice(payload);
    socket
        .send_to(&packet, (relay_ip, relay_port))
        .await
        .map_err(|e| e.to_string())?;
    recv_socks_udp(&socket).await
}

fn append_socks_target(packet: &mut Vec<u8>, host: &str, port: u16) {
    if let Ok(ip) = host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(ip) => {
                packet.push(0x01);
                packet.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                packet.push(0x04);
                packet.extend_from_slice(&ip.octets());
            }
        }
    } else {
        packet.push(0x03);
        packet.push(host.len().min(255) as u8);
        packet.extend_from_slice(host.as_bytes());
    }
    packet.extend_from_slice(&port.to_be_bytes());
}

async fn recv_socks_udp(socket: &UdpSocket) -> Result<Option<usize>, String> {
    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_secs(3), socket.recv(&mut buf)).await {
        Ok(Ok(n)) => {
            if n < 10 {
                return Ok(None);
            }
            let offset = match buf[3] {
                0x01 => 4 + 4 + 2,
                0x04 => 4 + 16 + 2,
                0x03 => {
                    let len = buf[4] as usize;
                    4 + 1 + len + 2
                }
                _ => return Ok(None),
            };
            Ok(Some(n.saturating_sub(offset)))
        }
        _ => Ok(None),
    }
}

async fn poll_clash(
    api_port: u16,
    host: &str,
    port: u16,
    wait_ms: u64,
    source_port: Option<u16>,
    excluded: &std::collections::BTreeSet<String>,
) -> Option<ClashConnection> {
    let client = ClashClient::new(api_port, "");
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let host = host.to_ascii_lowercase();
    loop {
        if let Ok(response) = client.connections().await {
            if let Some(found) = response
                .connections
                .into_iter()
                .find(|conn| matches_probe_connection(conn, &host, port, source_port, excluded))
            {
                return Some(found);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

fn matches_probe_connection(
    connection: &ClashConnection,
    host: &str,
    port: u16,
    source_port: Option<u16>,
    excluded: &std::collections::BTreeSet<String>,
) -> bool {
    connection.metadata.destination_port == port.to_string()
        && (connection.host().eq_ignore_ascii_case(host)
            || connection
                .metadata
                .destination_ip
                .eq_ignore_ascii_case(host))
        && !excluded.contains(&connection.id)
        && source_port.is_none_or(|port| connection.metadata.source_port == port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_preview_ignores_saved_proxy_and_firewall_rules() {
        let mut model = AppModel::default();
        model.work_mode = WorkMode::Mesh;
        model.routing.rules.insert(
            0,
            crate::model::RouteRule::new(
                "saved block",
                Default::default(),
                crate::model::RouteAction::block(),
            ),
        );
        model.mesh_mode.target = Some(crate::model::MeshExitTarget {
            profile_id: "fixture".into(),
            peer_id: "exit".into(),
            provider: crate::mesh::MeshProvider::EasyTier,
            name: "Fixture exit".into(),
            address: "192.0.2.10".into(),
            gateway_id: "mesh-exit-fixture".into(),
        });
        let route = crate::mesh::coexist::MeshRoute {
            cidr: "192.0.2.0/24".into(),
            interface: "fixture-tun".into(),
        };
        let preview = explain_route(&model, &ConnectionInfo::default(), Some(&route));
        assert!(preview.rule.is_none());
        assert_eq!(preview.action.kind, RouteActionKind::Proxy);
        assert!(preview.explanation.contains("Fixture exit"));
        assert!(preview.explanation.contains("本地代理分流已暂停"));
        assert!(preview.explanation.contains("192.0.2.0/24"));
    }

    #[tokio::test]
    async fn system_probes_use_supplied_mesh_dns_answers_without_proxy_listeners() {
        let ips = [IpAddr::from([127, 0, 0, 1])];
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = parse_target(&format!("tcp://mesh-probe.invalid:{port}")).unwrap();
        assert_eq!(
            tcp_direct(&target, Some(&ips)).await.status,
            TraceStatus::Ok
        );
        let (connection, _) = listener.accept().await.unwrap();
        drop(connection);
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = vec![0; 4096];
            let n = stream.read(&mut data).await.unwrap();
            let headers = String::from_utf8_lossy(&data[..n]);
            assert!(
                headers
                    .to_lowercase()
                    .contains(&format!("host: mesh-probe.invalid:{port}"))
            );
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let target = parse_target(&format!("http://mesh-probe.invalid:{port}/")).unwrap();
        let http = http_test(&Settings::default(), &target, None, Some(&ips)).await;
        assert_eq!(http.status, TraceStatus::Ok);
        assert!(http.detail.contains("经组网"));
        assert!(!http.detail.contains("直连"));
        server.await.unwrap();
        let socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let echo = tokio::spawn(async move {
            let mut data = [0; 2048];
            let (n, from) = socket.recv_from(&mut data).await.unwrap();
            socket.send_to(&data[..n], from).await.unwrap();
        });
        let target = parse_target(&format!("udp://mesh-probe.invalid:{port}")).unwrap();
        assert_eq!(
            udp_direct(&target, Some(&ips)).await.status,
            TraceStatus::Ok
        );
        echo.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn trace_child_has_no_visible_console() {
        const CHILD: &str = "XROSE_TEST_INSPECT_TRACE_CONSOLE";
        if std::env::var_os(CHILD).is_some() {
            use windows_sys::Win32::{
                System::Console::GetConsoleWindow, UI::WindowsAndMessaging::IsWindowVisible,
            };
            // SAFETY: both calls inspect the current process's console window.
            let visible = unsafe { IsWindowVisible(GetConsoleWindow()) };
            assert_eq!(visible, 0);
            return;
        }
        let output = trace_command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "trace::tests::trace_child_has_no_visible_console",
            ])
            .env(CHILD, "1")
            .output()
            .await
            .unwrap();
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && report.contains("1 passed"),
            "{report}"
        );
    }

    #[test]
    fn direct_trace_identifies_firewall_group_and_prioritized_mesh_route() {
        use crate::model::{Matchers, RouteAction, RouteRule, RouteRuleGroup};
        let mut model = AppModel::default();
        model.routing.rules.clear();
        let info = ConnectionInfo {
            domain: Some("blocked.example.org".into()),
            destination_ip: Some("192.0.2.15".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(
            explain_route(&model, &info, None).action,
            RouteAction::direct()
        );
        let rule = RouteRule::new(
            "Block example",
            Matchers {
                domains: vec!["blocked.example.org".into()],
                ..Default::default()
            },
            RouteAction::block(),
        );
        model.routing.rule_groups.push(RouteRuleGroup {
            name: "Example group".into(),
            rules: vec![rule.clone()],
            ..Default::default()
        });
        let blocked = explain_route(&model, &info, None);
        assert_eq!(blocked.action, RouteAction::block());
        assert!(
            blocked.explanation.contains(&rule.name)
                && blocked.explanation.contains("Example group")
        );
        let mesh = crate::mesh::coexist::MeshRoute {
            cidr: "192.0.2.0/24".into(),
            interface: "mesh-test".into(),
        };
        let preview = explain_route(&model, &info, Some(&mesh));
        assert_eq!(preview.action, RouteAction::direct());
        assert!(preview.explanation.contains("mesh-test"));
    }

    #[test]
    fn probe_metadata_requires_matching_host_port_and_source() {
        let mut c = ClashConnection::default();
        c.id = "new".into();
        c.metadata.host = "Example.com".into();
        c.metadata.destination_port = "443".into();
        c.metadata.source_port = "12345".into();
        let excluded = Default::default();
        assert!(matches_probe_connection(
            &c,
            "example.com",
            443,
            Some(12345),
            &excluded
        ));
        assert!(!matches_probe_connection(
            &c,
            "example.com",
            80,
            Some(12345),
            &excluded
        ));
        assert!(!matches_probe_connection(
            &c,
            "unrelated.example",
            443,
            Some(12345),
            &excluded
        ));
        assert!(!matches_probe_connection(
            &c,
            "example.com",
            443,
            Some(54321),
            &excluded
        ));
        assert!(!matches_probe_connection(
            &c,
            "example.com",
            443,
            None,
            &["new".to_string()].into_iter().collect()
        ));
        c.metadata.host.clear();
        assert!(!matches_probe_connection(
            &c,
            "example.com",
            443,
            None,
            &excluded
        ));
    }

    #[test]
    fn early_failure_identifies_active_stage_and_skips_later_stages() {
        let target = parse_target("https://example.com").unwrap();
        let mut trace = TraceSession::new(1, &target);
        trace.activate(TraceStepKind::Dns, "解析中");
        trace.finish_failure("DNS 解析超时".into());
        assert_eq!(
            trace.step(TraceStepKind::Dns).unwrap().status,
            TraceStatus::Error
        );
        assert_eq!(
            trace.step(TraceStepKind::Remote).unwrap().status,
            TraceStatus::Skipped
        );
        assert_eq!(trace.phase, TracePhase::Failed);
    }

    #[test]
    fn parses_supported_targets() {
        let http = parse_target("https://example.com/a?b=1").unwrap();
        assert_eq!(http.protocol, TraceProtocol::Https);
        assert_eq!(http.port, 443);
        assert_eq!(http.path, "/a?b=1");
        let tcp = parse_target("tcp://1.2.3.4:8443").unwrap();
        assert_eq!(tcp.protocol, TraceProtocol::Tcp);
        assert_eq!(tcp.port, 8443);
        let udp = parse_target("udp://8.8.8.8:53").unwrap();
        assert_eq!(udp.protocol, TraceProtocol::Udp);
        let icmp = parse_target("icmp://1.1.1.1").unwrap();
        assert_eq!(icmp.port, 0);
        assert!(parse_target("ftp://example.com").is_err());
        assert!(parse_target("").is_err());
    }

    #[test]
    fn dns_rule_matching_is_semantic() {
        assert!(domain_rule_matches("geosite:cn", "www.taobao.cn"));
        assert!(!domain_rule_matches(
            "geosite:geolocation-!cn",
            "www.taobao.cn"
        ));
        assert!(domain_rule_matches("suffix:example.com", "a.example.com"));
        assert!(domain_rule_matches("fulldomain", "fulldomain"));
        assert!(!domain_rule_matches("suffix:example.com", "notexample.com"));
    }

    #[test]
    fn builds_dns_query_for_udp_53() {
        let payload = udp_payload(53);
        assert!(payload.len() > 20);
        assert_eq!(payload[4], 0);
        assert_eq!(payload[5], 1);
        assert_ne!(udp_payload(443), payload);
    }
}
