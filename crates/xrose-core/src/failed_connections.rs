//! Core-independent failed, stalled, and rule-rejected connection tracking.
//!
//! Combines real-time core log stream correlation (`sing-box` & `Xray`) with
//! Clash API `/connections` stalled-TCP detection and metadata enrichment.
//! All records are held strictly in a bounded in-memory ring buffer and never
//! persisted to disk.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::diagnostics::{
    DiagnosticRuleKind, DiagnosticTarget, format_url_host, is_fake_ip, is_tls_port,
};
use crate::error::{Error, Result};
use crate::flow::is_protected_process;
use crate::model::{AppModel, CoreKind, Matchers, RouteAction, WorkMode};
use crate::runtime::ClashConnection;
use crate::util::sanitize_line;

pub const FAILED_RECORD_LIMIT: usize = 300;
pub const FAILED_RECORD_TTL_SECS: i64 = 15 * 60;
pub const STALLED_THRESHOLD_SECS: i64 = 4;

const CONTEXT_TTL_SECS: i64 = 60;
const MAX_CONTEXTS: usize = 2000;
const ENRICH_TTL_SECS: i64 = 30;
const MAX_ENRICH_ENTRIES: usize = 1000;
const DNS_OK_SUPPRESS_SECS: i64 = 5;
const OFFLINE_BURST_WINDOW_SECS: i64 = 15;
const RAW_DETAIL_LIMIT: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Timeout,
    Reset,
    Unreachable,
    DnsFailed,
    TlsError,
    Stalled,
    Rejected,
    Other,
}

impl FailureKind {
    pub const fn category(self) -> FailureCategory {
        match self {
            Self::Stalled => FailureCategory::Stalled,
            Self::Rejected => FailureCategory::Rejected,
            Self::Timeout
            | Self::Reset
            | Self::Unreachable
            | Self::DnsFailed
            | Self::TlsError
            | Self::Other => FailureCategory::Failed,
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Timeout => "连接超时",
            Self::Reset => "连接重置",
            Self::Unreachable => "无法连接",
            Self::DnsFailed => "DNS 失败",
            Self::TlsError => "TLS 失败",
            Self::Stalled => "无响应",
            Self::Rejected => "规则拦截",
            Self::Other => "连接失败",
        }
    }

    pub const fn label_en(self) -> &'static str {
        match self {
            Self::Timeout => "Timeout",
            Self::Reset => "Reset",
            Self::Unreachable => "Unreachable",
            Self::DnsFailed => "DNS failed",
            Self::TlsError => "TLS error",
            Self::Stalled => "Stalled",
            Self::Rejected => "Blocked",
            Self::Other => "Failed",
        }
    }

    pub const fn short_label_zh(self) -> &'static str {
        match self {
            Self::Timeout => "超时",
            Self::Reset => "重置",
            Self::Unreachable => "无法连接",
            Self::DnsFailed => "DNS",
            Self::TlsError => "TLS",
            Self::Stalled => "无响应",
            Self::Rejected => "已拦截",
            Self::Other => "失败",
        }
    }

    pub const fn short_label_en(self) -> &'static str {
        match self {
            Self::Timeout => "Timeout",
            Self::Reset => "Reset",
            Self::Unreachable => "Unreach",
            Self::DnsFailed => "DNS",
            Self::TlsError => "TLS",
            Self::Stalled => "Stalled",
            Self::Rejected => "Blocked",
            Self::Other => "Failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    #[default]
    Failed,
    Stalled,
    Rejected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundDirection {
    Direct,
    Proxy,
    Reject,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainMatchGranularity {
    #[default]
    RootDomainSuffix,
    Exact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedRuleSummary {
    pub expression: String,
    pub action: RouteAction,
    pub outbound_label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailedConnectionRecord {
    pub id: u64,
    pub domain: String,
    pub domain_is_sniffed: bool,
    pub root_domain: Option<String>,
    pub destination_ip: String,
    pub destination_port: String,
    pub network: String,
    pub inbound: String,
    pub process_name: String,
    pub process_path: String,
    pub outbound_tag: String,
    pub outbound_label: String,
    pub outbound_direction: OutboundDirection,
    pub matched_rule: Option<String>,
    pub failure_kind: FailureKind,
    pub reason_summary: String,
    pub raw_detail: String,
    pub is_stalled_live: bool,
    pub stalled_secs: u64,
    pub count: u32,
    pub first_seen: i64,
    pub last_seen: i64,
    pub applied_rule: Option<AppliedRuleSummary>,
}

impl Default for FailureKind {
    fn default() -> Self {
        Self::Other
    }
}

impl FailedConnectionRecord {
    pub fn host(&self) -> &str {
        if !self.domain.is_empty() {
            &self.domain
        } else {
            &self.destination_ip
        }
    }

    pub fn is_unfixed(&self) -> bool {
        self.applied_rule.is_none()
    }

    pub fn suggested_action(&self, work_mode: WorkMode) -> RouteAction {
        match self.outbound_direction {
            OutboundDirection::Direct | OutboundDirection::Unknown => {
                if work_mode == WorkMode::Direct {
                    RouteAction::proxy()
                } else {
                    RouteAction::proxy()
                }
            }
            OutboundDirection::Proxy => RouteAction::direct(),
            OutboundDirection::Reject => RouteAction::direct(),
        }
    }

    pub fn rule_expression(&self, granularity: DomainMatchGranularity) -> Option<String> {
        self.build_matchers(granularity).ok().map(|(expr, _)| expr)
    }

    pub fn build_matchers(
        &self,
        granularity: DomainMatchGranularity,
    ) -> Result<(String, Matchers)> {
        let mut matchers = Matchers::default();
        if !self.domain.is_empty() {
            let domain = normalize_domain(&self.domain);
            if domain.is_empty() || domain.parse::<IpAddr>().is_ok() {
                return Err(Error::config("该记录缺少有效的目标域名或 IP"));
            }
            match granularity {
                DomainMatchGranularity::RootDomainSuffix => {
                    if let Some(root) = self
                        .root_domain
                        .as_deref()
                        .map(normalize_domain)
                        .filter(|r| !r.is_empty())
                        .or_else(|| extract_root_domain(&domain))
                    {
                        let expr = format!("*.{root}");
                        matchers.domain_suffixes.push(root);
                        return Ok((expr, matchers));
                    }
                    matchers.domains.push(domain.clone());
                    return Ok((domain, matchers));
                }
                DomainMatchGranularity::Exact => {
                    matchers.domains.push(domain.clone());
                    return Ok((domain, matchers));
                }
            }
        }
        if !self.destination_ip.is_empty() {
            let ip: IpAddr = self
                .destination_ip
                .trim()
                .trim_matches(['[', ']'])
                .parse()
                .map_err(|_| Error::config("缺少有效的目标 IP"))?;
            let cidr = format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 });
            matchers.ip_cidrs.push(cidr.clone());
            return Ok((cidr, matchers));
        }
        Err(Error::config("该失败记录缺少可生成规则的域名或目标 IP"))
    }

    pub fn diagnostic_target(&self) -> DiagnosticTarget {
        DiagnosticTarget {
            domain: self.domain.clone(),
            ip: self.destination_ip.clone(),
            process: self.process_name.clone(),
        }
    }

    pub fn domain_test_target(&self) -> Option<String> {
        if self.domain.is_empty() || self.domain.parse::<IpAddr>().is_ok() {
            return None;
        }
        let port = self
            .destination_port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)?;
        if self.network.eq_ignore_ascii_case("udp") {
            return Some(format!("udp://{}:{port}", self.domain));
        }
        if self.network.eq_ignore_ascii_case("dns") {
            return Some(format!("https://{}", self.domain));
        }
        Some(match port {
            443 => format!("https://{}", self.domain),
            80 => format!("http://{}", self.domain),
            _ if is_tls_port(port)
                || self.domain_is_sniffed
                || self.failure_kind == FailureKind::TlsError =>
            {
                format!("tls://{}:{port}", self.domain)
            }
            _ => format!("tcp://{}:{port}", self.domain),
        })
    }

    pub fn ip_test_target(&self) -> Option<String> {
        let ip: IpAddr = self
            .destination_ip
            .trim()
            .trim_matches(['[', ']'])
            .parse()
            .ok()?;
        if is_fake_ip(ip) {
            return None;
        }
        let port = self
            .destination_port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)?;
        let formatted_ip = format_url_host(&ip.to_string());
        if self.network.eq_ignore_ascii_case("udp") {
            return Some(format!("udp://{formatted_ip}:{port}"));
        }
        let sni = (!self.domain.is_empty() && self.domain.parse::<IpAddr>().is_err())
            .then_some(self.domain.as_str());
        Some(match (port, sni) {
            (80, _) => format!("tcp://{formatted_ip}:80"),
            (_, Some(sni)) => format!("tls://{formatted_ip}:{port}?sni={sni}"),
            (p, None) if is_tls_port(p) || self.failure_kind == FailureKind::TlsError => {
                format!("tls://{formatted_ip}:{p}")
            }
            (_, None) => format!("tcp://{formatted_ip}:{port}"),
        })
    }

    pub fn test_target(&self) -> Option<String> {
        let host = self.host();
        if host.is_empty() {
            return None;
        }
        let port = self
            .destination_port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)?;
        if self.network.eq_ignore_ascii_case("udp") {
            let formatted_host = format_url_host(host);
            return Some(format!("udp://{formatted_host}:{port}"));
        }
        if !self.domain.is_empty() && self.domain.parse::<IpAddr>().is_err() {
            // For non-standard ports where SNI was sniffed from a raw-IP connection
            // (e.g. TUN IPv6:2084 with SNI), replay the exact IP + SNI over TLS.
            let prefer_ip_sni = port != 443
                && port != 80
                && (self.domain_is_sniffed || self.inbound.to_ascii_lowercase().contains("tun"));
            if prefer_ip_sni && let Some(ip_target) = self.ip_test_target() {
                return Some(ip_target);
            }
            return self.domain_test_target();
        }
        if let Some(ip_target) = self.ip_test_target() {
            return Some(ip_target);
        }
        let formatted_host = format_url_host(host);
        if is_tls_port(port) || self.failure_kind == FailureKind::TlsError {
            Some(format!("tls://{formatted_host}:{port}"))
        } else {
            Some(format!("tcp://{formatted_host}:{port}"))
        }
    }

    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return true;
        }
        let fields = [
            self.domain.to_ascii_lowercase(),
            self.root_domain.clone().unwrap_or_default(),
            self.destination_ip.to_ascii_lowercase(),
            self.destination_port.to_ascii_lowercase(),
            self.network.to_ascii_lowercase(),
            self.inbound.to_ascii_lowercase(),
            self.process_name.to_ascii_lowercase(),
            self.process_path.to_ascii_lowercase(),
            self.outbound_tag.to_ascii_lowercase(),
            self.outbound_label.to_ascii_lowercase(),
            self.matched_rule
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
            self.failure_kind.label_zh().to_ascii_lowercase(),
            self.failure_kind.label_en().to_ascii_lowercase(),
            self.reason_summary.to_ascii_lowercase(),
            self.raw_detail.to_ascii_lowercase(),
        ];
        query
            .split_whitespace()
            .all(|token| fields.iter().any(|field| field.contains(token)))
    }

    pub fn covered_by_matchers(&self, matchers: &Matchers) -> bool {
        if !self.domain.is_empty() {
            let domain = normalize_domain(&self.domain);
            if matchers
                .domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&domain))
            {
                return true;
            }
            if matchers.domain_suffixes.iter().any(|suffix| {
                let s = normalize_domain(suffix);
                !s.is_empty() && (domain == s || domain.ends_with(&format!(".{s}")))
            }) {
                return true;
            }
        }
        if !self.destination_ip.is_empty()
            && let Ok(ip) = self.destination_ip.trim().parse::<IpAddr>()
        {
            if matchers.ip_cidrs.iter().any(|cidr| {
                cidr.parse::<ipnet::IpNet>()
                    .is_ok_and(|net| net.contains(&ip))
            }) {
                return true;
            }
        }
        if !self.process_name.is_empty()
            && matchers
                .process_names
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&self.process_name))
        {
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailedConnectionsView {
    pub records: Vec<FailedConnectionRecord>,
    pub unfixed_count: usize,
    pub unfixed_only_stalled: bool,
    pub host_offline: bool,
}

impl FailedConnectionsView {
    pub fn recent_unfixed(&self, limit: usize) -> impl Iterator<Item = &FailedConnectionRecord> {
        self.records.iter().filter(|r| r.is_unfixed()).take(limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedRuleDraft {
    pub record_id: u64,
    pub granularity: DomainMatchGranularity,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchRuleApplyOutcome {
    pub rules_saved: usize,
    pub records_marked: usize,
    pub reloaded: bool,
}

/// Extracts the registerable root domain via the offline Public Suffix List.
pub fn extract_root_domain(host: &str) -> Option<String> {
    let lower = normalize_domain(host);
    if lower.is_empty() || lower.parse::<IpAddr>().is_ok() || !lower.contains('.') {
        return None;
    }
    if lower == "localhost"
        || lower.ends_with(".local")
        || lower.ends_with(".localhost")
        || lower.ends_with(".internal")
        || lower.ends_with(".lan")
        || lower.ends_with(".home.arpa")
    {
        return None;
    }
    if psl::suffix(lower.as_bytes()).is_some_and(|suffix| suffix.is_known()) {
        return psl::domain_str(&lower).map(str::to_owned);
    }
    None
}

pub fn normalize_domain(host: &str) -> String {
    host.trim()
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Counts how many unique rules a batch selection will produce after PSL deduplication.
pub fn count_unique_batch_rules<'a>(
    records: impl IntoIterator<Item = &'a FailedConnectionRecord>,
    granularity: DomainMatchGranularity,
) -> usize {
    let mut keys = BTreeSet::new();
    for record in records {
        if let Some(expr) = record.rule_expression(granularity) {
            keys.insert(expr);
        }
    }
    keys.len()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AggKey {
    target: String,
    outbound: String,
    is_rejected: bool,
}

struct InternalRecord {
    agg_key: AggKey,
    record: FailedConnectionRecord,
    active_stalled_flows: HashSet<(String, String)>,
    only_from_stalled: bool,
}

#[derive(Debug, Clone, Default)]
struct SingBoxConnContext {
    updated_at: i64,
    network: String,
    inbound: String,
    source_addr: String,
    target_host: String,
    sniff_domain: String,
    destination_ip: String,
    destination_port: String,
    process_name: String,
    process_path: String,
    outbound_tag: String,
    outbound_type: String,
    matched_rule: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ClashMetaSnapshot {
    updated_at: i64,
    process_name: String,
    process_path: String,
    matched_rule: Option<String>,
    outbound_tag: String,
    inbound: String,
    destination_ip: String,
    domain: String,
}

struct TrackedTcpFlow {
    first_zero_rx_ts: i64,
    last_seen_ts: i64,
    has_rx: bool,
    last_upload: u64,
    stalled_record_id: Option<u64>,
    connection: ClashConnection,
}

struct FailureEvent {
    domain: String,
    domain_is_sniffed: bool,
    destination_ip: String,
    destination_port: String,
    network: String,
    inbound: String,
    source_addr: String,
    process_name: String,
    process_path: String,
    outbound_tag: String,
    outbound_type: String,
    matched_rule: Option<String>,
    failure_kind: FailureKind,
    reason_summary: String,
    raw_detail: String,
}

pub struct FailedConnectionTracker {
    next_id: u64,
    records: Vec<InternalRecord>,
    singbox_contexts: HashMap<String, SingBoxConnContext>,
    enrich_by_dest: HashMap<(String, String), ClashMetaSnapshot>,
    enrich_by_source: HashMap<String, ClashMetaSnapshot>,
    active_tcp_flows: HashMap<(String, String), TrackedTcpFlow>,
    recent_ok_domains: HashMap<String, i64>,
    failure_burst: VecDeque<(i64, OutboundDirection, String)>,
    last_success_ts: i64,
    external_host_offline: bool,
    local_host_offline: bool,
}

impl Default for FailedConnectionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FailedConnectionTracker {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            records: Vec::new(),
            singbox_contexts: HashMap::new(),
            enrich_by_dest: HashMap::new(),
            enrich_by_source: HashMap::new(),
            active_tcp_flows: HashMap::new(),
            recent_ok_domains: HashMap::new(),
            failure_burst: VecDeque::new(),
            last_success_ts: 0,
            external_host_offline: false,
            local_host_offline: false,
        }
    }

    pub fn set_external_host_offline(&mut self, offline: bool) {
        self.external_host_offline = offline;
    }

    /// Keeps recent records across hot reloads while clearing live-stalled state
    /// and per-process correlation caches from the previous core epoch.
    pub fn on_core_epoch_changed(&mut self) {
        self.singbox_contexts.clear();
        self.enrich_by_dest.clear();
        self.enrich_by_source.clear();
        self.active_tcp_flows.clear();
        for entry in &mut self.records {
            entry.active_stalled_flows.clear();
            entry.record.is_stalled_live = false;
        }
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.failure_burst.clear();
        self.local_host_offline = false;
        for flow in self.active_tcp_flows.values_mut() {
            flow.stalled_record_id = None;
        }
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        let before = self.records.len();
        self.records.retain(|entry| entry.record.id != id);
        if self.records.len() != before {
            for flow in self.active_tcp_flows.values_mut() {
                if flow.stalled_record_id == Some(id) {
                    flow.stalled_record_id = None;
                }
            }
            true
        } else {
            false
        }
    }

    pub fn record(&self, id: u64) -> Option<FailedConnectionRecord> {
        self.records
            .iter()
            .find(|entry| entry.record.id == id)
            .map(|entry| entry.record.clone())
    }

    pub fn mark_rules_applied(
        &mut self,
        explicit_ids: &[u64],
        applied_matchers: &[(Matchers, AppliedRuleSummary)],
    ) -> usize {
        let explicit: HashSet<u64> = explicit_ids.iter().copied().collect();
        let mut marked = 0usize;
        for entry in &mut self.records {
            if let Some((_, summary)) = applied_matchers
                .iter()
                .find(|(m, _)| entry.record.covered_by_matchers(m))
            {
                if entry.record.applied_rule.as_ref() != Some(summary) {
                    entry.record.applied_rule = Some(summary.clone());
                    marked += 1;
                }
            } else if explicit.contains(&entry.record.id)
                && let Some((_, summary)) = applied_matchers.first()
            {
                entry.record.applied_rule = Some(summary.clone());
                marked += 1;
            }
        }
        marked
    }

    pub fn snapshot(&mut self, now: i64) -> FailedConnectionsView {
        self.prune(now);
        let mut records: Vec<FailedConnectionRecord> =
            self.records.iter().map(|e| e.record.clone()).collect();
        records.sort_by(|a, b| b.last_seen.cmp(&a.last_seen).then_with(|| b.id.cmp(&a.id)));
        let unfixed_count = records.iter().filter(|r| r.is_unfixed()).count();
        let unfixed_only_stalled = unfixed_count > 0
            && records
                .iter()
                .filter(|r| r.is_unfixed())
                .all(|r| r.failure_kind == FailureKind::Stalled);
        FailedConnectionsView {
            records,
            unfixed_count,
            unfixed_only_stalled,
            host_offline: self.is_offline(now),
        }
    }

    pub fn observe_log_line(&mut self, core: CoreKind, raw_line: &str, model: &AppModel, now: i64) {
        let line = sanitize_line(raw_line);
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        match core {
            CoreKind::SingBox => self.observe_singbox_line(line, model, now),
            CoreKind::Xray => self.observe_xray_line(line, model, now),
        }
    }

    pub fn observe_clash_connections(
        &mut self,
        connections: &[ClashConnection],
        model: &AppModel,
        now: i64,
    ) {
        let mut current_keys = HashSet::with_capacity(connections.len());
        for conn in connections {
            let host = conn.host();
            let port = conn.metadata.destination_port.clone();
            let process_name = conn.process_name();
            let process_path = conn.process_path().to_owned();
            if is_protected_process(&process_name) || is_protected_process(&process_path) {
                continue;
            }
            if is_local_or_loopback(&host)
                && (conn.metadata.destination_ip.is_empty()
                    || is_local_or_loopback(&conn.metadata.destination_ip))
            {
                continue;
            }
            let outbound_tag = conn.chains.first().cloned().unwrap_or_default();
            let matched_rule = format_clash_rule(&conn.rule, &conn.rule_payload);
            let meta = ClashMetaSnapshot {
                updated_at: now,
                process_name: process_name.clone(),
                process_path: process_path.clone(),
                matched_rule: matched_rule.clone(),
                outbound_tag: outbound_tag.clone(),
                inbound: conn.metadata.kind.clone(),
                destination_ip: conn.metadata.destination_ip.clone(),
                domain: if host.parse::<IpAddr>().is_err() {
                    normalize_domain(&host)
                } else {
                    String::new()
                },
            };
            if !host.is_empty() && !port.is_empty() {
                self.enrich_by_dest
                    .insert((normalize_domain(&host), port.clone()), meta.clone());
            }
            if !conn.metadata.destination_ip.is_empty() && !port.is_empty() {
                self.enrich_by_dest.insert(
                    (
                        normalize_domain(&conn.metadata.destination_ip),
                        port.clone(),
                    ),
                    meta.clone(),
                );
            }
            if !conn.metadata.source_ip.is_empty() && !conn.metadata.source_port.is_empty() {
                let src = format_host_port(&conn.metadata.source_ip, &conn.metadata.source_port);
                self.enrich_by_source.insert(src, meta);
            }

            if !conn.metadata.network.eq_ignore_ascii_case("tcp") {
                if conn.download > 0 {
                    self.note_success(&host, now);
                }
                continue;
            }

            let flow_key = (conn.id.clone(), conn.start.clone());
            current_keys.insert(flow_key.clone());

            let flow = self
                .active_tcp_flows
                .entry(flow_key.clone())
                .or_insert_with(|| TrackedTcpFlow {
                    first_zero_rx_ts: now,
                    last_seen_ts: now,
                    has_rx: conn.download > 0,
                    last_upload: conn.upload,
                    stalled_record_id: None,
                    connection: conn.clone(),
                });
            flow.last_seen_ts = now;
            flow.last_upload = flow.last_upload.max(conn.upload);
            flow.connection = conn.clone();

            if conn.download > 0 {
                let prev_stalled = flow.stalled_record_id.take();
                flow.has_rx = true;
                self.note_success(&host, now);
                if let Some(rec_id) = prev_stalled {
                    self.resolve_recovered_stalled(rec_id, &flow_key);
                }
                continue;
            }

            if !flow.has_rx && flow.last_upload > 0 {
                let elapsed = now.saturating_sub(flow.first_zero_rx_ts).max(0) as u64;
                if elapsed >= STALLED_THRESHOLD_SECS as u64 {
                    let already_tracked = flow.stalled_record_id;
                    let conn_clone = flow.connection.clone();
                    let rec_id = self.upsert_stalled_flow(
                        &flow_key,
                        &conn_clone,
                        elapsed,
                        already_tracked,
                        model,
                        now,
                    );
                    if let Some(flow_mut) = self.active_tcp_flows.get_mut(&flow_key) {
                        flow_mut.stalled_record_id = rec_id;
                    }
                }
            }
        }

        let finished_keys: Vec<(String, String)> = self
            .active_tcp_flows
            .keys()
            .filter(|k| !current_keys.contains(*k))
            .cloned()
            .collect();
        for key in finished_keys {
            if let Some(flow) = self.active_tcp_flows.remove(&key)
                && let Some(rec_id) = flow.stalled_record_id
            {
                let stalled_for = flow
                    .last_seen_ts
                    .saturating_sub(flow.first_zero_rx_ts)
                    .max(0) as u64;
                self.finalize_disappeared_stalled(
                    rec_id,
                    &key,
                    flow.has_rx,
                    stalled_for,
                    flow.last_upload,
                    now,
                );
            }
        }

        self.prune_caches(now);
        self.prune(now);
    }

    fn note_success(&mut self, host: &str, now: i64) {
        self.last_success_ts = self.last_success_ts.max(now);
        self.local_host_offline = false;
        let normalized = normalize_domain(host);
        if !normalized.is_empty() && normalized.parse::<IpAddr>().is_err() {
            self.recent_ok_domains.insert(normalized, now);
        }
    }

    fn resolve_recovered_stalled(&mut self, record_id: u64, flow_key: &(String, String)) {
        if let Some(idx) = self.records.iter().position(|e| e.record.id == record_id) {
            let entry = &mut self.records[idx];
            entry.active_stalled_flows.remove(flow_key);
            if entry.active_stalled_flows.is_empty() {
                entry.record.is_stalled_live = false;
                if entry.only_from_stalled {
                    self.records.remove(idx);
                }
            }
        }
    }

    fn finalize_disappeared_stalled(
        &mut self,
        record_id: u64,
        flow_key: &(String, String),
        had_rx: bool,
        stalled_secs: u64,
        last_upload: u64,
        now: i64,
    ) {
        if let Some(idx) = self.records.iter().position(|e| e.record.id == record_id) {
            let entry = &mut self.records[idx];
            entry.active_stalled_flows.remove(flow_key);
            if entry.active_stalled_flows.is_empty() {
                entry.record.is_stalled_live = false;
                if had_rx && entry.only_from_stalled {
                    self.records.remove(idx);
                } else if entry.record.failure_kind == FailureKind::Stalled {
                    entry.only_from_stalled = false;
                    // Without an explicit RST log from the core, a 0-download stalled
                    // connection closing is an unresponsive timeout/drop rather than RST.
                    entry.record.failure_kind = FailureKind::Timeout;
                    let upload = crate::util::format_bytes(last_upload);
                    entry.record.reason_summary =
                        format!("已发送 {upload}，无响应 {stalled_secs} 秒后连接关闭（下行 0 B）");
                    entry.record.raw_detail = format!(
                        "TCP closed after {stalled_secs}s with upload={upload}, download=0 B"
                    );
                    entry.record.last_seen = now;
                }
            }
        }
    }

    fn upsert_stalled_flow(
        &mut self,
        flow_key: &(String, String),
        conn: &ClashConnection,
        elapsed_secs: u64,
        existing_id: Option<u64>,
        model: &AppModel,
        now: i64,
    ) -> Option<u64> {
        if let Some(id) = existing_id
            && let Some(entry) = self.records.iter_mut().find(|e| e.record.id == id)
        {
            entry.record.is_stalled_live = true;
            entry.record.stalled_secs = entry.record.stalled_secs.max(elapsed_secs);
            entry.record.last_seen = now;
            if entry.record.failure_kind == FailureKind::Stalled {
                let upload = crate::util::format_bytes(conn.upload);
                entry.record.reason_summary =
                    format!("已发送 {upload}，持续 {elapsed_secs} 秒无下行返回");
                entry.record.raw_detail =
                    format!("TCP active {elapsed_secs}s with upload={upload}, download=0 B");
            }
            return Some(id);
        }

        let host = conn.host();
        if host.is_empty() || is_local_or_loopback(&host) {
            return None;
        }
        let outbound_tag = conn.chains.first().cloned().unwrap_or_default();
        let (direction, outbound_label) = resolve_outbound(&outbound_tag, "", &conn.chains, model);
        if direction == OutboundDirection::Reject {
            return None;
        }
        if self.is_offline(now) {
            return None;
        }

        let domain = if host.parse::<IpAddr>().is_err() {
            normalize_domain(&host)
        } else {
            String::new()
        };
        let domain_is_sniffed = !domain.is_empty()
            && (!conn.metadata.sniff_host.is_empty()
                || (conn.metadata.kind.to_ascii_lowercase().contains("tun")
                    && conn
                        .metadata
                        .destination_ip
                        .parse::<IpAddr>()
                        .is_ok_and(|ip| !is_fake_ip(ip))));
        let target_key = if !domain.is_empty() {
            domain.clone()
        } else {
            normalize_domain(&conn.metadata.destination_ip)
        };
        let agg_key = AggKey {
            target: target_key,
            outbound: normalized_outbound_key(&outbound_tag, direction),
            is_rejected: false,
        };

        if let Some(entry) = self.records.iter_mut().find(|e| e.agg_key == agg_key) {
            let is_new_flow = entry.active_stalled_flows.insert(flow_key.clone());
            if is_new_flow {
                entry.record.count = entry.record.count.saturating_add(1);
            }
            entry.record.is_stalled_live = true;
            entry.record.stalled_secs = entry.record.stalled_secs.max(elapsed_secs);
            entry.record.last_seen = now;
            entry.record.domain_is_sniffed |= domain_is_sniffed;
            if entry.record.process_name.is_empty() {
                entry.record.process_name = conn.process_name();
                entry.record.process_path = conn.process_path().to_owned();
            }
            if entry.record.matched_rule.is_none() {
                entry.record.matched_rule = format_clash_rule(&conn.rule, &conn.rule_payload);
            }
            if entry.record.failure_kind == FailureKind::Stalled {
                let upload = crate::util::format_bytes(conn.upload);
                entry.record.reason_summary =
                    format!("已发送 {upload}，持续 {elapsed_secs} 秒无下行返回");
                entry.record.raw_detail =
                    format!("TCP active {elapsed_secs}s with upload={upload}, download=0 B");
            }
            return Some(entry.record.id);
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let root_domain = if !domain.is_empty() {
            extract_root_domain(&domain)
        } else {
            None
        };
        let mut active_stalled_flows = HashSet::new();
        active_stalled_flows.insert(flow_key.clone());
        let upload = crate::util::format_bytes(conn.upload);
        let record = FailedConnectionRecord {
            id,
            domain,
            domain_is_sniffed,
            root_domain,
            destination_ip: conn.metadata.destination_ip.clone(),
            destination_port: conn.metadata.destination_port.clone(),
            network: conn.metadata.network.to_ascii_lowercase(),
            inbound: conn.metadata.kind.clone(),
            process_name: conn.process_name(),
            process_path: conn.process_path().to_owned(),
            outbound_tag,
            outbound_label,
            outbound_direction: direction,
            matched_rule: format_clash_rule(&conn.rule, &conn.rule_payload),
            failure_kind: FailureKind::Stalled,
            reason_summary: format!("已发送 {upload}，持续 {elapsed_secs} 秒无下行返回"),
            raw_detail: format!("TCP active {elapsed_secs}s with upload={upload}, download=0 B"),
            is_stalled_live: true,
            stalled_secs: elapsed_secs,
            count: 1,
            first_seen: now,
            last_seen: now,
            applied_rule: None,
        };
        self.records.push(InternalRecord {
            agg_key,
            record,
            active_stalled_flows,
            only_from_stalled: true,
        });
        self.prune(now);
        Some(id)
    }

    fn observe_singbox_line(&mut self, line: &str, model: &AppModel, now: i64) {
        let Some((level, cid, elapsed_ms, body)) = parse_singbox_envelope(line) else {
            return;
        };

        // 1. Track successful DNS exchanges to suppress harmless AAAA failures.
        if let Some(rest) = body.strip_prefix("dns: exchanged ") {
            if let Some(domain) = rest.split_whitespace().next() {
                let normalized = normalize_domain(domain);
                if !normalized.is_empty() && normalized.parse::<IpAddr>().is_err() {
                    self.recent_ok_domains.insert(normalized, now);
                }
            }
            return;
        }

        // 2. DNS exchange failure.
        if let Some(rest) = body.strip_prefix("dns: exchange failed for ") {
            if let Some((domain, qtype, err)) = parse_singbox_dns_failure(rest) {
                let normalized = normalize_domain(&domain);
                if normalized.is_empty()
                    || normalized.parse::<IpAddr>().is_ok()
                    || is_local_or_loopback(&normalized)
                {
                    return;
                }
                if qtype.eq_ignore_ascii_case("AAAA")
                    && self
                        .recent_ok_domains
                        .get(&normalized)
                        .is_some_and(|ts| now.saturating_sub(*ts) <= DNS_OK_SUPPRESS_SECS)
                {
                    return;
                }
                if is_ignorable_close_error(&err) {
                    return;
                }
                let ctx = cid
                    .and_then(|id| self.singbox_contexts.get(id))
                    .cloned()
                    .unwrap_or_default();
                self.record_failure(
                    FailureEvent {
                        domain: normalized,
                        domain_is_sniffed: false,
                        destination_ip: String::new(),
                        destination_port: "53".into(),
                        network: "dns".into(),
                        inbound: ctx.inbound,
                        source_addr: ctx.source_addr,
                        process_name: ctx.process_name,
                        process_path: ctx.process_path,
                        outbound_tag: if ctx.outbound_tag.is_empty() {
                            "direct".into()
                        } else {
                            ctx.outbound_tag
                        },
                        outbound_type: ctx.outbound_type,
                        matched_rule: ctx.matched_rule,
                        failure_kind: FailureKind::DnsFailed,
                        reason_summary: human_failure_summary(FailureKind::DnsFailed, &err),
                        raw_detail: truncate_detail(body),
                    },
                    model,
                    now,
                );
            }
            return;
        }

        // 3. Context-building lines keyed by sing-box connection ID.
        if let Some(cid) = cid {
            if let Some((inbound, rest)) = parse_singbox_inbound_prefix(body) {
                if let Some(src) = rest
                    .strip_prefix("inbound connection from ")
                    .or_else(|| rest.strip_prefix("inbound packet connection from "))
                {
                    let network = if rest.contains(" packet ") {
                        "udp"
                    } else {
                        "tcp"
                    };
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.inbound = inbound.to_owned();
                    ctx.network = network.into();
                    ctx.source_addr = src.trim().to_owned();
                    return;
                }
                if let Some(dst) = rest
                    .strip_prefix("inbound connection to ")
                    .or_else(|| rest.strip_prefix("inbound packet connection to "))
                {
                    let network = if rest.contains(" packet ") {
                        "udp"
                    } else {
                        "tcp"
                    };
                    let (host, port) = split_host_port(dst.trim());
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.inbound = inbound.to_owned();
                    ctx.network = network.into();
                    assign_host_to_context(ctx, host, port);
                    return;
                }
                if let Some((src, err)) = parse_singbox_process_conn_error(rest) {
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.inbound = inbound.to_owned();
                    if ctx.source_addr.is_empty() {
                        ctx.source_addr = src.to_owned();
                    }
                    let ctx_snapshot = ctx.clone();
                    if is_rejected_error(err) {
                        self.emit_context_failure(
                            &ctx_snapshot,
                            FailureKind::Rejected,
                            err,
                            body,
                            model,
                            now,
                        );
                    } else if level == "ERROR"
                        && let Some(kind) = classify_transport_error(err, elapsed_ms)
                    {
                        self.emit_context_failure(&ctx_snapshot, kind, err, body, model, now);
                    }
                    return;
                }
                if let Some(err) = rest.strip_prefix("connection closed: ") {
                    if is_rejected_error(err) {
                        let ctx_snapshot = self.touch_singbox_context(cid, now).clone();
                        self.emit_context_failure(
                            &ctx_snapshot,
                            FailureKind::Rejected,
                            err,
                            body,
                            model,
                            now,
                        );
                        return;
                    }
                    if level == "ERROR"
                        && let Some(kind) = classify_transport_error(err, elapsed_ms)
                    {
                        let ctx_snapshot = self.touch_singbox_context(cid, now).clone();
                        self.emit_context_failure(&ctx_snapshot, kind, err, body, model, now);
                        return;
                    }
                }
            }

            if let Some(rest) = body.strip_prefix("router: ") {
                if let Some(domain) = extract_sniffed_domain(rest) {
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.sniff_domain = normalize_domain(domain);
                    return;
                }
                if let Some(raw_path) = rest
                    .strip_prefix("found process path: ")
                    .or_else(|| rest.strip_prefix("found process: "))
                {
                    let raw_path = raw_path.trim();
                    let path = raw_path
                        .split_once(", user:")
                        .or_else(|| raw_path.split_once(", uid:"))
                        .or_else(|| raw_path.split_once(", pid:"))
                        .map_or(raw_path, |(head, _)| head)
                        .trim();
                    let name = crate::flow::process_basename("", path).to_owned();
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.process_path = path.to_owned();
                    if !name.is_empty() {
                        ctx.process_name = name;
                    }
                    return;
                }
                if let Some((rule_desc, target)) = parse_singbox_router_match(rest) {
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.matched_rule = Some(rule_desc.to_owned());
                    if target.eq_ignore_ascii_case("reject") || target.starts_with("reject(") {
                        ctx.outbound_tag = "reject".into();
                        ctx.outbound_type = "reject".into();
                        let ctx_snapshot = ctx.clone();
                        self.emit_context_failure(
                            &ctx_snapshot,
                            FailureKind::Rejected,
                            rest,
                            body,
                            model,
                            now,
                        );
                    } else {
                        ctx.outbound_tag = target.to_owned();
                    }
                    return;
                }
                if rest.eq_ignore_ascii_case("rejected")
                    || rest.starts_with("rejected ")
                    || rest.starts_with("rejected:")
                    || rest.starts_with("connection closed: rejected")
                    || rest.starts_with("connection rejected")
                {
                    let ctx = self.touch_singbox_context(cid, now);
                    ctx.outbound_tag = "reject".into();
                    ctx.outbound_type = "reject".into();
                    let ctx_snapshot = ctx.clone();
                    self.emit_context_failure(
                        &ctx_snapshot,
                        FailureKind::Rejected,
                        rest,
                        body,
                        model,
                        now,
                    );
                    return;
                }
            }

            if let Some((out_type, out_tag, rest)) = parse_singbox_outbound_prefix(body)
                && let Some(dst) = rest
                    .strip_prefix("outbound connection to ")
                    .or_else(|| rest.strip_prefix("outbound packet connection to "))
            {
                let (host, port) = split_host_port(dst.trim());
                let ctx = self.touch_singbox_context(cid, now);
                ctx.outbound_type = out_type.to_owned();
                ctx.outbound_tag = out_tag.to_owned();
                if ctx.network.is_empty() {
                    ctx.network = if rest.contains(" packet ") {
                        "udp".into()
                    } else {
                        "tcp".into()
                    };
                }
                assign_host_to_context(ctx, host, port);
                return;
            }
        }

        // 4. Connection open / transfer failure lines (may or may not have prior context).
        if let Some(rest) = body.strip_prefix("connection: ") {
            if let Some((dst, out_type, out_tag, err)) = parse_singbox_open_connection_error(rest) {
                if is_ignorable_close_error(err) {
                    return;
                }
                let mut ctx = cid
                    .and_then(|id| self.singbox_contexts.get(id))
                    .cloned()
                    .unwrap_or_default();
                let (host, port) = split_host_port(dst);
                assign_host_to_context(&mut ctx, host, port);
                if !out_type.is_empty() {
                    ctx.outbound_type = out_type.to_owned();
                }
                if !out_tag.is_empty() {
                    ctx.outbound_tag = out_tag.to_owned();
                }
                if ctx.network.is_empty() {
                    ctx.network = if rest.contains("packet") {
                        "udp".into()
                    } else {
                        "tcp".into()
                    };
                }
                let kind = if is_rejected_error(err) {
                    FailureKind::Rejected
                } else {
                    classify_dial_error(err)
                };
                self.emit_context_failure(&ctx, kind, err, body, model, now);
                return;
            }

            if let Some(err) = rest
                .strip_prefix("connection download closed: ")
                .or_else(|| rest.strip_prefix("connection upload closed: "))
                .or_else(|| rest.strip_prefix("packet connection closed: "))
            {
                if level != "ERROR" && !is_rejected_error(err) {
                    return;
                }
                // Sometimes sing-box embeds "open connection to ... using outbound/..." inside download closed.
                if let Some(open_pos) = err.find("open connection to ")
                    && let Some((dst, out_type, out_tag, inner_err)) =
                        parse_singbox_open_connection_error(&err[open_pos..])
                {
                    if is_ignorable_close_error(inner_err) {
                        return;
                    }
                    let mut ctx = cid
                        .and_then(|id| self.singbox_contexts.get(id))
                        .cloned()
                        .unwrap_or_default();
                    let (host, port) = split_host_port(dst);
                    assign_host_to_context(&mut ctx, host, port);
                    if !out_type.is_empty() {
                        ctx.outbound_type = out_type.to_owned();
                    }
                    if !out_tag.is_empty() {
                        ctx.outbound_tag = out_tag.to_owned();
                    }
                    let kind = classify_dial_error(inner_err);
                    self.emit_context_failure(&ctx, kind, inner_err, body, model, now);
                    return;
                }
                let Some(ctx) = cid.and_then(|id| self.singbox_contexts.get(id)).cloned() else {
                    return;
                };
                if is_rejected_error(err) {
                    self.emit_context_failure(&ctx, FailureKind::Rejected, err, body, model, now);
                } else if let Some(kind) = classify_transport_error(err, elapsed_ms) {
                    self.emit_context_failure(&ctx, kind, err, body, model, now);
                }
            }
        }
    }

    fn observe_xray_line(&mut self, line: &str, model: &AppModel, now: i64) {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("failed to")
            && !lower.contains("i/o timeout")
            && !lower.contains("connection reset")
            && !lower.contains("connection refused")
        {
            return;
        }
        if is_ignorable_close_error(line) {
            return;
        }

        // DNS failure in Xray: "app/dns: failed to lookup ip for domain example.com > ..."
        if let Some(idx) = lower.find("for domain ") {
            let after = &line[idx + "for domain ".len()..];
            let domain = after
                .split(|c: char| c.is_whitespace() || matches!(c, '>' | ':' | ',' | ')'))
                .next()
                .unwrap_or_default();
            let normalized = normalize_domain(domain);
            if !normalized.is_empty()
                && normalized.parse::<IpAddr>().is_err()
                && !is_local_or_loopback(&normalized)
            {
                self.record_failure(
                    FailureEvent {
                        domain: normalized,
                        domain_is_sniffed: false,
                        destination_ip: String::new(),
                        destination_port: "53".into(),
                        network: "dns".into(),
                        inbound: String::new(),
                        source_addr: String::new(),
                        process_name: String::new(),
                        process_path: String::new(),
                        outbound_tag: "direct".into(),
                        outbound_type: "dns".into(),
                        matched_rule: None,
                        failure_kind: FailureKind::DnsFailed,
                        reason_summary: human_failure_summary(FailureKind::DnsFailed, line),
                        raw_detail: truncate_detail(line),
                    },
                    model,
                    now,
                );
                return;
            }
        }

        // Target extraction: "tcp:host:port" or "udp:host:port"
        let Some((network, host, port)) = extract_xray_target(line) else {
            return;
        };
        if host.is_empty() || is_local_or_loopback(host) {
            return;
        }

        let (outbound_tag, outbound_type) = if lower.contains("proxy/freedom") {
            ("direct".to_owned(), "direct".to_owned())
        } else if lower.contains("proxy/blackhole") {
            ("block".to_owned(), "reject".to_owned())
        } else if lower.contains("proxy/") {
            ("proxy".to_owned(), "proxy".to_owned())
        } else {
            (String::new(), String::new())
        };

        let kind = if outbound_type == "reject" {
            FailureKind::Rejected
        } else {
            classify_dial_error(line)
        };
        let (domain, destination_ip) = if host.parse::<IpAddr>().is_ok() {
            (String::new(), host.trim_matches(['[', ']']).to_owned())
        } else {
            (normalize_domain(host), String::new())
        };

        self.record_failure(
            FailureEvent {
                domain,
                domain_is_sniffed: false,
                destination_ip,
                destination_port: port.to_owned(),
                network: network.to_owned(),
                inbound: String::new(),
                source_addr: String::new(),
                process_name: String::new(),
                process_path: String::new(),
                outbound_tag,
                outbound_type,
                matched_rule: None,
                failure_kind: kind,
                reason_summary: human_failure_summary(kind, line),
                raw_detail: truncate_detail(line),
            },
            model,
            now,
        );
    }

    fn touch_singbox_context(&mut self, cid: &str, now: i64) -> &mut SingBoxConnContext {
        if self.singbox_contexts.len() >= MAX_CONTEXTS && !self.singbox_contexts.contains_key(cid) {
            self.singbox_contexts
                .retain(|_, ctx| now.saturating_sub(ctx.updated_at) <= CONTEXT_TTL_SECS);
            if self.singbox_contexts.len() >= MAX_CONTEXTS {
                let oldest = self
                    .singbox_contexts
                    .iter()
                    .min_by_key(|(_, ctx)| ctx.updated_at)
                    .map(|(k, _)| k.clone());
                if let Some(key) = oldest {
                    self.singbox_contexts.remove(&key);
                }
            }
        }
        let ctx = self.singbox_contexts.entry(cid.to_owned()).or_default();
        ctx.updated_at = now;
        ctx
    }

    fn emit_context_failure(
        &mut self,
        ctx: &SingBoxConnContext,
        kind: FailureKind,
        err: &str,
        body: &str,
        model: &AppModel,
        now: i64,
    ) {
        let domain = if !ctx.sniff_domain.is_empty() {
            ctx.sniff_domain.clone()
        } else if !ctx.target_host.is_empty() && ctx.target_host.parse::<IpAddr>().is_err() {
            normalize_domain(&ctx.target_host)
        } else {
            String::new()
        };
        let destination_ip = if !ctx.destination_ip.is_empty() {
            ctx.destination_ip.clone()
        } else if ctx.target_host.parse::<IpAddr>().is_ok() {
            ctx.target_host.clone()
        } else {
            String::new()
        };
        let domain_is_sniffed = !ctx.sniff_domain.is_empty()
            && (ctx.target_host.is_empty() || ctx.target_host.parse::<IpAddr>().is_ok());
        self.record_failure(
            FailureEvent {
                domain,
                domain_is_sniffed,
                destination_ip,
                destination_port: ctx.destination_port.clone(),
                network: if ctx.network.is_empty() {
                    "tcp".into()
                } else {
                    ctx.network.clone()
                },
                inbound: ctx.inbound.clone(),
                source_addr: ctx.source_addr.clone(),
                process_name: ctx.process_name.clone(),
                process_path: ctx.process_path.clone(),
                outbound_tag: ctx.outbound_tag.clone(),
                outbound_type: ctx.outbound_type.clone(),
                matched_rule: ctx.matched_rule.clone(),
                failure_kind: kind,
                reason_summary: human_failure_summary(kind, err),
                raw_detail: truncate_detail(body),
            },
            model,
            now,
        );
    }

    fn record_failure(&mut self, mut event: FailureEvent, model: &AppModel, now: i64) {
        // Enrich from recent Clash API connection metadata when log lacks process/rule/domain.
        let enrich = self.lookup_enrichment(&event, now);
        if let Some(meta) = enrich {
            if event.process_name.is_empty() && !meta.process_name.is_empty() {
                event.process_name = meta.process_name;
            }
            if event.process_path.is_empty() && !meta.process_path.is_empty() {
                event.process_path = meta.process_path;
            }
            if event.matched_rule.is_none() && meta.matched_rule.is_some() {
                event.matched_rule = meta.matched_rule;
            }
            if event.outbound_tag.is_empty() && !meta.outbound_tag.is_empty() {
                event.outbound_tag = meta.outbound_tag;
            }
            if event.inbound.is_empty() && !meta.inbound.is_empty() {
                event.inbound = meta.inbound;
            }
            if event.destination_ip.is_empty() && !meta.destination_ip.is_empty() {
                event.destination_ip = meta.destination_ip;
            }
            if event.domain.is_empty() && !meta.domain.is_empty() {
                event.domain = meta.domain;
            }
        }

        if is_protected_process(&event.process_name) || is_protected_process(&event.process_path) {
            return;
        }
        let target_host = if !event.domain.is_empty() {
            event.domain.clone()
        } else {
            normalize_domain(&event.destination_ip)
        };
        if target_host.is_empty() || is_local_or_loopback(&target_host) {
            return;
        }

        let (direction, outbound_label) = if event.failure_kind == FailureKind::Rejected {
            (
                OutboundDirection::Reject,
                event
                    .matched_rule
                    .as_deref()
                    .map(|r| format!("阻止 · {r}"))
                    .unwrap_or_else(|| "规则拦截".into()),
            )
        } else {
            resolve_outbound(&event.outbound_tag, &event.outbound_type, &[], model)
        };

        // Host offline burst guard (only suppresses non-Rejected network errors when both
        // Direct and Proxy outbounds fail simultaneously without recent success).
        if event.failure_kind != FailureKind::Rejected {
            self.record_burst_signal(direction, &target_host, now);
            if self.is_offline(now) {
                return;
            }
        }

        let is_rejected = event.failure_kind == FailureKind::Rejected;
        let agg_key = AggKey {
            target: target_host,
            outbound: normalized_outbound_key(&event.outbound_tag, direction),
            is_rejected,
        };

        if let Some(existing) = self.records.iter_mut().find(|e| e.agg_key == agg_key) {
            existing.only_from_stalled = false;
            existing.record.count = existing.record.count.saturating_add(1);
            existing.record.last_seen = now;
            existing.record.failure_kind = event.failure_kind;
            existing.record.reason_summary = event.reason_summary;
            existing.record.raw_detail = event.raw_detail;
            existing.record.is_stalled_live = false;
            existing.record.domain_is_sniffed |= event.domain_is_sniffed;
            if existing.record.destination_ip.is_empty() && !event.destination_ip.is_empty() {
                existing.record.destination_ip = event.destination_ip;
            }
            if existing.record.destination_port.is_empty() && !event.destination_port.is_empty() {
                existing.record.destination_port = event.destination_port;
            }
            if existing.record.process_name.is_empty() && !event.process_name.is_empty() {
                existing.record.process_name = event.process_name;
                existing.record.process_path = event.process_path;
            }
            if existing.record.matched_rule.is_none() && event.matched_rule.is_some() {
                existing.record.matched_rule = event.matched_rule;
            }
            if existing.record.inbound.is_empty() && !event.inbound.is_empty() {
                existing.record.inbound = event.inbound;
            }
            return;
        }

        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let root_domain = if !event.domain.is_empty() {
            extract_root_domain(&event.domain)
        } else {
            None
        };
        let record = FailedConnectionRecord {
            id,
            domain: event.domain,
            domain_is_sniffed: event.domain_is_sniffed,
            root_domain,
            destination_ip: event.destination_ip,
            destination_port: event.destination_port,
            network: event.network,
            inbound: event.inbound,
            process_name: event.process_name,
            process_path: event.process_path,
            outbound_tag: event.outbound_tag,
            outbound_label,
            outbound_direction: direction,
            matched_rule: event.matched_rule,
            failure_kind: event.failure_kind,
            reason_summary: event.reason_summary,
            raw_detail: event.raw_detail,
            is_stalled_live: false,
            stalled_secs: 0,
            count: 1,
            first_seen: now,
            last_seen: now,
            applied_rule: None,
        };
        self.records.push(InternalRecord {
            agg_key,
            record,
            active_stalled_flows: HashSet::new(),
            only_from_stalled: false,
        });
        self.prune(now);
    }

    fn lookup_enrichment(&self, event: &FailureEvent, now: i64) -> Option<ClashMetaSnapshot> {
        if !event.source_addr.is_empty()
            && let Some(meta) = self.enrich_by_source.get(&event.source_addr)
            && now.saturating_sub(meta.updated_at) <= ENRICH_TTL_SECS
        {
            return Some(meta.clone());
        }
        let host = if !event.domain.is_empty() {
            event.domain.as_str()
        } else {
            event.destination_ip.as_str()
        };
        if !host.is_empty() && !event.destination_port.is_empty() {
            let key = (normalize_domain(host), event.destination_port.clone());
            if let Some(meta) = self.enrich_by_dest.get(&key)
                && now.saturating_sub(meta.updated_at) <= ENRICH_TTL_SECS
            {
                return Some(meta.clone());
            }
        }
        None
    }

    fn record_burst_signal(&mut self, direction: OutboundDirection, target: &str, now: i64) {
        self.failure_burst
            .push_back((now, direction, target.to_owned()));
        while self
            .failure_burst
            .front()
            .is_some_and(|(ts, _, _)| now.saturating_sub(*ts) > OFFLINE_BURST_WINDOW_SECS)
        {
            self.failure_burst.pop_front();
        }
        if now.saturating_sub(self.last_success_ts) > OFFLINE_BURST_WINDOW_SECS {
            let mut has_direct = false;
            let mut has_proxy = false;
            let mut targets = HashSet::new();
            for (_, dir, t) in &self.failure_burst {
                match dir {
                    OutboundDirection::Direct => has_direct = true,
                    OutboundDirection::Proxy => has_proxy = true,
                    _ => {}
                }
                targets.insert(t.as_str());
            }
            if has_direct && has_proxy && targets.len() >= 3 {
                self.local_host_offline = true;
            }
        }
    }

    fn is_offline(&self, now: i64) -> bool {
        if self.external_host_offline {
            return true;
        }
        if !self.local_host_offline {
            return false;
        }
        self.failure_burst
            .back()
            .is_some_and(|(ts, _, _)| now.saturating_sub(*ts) <= OFFLINE_BURST_WINDOW_SECS)
    }

    fn prune_caches(&mut self, now: i64) {
        self.recent_ok_domains
            .retain(|_, ts| now.saturating_sub(*ts) <= DNS_OK_SUPPRESS_SECS);
        if self.enrich_by_dest.len() > MAX_ENRICH_ENTRIES {
            self.enrich_by_dest
                .retain(|_, meta| now.saturating_sub(meta.updated_at) <= ENRICH_TTL_SECS);
        }
        if self.enrich_by_source.len() > MAX_ENRICH_ENTRIES {
            self.enrich_by_source
                .retain(|_, meta| now.saturating_sub(meta.updated_at) <= ENRICH_TTL_SECS);
        }
        if self.singbox_contexts.len() > MAX_CONTEXTS / 2 {
            self.singbox_contexts
                .retain(|_, ctx| now.saturating_sub(ctx.updated_at) <= CONTEXT_TTL_SECS);
        }
    }

    fn prune(&mut self, now: i64) {
        self.records.retain(|entry| {
            entry.record.is_stalled_live
                || now.saturating_sub(entry.record.last_seen) <= FAILED_RECORD_TTL_SECS
        });
        if self.records.len() > FAILED_RECORD_LIMIT {
            self.records.sort_by(|a, b| {
                b.record
                    .last_seen
                    .cmp(&a.record.last_seen)
                    .then_with(|| b.record.id.cmp(&a.record.id))
            });
            self.records.truncate(FAILED_RECORD_LIMIT);
        }
    }
}

/// Builds a human-friendly diagnostic rule name for a failed-connection fix.
pub fn rule_name_for_expression(expression: &str) -> String {
    format!("快速修复 · {expression}")
}

/// Builds matchers for a specific rule kind from a failed connection (used in context menus).
pub fn matchers_for_record_kind(
    record: &FailedConnectionRecord,
    kind: DiagnosticRuleKind,
) -> Result<(String, Matchers)> {
    let target = record.diagnostic_target();
    let matchers = target.matchers(kind)?;
    let expr = match kind {
        DiagnosticRuleKind::Domain => target.domain,
        DiagnosticRuleKind::Ip => matchers.ip_cidrs.first().cloned().unwrap_or_default(),
        DiagnosticRuleKind::Process => format!("process:{}", target.process),
    };
    Ok((expr, matchers))
}

fn normalized_outbound_key(tag: &str, dir: OutboundDirection) -> String {
    if !tag.is_empty() {
        tag.to_ascii_lowercase()
    } else {
        match dir {
            OutboundDirection::Direct => "direct".into(),
            OutboundDirection::Proxy => "proxy".into(),
            OutboundDirection::Reject => "reject".into(),
            OutboundDirection::Unknown => "unknown".into(),
        }
    }
}

fn resolve_outbound(
    tag: &str,
    out_type: &str,
    chains: &[String],
    model: &AppModel,
) -> (OutboundDirection, String) {
    let lower_tag = tag.to_ascii_lowercase();
    let lower_type = out_type.to_ascii_lowercase();
    if lower_tag == "reject" || lower_tag == "block" || lower_type == "reject" {
        return (OutboundDirection::Reject, "规则拦截".into());
    }
    if lower_tag == "direct" || lower_type == "direct" {
        return (OutboundDirection::Direct, "直连".into());
    }
    if lower_type == "dns" {
        return (OutboundDirection::Direct, "DNS".into());
    }
    if let Some(node) = model.nodes.iter().find(|n| n.outbound_tag() == tag) {
        return (OutboundDirection::Proxy, format!("代理 · {}", node.name));
    }
    if let Some(group) = model.groups.iter().find(|g| g.tag() == tag) {
        return (OutboundDirection::Proxy, format!("代理 · {}", group.name));
    }
    if !chains.is_empty() {
        let mut names = Vec::new();
        let mut is_direct = true;
        for chain in chains {
            if chain.eq_ignore_ascii_case("direct") {
                names.push("直连".to_owned());
                continue;
            }
            if chain.eq_ignore_ascii_case("reject") || chain.eq_ignore_ascii_case("block") {
                return (OutboundDirection::Reject, "规则拦截".into());
            }
            is_direct = false;
            if let Some(node) = model.nodes.iter().find(|n| n.outbound_tag() == *chain) {
                names.push(node.name.clone());
            } else if let Some(group) = model.groups.iter().find(|g| g.tag() == *chain) {
                names.push(group.name.clone());
            } else {
                names.push(chain.clone());
            }
        }
        let dir = if is_direct {
            OutboundDirection::Direct
        } else {
            OutboundDirection::Proxy
        };
        return (dir, names.join(" → "));
    }
    if !tag.is_empty() {
        if tag.starts_with("node-") || tag.starts_with("group-") || lower_tag == "proxy" {
            return (OutboundDirection::Proxy, format!("代理 · {tag}"));
        }
        return (OutboundDirection::Proxy, tag.to_owned());
    }
    (OutboundDirection::Unknown, "未知出站".into())
}

fn format_clash_rule(rule: &str, payload: &str) -> Option<String> {
    if rule.is_empty() {
        None
    } else if payload.is_empty() {
        Some(rule.to_owned())
    } else {
        Some(format!("{rule} · {payload}"))
    }
}

fn is_local_or_loopback(host: &str) -> bool {
    let trimmed = host.trim().trim_matches(['[', ']']);
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return ip.is_loopback() || ip.is_unspecified() || ip.is_multicast();
    }
    false
}

fn truncate_detail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= RAW_DETAIL_LIMIT {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(RAW_DETAIL_LIMIT - 1).collect();
    out.push('…');
    out
}

fn assign_host_to_context(ctx: &mut SingBoxConnContext, host: &str, port: &str) {
    let host = host.trim().trim_matches(['[', ']']);
    if !host.is_empty() {
        if host.parse::<IpAddr>().is_ok() {
            if ctx.destination_ip.is_empty() {
                ctx.destination_ip = host.to_owned();
            }
            if ctx.target_host.is_empty() {
                ctx.target_host = host.to_owned();
            }
        } else {
            ctx.target_host = normalize_domain(host);
        }
    }
    if !port.is_empty() && ctx.destination_port.is_empty() {
        ctx.destination_port = port.to_owned();
    }
}

fn format_host_port(ip: &str, port: &str) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

fn split_host_port(endpoint: &str) -> (&str, &str) {
    let s = endpoint.trim().trim_end_matches([',', ';', ')']);
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            let port = after.strip_prefix(':').unwrap_or_default();
            return (host, port);
        }
    }
    if let Some((host, port)) = s.rsplit_once(':')
        && port.chars().all(|c| c.is_ascii_digit())
        && !port.is_empty()
    {
        return (host, port);
    }
    (s, "")
}

/// Parses a sing-box log line into `(level, Option<conn_id>, Option<elapsed_ms>, body)`.
fn parse_singbox_envelope(line: &str) -> Option<(&'static str, Option<&str>, Option<u64>, &str)> {
    let levels = [
        ("TRACE", "TRACE"),
        ("DEBUG", "DEBUG"),
        ("INFO", "INFO"),
        ("WARN", "WARN"),
        ("WARNING", "WARN"),
        ("ERROR", "ERROR"),
        ("FATAL", "ERROR"),
    ];
    let mut found: Option<(&'static str, usize, usize)> = None;
    for (needle, norm) in levels {
        for (idx, _) in line.match_indices(needle) {
            let before_ok = idx == 0 || line.as_bytes()[idx - 1].is_ascii_whitespace();
            let end = idx + needle.len();
            let after_byte = line.as_bytes().get(end).copied();
            let after_ok = matches!(after_byte, Some(b' ') | Some(b'['));
            if before_ok && after_ok {
                if found.is_none_or(|(_, best_idx, _)| idx < best_idx) {
                    found = Some((norm, idx, end));
                }
                break;
            }
        }
    }
    let (level, _, end) = found?;
    let mut rest = &line[end..];
    // Handle `ERROR[0002]` style elapsed prefix right after level token.
    if rest.starts_with('[')
        && let Some(close) = rest.find(']')
    {
        let inside = &rest[1..close];
        if !inside.contains(' ') && inside.chars().all(|c| c.is_ascii_digit()) {
            rest = &rest[close + 1..];
        }
    }
    rest = rest.trim_start();
    let (cid, elapsed_ms, body) = if let Some(after_open) = rest.strip_prefix('[')
        && let Some((inside, tail)) = after_open.split_once(']')
    {
        let mut parts = inside.split_whitespace();
        let id = parts
            .next()
            .filter(|s| s.chars().all(|c| c.is_ascii_digit()));
        let elapsed = parts.next().and_then(parse_go_duration_ms);
        if let Some(id) = id {
            (Some(id), elapsed, tail.trim_start())
        } else {
            (None, None, rest)
        }
    } else {
        (None, None, rest)
    };
    Some((level, cid, elapsed_ms, body))
}

fn parse_go_duration_ms(text: &str) -> Option<u64> {
    let s = text.trim();
    if let Some(v) = s.strip_suffix("ms") {
        return v.parse::<f64>().ok().map(|n| n.max(0.0).round() as u64);
    }
    if let Some(v) = s.strip_suffix("µs").or_else(|| s.strip_suffix("us")) {
        return v
            .parse::<f64>()
            .ok()
            .map(|n| (n / 1000.0).max(0.0).round() as u64);
    }
    if let Some(v) = s.strip_suffix("ns") {
        return v
            .parse::<f64>()
            .ok()
            .map(|n| (n / 1_000_000.0).max(0.0).round() as u64);
    }
    if let Some(v) = s.strip_suffix('s') {
        if let Some((mins, secs)) = v.split_once('m') {
            let m = mins.parse::<f64>().ok()?;
            let s = secs.parse::<f64>().ok()?;
            return Some(((m * 60.0 + s) * 1000.0).max(0.0).round() as u64);
        }
        return v
            .parse::<f64>()
            .ok()
            .map(|n| (n * 1000.0).max(0.0).round() as u64);
    }
    None
}

fn parse_singbox_inbound_prefix(body: &str) -> Option<(&str, &str)> {
    let rest = body.strip_prefix("inbound/")?;
    let (prefix, tail) = rest.split_once(": ")?;
    let kind = prefix.split('[').next().unwrap_or(prefix);
    Some((kind, tail))
}

fn parse_singbox_outbound_prefix(body: &str) -> Option<(&str, &str, &str)> {
    let rest = body.strip_prefix("outbound/")?;
    let (prefix, tail) = rest.split_once(": ")?;
    let (out_type, out_tag) = parse_typed_tag(prefix);
    Some((out_type, out_tag, tail))
}

fn parse_typed_tag(spec: &str) -> (&str, &str) {
    if let Some((kind, after)) = spec.split_once('[')
        && let Some(tag) = after.strip_suffix(']')
    {
        return (kind, tag);
    }
    (spec, spec)
}

fn parse_singbox_open_connection_error(rest: &str) -> Option<(&str, &str, &str, &str)> {
    let after = rest
        .strip_prefix("open connection to ")
        .or_else(|| rest.strip_prefix("open packet connection to "))?;
    let (dst, tail) = after.split_once(" using ")?;
    let (out_spec, err) = tail.split_once(": ")?;
    let out_spec = out_spec.strip_prefix("outbound/").unwrap_or(out_spec);
    let (out_type, out_tag) = parse_typed_tag(out_spec);
    Some((dst.trim(), out_type, out_tag, err.trim()))
}

fn parse_singbox_process_conn_error(rest: &str) -> Option<(&str, &str)> {
    let after = rest
        .strip_prefix("process connection from ")
        .or_else(|| rest.strip_prefix("process packet connection from "))?;
    let (src, err) = after.split_once(": ")?;
    Some((src.trim(), err.trim()))
}

fn extract_sniffed_domain(rest: &str) -> Option<&str> {
    if !rest.starts_with("sniffed ") {
        return None;
    }
    let idx = rest.find("domain: ")?;
    let domain = rest[idx + "domain: ".len()..]
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()?;
    (!domain.is_empty()).then_some(domain)
}

fn parse_singbox_router_match(rest: &str) -> Option<(&str, &str)> {
    if !rest.starts_with("match[") {
        return None;
    }
    let (rule, target) = rest.split_once(" => ")?;
    let rule = rule
        .split_once("] ")
        .map(|(_, desc)| desc.trim())
        .unwrap_or(rule);
    Some((rule, target.trim()))
}

fn parse_singbox_dns_failure(rest: &str) -> Option<(String, String, String)> {
    let (lhs, err) = rest.split_once(": ")?;
    let mut parts = lhs.split_whitespace();
    let domain = parts.next()?.to_owned();
    let second = parts.next().unwrap_or_default();
    let qtype = if second.eq_ignore_ascii_case("IN") {
        parts.next().unwrap_or_default().to_owned()
    } else {
        second.to_owned()
    };
    Some((domain, qtype, err.trim().to_owned()))
}

fn extract_xray_target(line: &str) -> Option<(&'static str, &str, &str)> {
    for (prefix, net) in [("tcp:", "tcp"), ("udp:", "udp")] {
        let mut search = line;
        while let Some(pos) = search.find(prefix) {
            let candidate = &search[pos + prefix.len()..];
            let token = candidate
                .split(|c: char| c.is_whitespace() || matches!(c, ')' | '>' | ',' | ';' | '"'))
                .next()
                .unwrap_or_default();
            let (host, port) = split_host_port(token);
            if !host.is_empty() && !port.is_empty() && !is_local_or_loopback(host) {
                return Some((net, host, port));
            }
            search = &search[pos + prefix.len()..];
        }
    }
    None
}

fn is_rejected_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower == "rejected"
        || lower.starts_with("rejected:")
        || lower.ends_with(": rejected")
        || lower.contains("action=reject")
        || lower.contains("blocked by rule")
}

fn is_ignorable_close_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("context canceled")
        || lower.contains("operation was canceled")
        || lower.contains("use of closed network connection")
        || lower.contains("closed pipe")
}

fn classify_dial_error(err: &str) -> FailureKind {
    let lower = err.to_ascii_lowercase();
    if is_rejected_error(err) {
        return FailureKind::Rejected;
    }
    if lower.contains("i/o timeout")
        || lower.contains("timed out")
        || lower.contains("deadline exceeded")
        || lower.contains("timeout")
    {
        return FailureKind::Timeout;
    }
    if lower.contains("tls")
        || lower.contains("x509")
        || lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("reality")
    {
        return FailureKind::TlsError;
    }
    if lower.contains("no such host")
        || lower.contains("lookup ")
        || lower.contains("dns")
        || lower.contains("name error")
        || lower.contains("servfail")
    {
        return FailureKind::DnsFailed;
    }
    if lower.contains("reset by peer")
        || lower.contains("broken pipe")
        || lower.contains("forcibly closed")
        || lower.contains("connection aborted")
        || lower.contains("unexpected eof")
        || lower.ends_with(": eof")
        || lower == "eof"
    {
        return FailureKind::Reset;
    }
    if lower.contains("connection refused")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("host is unreachable")
        || lower.contains("cannot assign requested address")
    {
        return FailureKind::Unreachable;
    }
    FailureKind::Other
}

fn classify_transport_error(err: &str, elapsed_ms: Option<u64>) -> Option<FailureKind> {
    if is_ignorable_close_error(err) {
        return None;
    }
    let lower = err.to_ascii_lowercase();
    if lower.contains("i/o timeout")
        || lower.contains("timed out")
        || lower.contains("deadline exceeded")
    {
        return Some(FailureKind::Timeout);
    }
    if lower.contains("tls")
        || lower.contains("x509")
        || lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("reality")
    {
        return Some(FailureKind::TlsError);
    }
    if lower.contains("reset by peer")
        || lower.contains("broken pipe")
        || lower.contains("forcibly closed")
        || lower.contains("connection aborted")
    {
        return Some(FailureKind::Reset);
    }
    if lower.contains("connection refused")
        || lower.contains("no route to host")
        || lower.contains("network is unreachable")
    {
        return Some(FailureKind::Unreachable);
    }
    if (lower.contains("unexpected eof") || lower.ends_with(": eof") || lower == "eof")
        && elapsed_ms.is_some_and(|ms| ms <= 5_000)
    {
        return Some(FailureKind::Reset);
    }
    None
}

fn human_failure_summary(kind: FailureKind, raw_err: &str) -> String {
    let lower = raw_err.to_ascii_lowercase();
    match kind {
        FailureKind::Timeout => "连接超时，目标未在限定时间内响应".into(),
        FailureKind::Reset => {
            if lower.contains("eof") {
                "握手阶段连接意外中断（EOF）".into()
            } else {
                "连接被对端或中间网络重置（RST）".into()
            }
        }
        FailureKind::Unreachable => {
            if lower.contains("refused") {
                "目标拒绝连接（Connection refused）".into()
            } else if lower.contains("unreachable") || lower.contains("no route") {
                "目标网络不可达".into()
            } else {
                "无法建立连接".into()
            }
        }
        FailureKind::DnsFailed => {
            if lower.contains("name error") || lower.contains("no such host") {
                "域名解析失败（无对应记录）".into()
            } else if lower.contains("timeout") {
                "DNS 解析超时".into()
            } else {
                "DNS 解析失败".into()
            }
        }
        FailureKind::TlsError => "TLS 握手或证书校验失败".into(),
        FailureKind::Stalled => "连接建立后长时间无下行返回".into(),
        FailureKind::Rejected => "被当前路由或拦截规则拒绝".into(),
        FailureKind::Other => "连接建立或传输失败".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, Protocol};

    fn demo_model() -> AppModel {
        let mut model = AppModel::default();
        let node = Node::new("HK-01", Protocol::Vless, "edge.example", 443);
        model.add_nodes(vec![node]);
        model
    }

    fn clash_tcp(
        id: &str,
        host: &str,
        ip: &str,
        up: u64,
        down: u64,
        chain: &str,
    ) -> ClashConnection {
        let mut c = ClashConnection::default();
        c.id = id.into();
        c.start = "2026-11-21T10:00:00Z".into();
        c.metadata.network = "tcp".into();
        c.metadata.kind = "mixed".into();
        c.metadata.host = host.into();
        c.metadata.destination_ip = ip.into();
        c.metadata.destination_port = "443".into();
        c.metadata.process = "Safari".into();
        c.upload = up;
        c.download = down;
        c.chains = vec![chain.into()];
        c
    }

    #[test]
    fn singbox_correlates_context_dial_timeout_and_deduplicates_retries() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();

        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:00 INFO [1001 0ms] inbound/mixed[mixed-in]: inbound connection from 127.0.0.1:54321",
            &model,
            100,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:00 INFO [1001 0ms] inbound/mixed[mixed-in]: inbound connection to 203.0.113.18:443",
            &model,
            100,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:00 INFO [1001 1ms] router: sniffed protocol: tls, domain: api.docs.example.org",
            &model,
            100,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:00 INFO [1001 1ms] router: found process path: /Applications/Safari.app/Contents/MacOS/Safari",
            &model,
            100,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:10 ERROR [1001 10.0s] connection: open connection to 203.0.113.18:443 using outbound/direct[direct]: dial tcp 203.0.113.18:443: i/o timeout",
            &model,
            110,
        );
        // Second retry to the same domain via direct aggregates into count=2.
        tracker.observe_log_line(
            CoreKind::SingBox,
            "+0800 2026-11-21 17:00:12 ERROR [1002 180ms] connection: open connection to api.docs.example.org:443 using outbound/direct[direct]: read tcp: connection reset by peer",
            &model,
            112,
        );

        let view = tracker.snapshot(112);
        assert_eq!(view.records.len(), 1);
        let rec = &view.records[0];
        assert_eq!(rec.domain, "api.docs.example.org");
        assert_eq!(rec.root_domain.as_deref(), Some("example.org"));
        assert_eq!(rec.destination_ip, "203.0.113.18");
        assert_eq!(rec.process_name, "Safari");
        assert_eq!(rec.outbound_direction, OutboundDirection::Direct);
        assert_eq!(rec.failure_kind, FailureKind::Reset);
        assert_eq!(rec.count, 2);
        assert_eq!(view.unfixed_count, 1);
    }

    #[test]
    fn singbox_captures_rejected_and_filters_normal_close_and_aaaa_noise() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();

        // Successful A lookup suppresses follow-up AAAA failure.
        tracker.observe_log_line(
            CoreKind::SingBox,
            "INFO [2001 4ms] dns: exchanged ok.example.com. IN A 203.0.113.5",
            &model,
            200,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [2002 8ms] dns: exchange failed for ok.example.com. IN AAAA: name error",
            &model,
            201,
        );
        // Context canceled & late idle EOF are ignored.
        tracker.observe_log_line(
            CoreKind::SingBox,
            "INFO [2003 0ms] inbound/mixed[mixed-in]: inbound connection to cancel.example.com:443",
            &model,
            201,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [2003 200ms] connection: connection download closed: context canceled",
            &model,
            201,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [2003 45.0s] connection: connection download closed: EOF",
            &model,
            245,
        );
        // Rule rejection IS captured.
        tracker.observe_log_line(
            CoreKind::SingBox,
            "INFO [2004 0ms] inbound/tun[tun-in]: inbound connection to tracker.example.net:443",
            &model,
            246,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "DEBUG [2004 1ms] router: match[3] rule_set=geosite-ads => reject",
            &model,
            246,
        );
        // Real A DNS failure IS captured.
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [2005 5.0s] dns: exchange failed for broken.example.io. IN A: i/o timeout",
            &model,
            247,
        );

        let view = tracker.snapshot(247);
        assert_eq!(view.records.len(), 2);
        assert_eq!(view.records[0].domain, "broken.example.io");
        assert_eq!(view.records[0].failure_kind, FailureKind::DnsFailed);
        assert_eq!(view.records[1].domain, "tracker.example.net");
        assert_eq!(view.records[1].failure_kind, FailureKind::Rejected);
        assert_eq!(
            view.records[1].outbound_direction,
            OutboundDirection::Reject
        );
    }

    #[test]
    fn stalled_flow_appears_at_threshold_and_clears_on_recovery_or_upgrades_on_drop() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();

        let c_recover = clash_tcp("a", "slow.example.com", "203.0.113.10", 512, 0, "direct");
        let c_stuck = clash_tcp("b", "dead.example.com", "203.0.113.11", 512, 0, "direct");

        tracker.observe_clash_connections(&[c_recover.clone(), c_stuck.clone()], &model, 100);
        assert!(tracker.snapshot(100).records.is_empty());

        // Still zero download after 4 seconds -> both marked stalled.
        tracker.observe_clash_connections(&[c_recover.clone(), c_stuck.clone()], &model, 104);
        let snap = tracker.snapshot(104);
        assert_eq!(snap.records.len(), 2);
        assert!(snap.unfixed_only_stalled);
        assert!(snap.records.iter().all(|r| r.is_stalled_live));

        // `slow.example.com` receives data -> removed cleanly; `dead.example.com` disappears -> upgraded.
        let mut c_recovered = c_recover;
        c_recovered.download = 4096;
        tracker.observe_clash_connections(&[c_recovered], &model, 109);
        let snap = tracker.snapshot(109);
        assert_eq!(snap.records.len(), 1);
        assert_eq!(snap.records[0].domain, "dead.example.com");
        assert!(!snap.records[0].is_stalled_live);
        assert_eq!(snap.records[0].failure_kind, FailureKind::Timeout);
        assert!(snap.records[0].raw_detail.contains("TCP closed after"));
        assert!(snap.records[0].reason_summary.contains("连接关闭"));
    }

    #[test]
    fn test_target_prefers_https_and_preserves_original_ip_with_sni() {
        let mut rec = FailedConnectionRecord {
            domain: "www.example.com".into(),
            domain_is_sniffed: true,
            destination_ip: "2001:db8::42".into(),
            destination_port: "2084".into(),
            network: "tcp".into(),
            inbound: "tun/tun-in".into(),
            failure_kind: FailureKind::Timeout,
            ..Default::default()
        };
        assert_eq!(
            rec.test_target().as_deref(),
            Some("tls://[2001:db8::42]:2084?sni=www.example.com")
        );
        assert_eq!(
            rec.domain_test_target().as_deref(),
            Some("tls://www.example.com:2084")
        );
        assert_eq!(
            rec.ip_test_target().as_deref(),
            Some("tls://[2001:db8::42]:2084?sni=www.example.com")
        );

        rec.destination_port = "443".into();
        assert_eq!(
            rec.test_target().as_deref(),
            Some("https://www.example.com")
        );
        assert_eq!(
            rec.ip_test_target().as_deref(),
            Some("tls://[2001:db8::42]:443?sni=www.example.com")
        );

        // FakeIP destination is never returned as an original-IP replay target.
        rec.destination_ip = "198.18.0.15".into();
        assert!(rec.ip_test_target().is_none());
    }

    #[test]
    fn xray_error_logs_extract_failed_targets() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();

        tracker.observe_log_line(
            CoreKind::Xray,
            "2026/11/21 17:00:00.123 [Warning] [42] app/proxyman/outbound: failed to process outbound traffic > proxy/freedom: failed to open connection to tcp:blocked.example.org:443 > dial tcp 203.0.113.20:443: i/o timeout",
            &model,
            300,
        );
        let view = tracker.snapshot(300);
        assert_eq!(view.records.len(), 1);
        assert_eq!(view.records[0].domain, "blocked.example.org");
        assert_eq!(
            view.records[0].outbound_direction,
            OutboundDirection::Direct
        );
        assert_eq!(view.records[0].failure_kind, FailureKind::Timeout);
    }

    #[test]
    fn batch_rule_dedup_and_coverage_marks_sibling_subdomains() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [1 5.0s] connection: open connection to api.example.org:443 using outbound/direct[direct]: dial tcp: i/o timeout",
            &model,
            400,
        );
        tracker.observe_log_line(
            CoreKind::SingBox,
            "ERROR [2 5.0s] connection: open connection to cdn.example.org:443 using outbound/direct[direct]: dial tcp: i/o timeout",
            &model,
            401,
        );
        let snap = tracker.snapshot(401);
        assert_eq!(snap.records.len(), 2);
        assert_eq!(
            count_unique_batch_rules(
                snap.records.iter(),
                DomainMatchGranularity::RootDomainSuffix
            ),
            1
        );
        assert_eq!(
            count_unique_batch_rules(snap.records.iter(), DomainMatchGranularity::Exact),
            2
        );
        let (expr, matchers) = snap.records[0]
            .build_matchers(DomainMatchGranularity::RootDomainSuffix)
            .unwrap();
        assert_eq!(expr, "*.example.org");
        assert_eq!(matchers.domain_suffixes, ["example.org"]);

        let marked = tracker.mark_rules_applied(
            &[snap.records[0].id],
            &[(
                matchers,
                AppliedRuleSummary {
                    expression: expr,
                    action: RouteAction::proxy(),
                    outbound_label: "默认出站组".into(),
                },
            )],
        );
        assert_eq!(marked, 2);
        assert_eq!(tracker.snapshot(402).unfixed_count, 0);
    }

    #[test]
    fn epoch_change_preserves_recent_records_but_clears_live_stalled_flag() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();
        let c = clash_tcp("a", "hang.example.com", "203.0.113.15", 200, 0, "direct");
        tracker.observe_clash_connections(std::slice::from_ref(&c), &model, 500);
        tracker.observe_clash_connections(&[c], &model, 505);
        assert!(tracker.snapshot(505).records[0].is_stalled_live);

        tracker.on_core_epoch_changed();
        let snap = tracker.snapshot(506);
        assert_eq!(snap.records.len(), 1);
        assert!(!snap.records[0].is_stalled_live);
    }

    #[test]
    fn singbox_found_process_strips_user_suffix_and_filters_xrose_app() {
        let model = demo_model();
        let mut tracker = FailedConnectionTracker::new();
        for line in [
            "INFO[0010] [991 0ms] inbound/tun[tun-in]: inbound connection to 203.0.113.50:11010",
            "DEBUG[0010] [991 1ms] router: found process: xrose-app, user: root",
            "INFO[0010] [991 2ms] outbound/direct[direct]: outbound connection to 203.0.113.50:11010",
            "ERROR[0011] [991 800ms] inbound/tun[tun-in]: process connection from 198.18.0.1:50000: dial tcp 203.0.113.50:11010: connect: connection refused",
        ] {
            tracker.observe_log_line(CoreKind::SingBox, line, &model, 600);
        }
        assert!(tracker.snapshot(600).records.is_empty());

        for line in [
            "INFO[0012] [992 0ms] inbound/tun[tun-in]: inbound connection to api.example.net:443",
            "DEBUG[0012] [992 1ms] router: found process: /Applications/Browser.app/Contents/MacOS/Browser, user: demo",
            "INFO[0012] [992 2ms] outbound/direct[direct]: outbound connection to api.example.net:443",
            "ERROR[0014] [992 2s] inbound/tun[tun-in]: process connection from 198.18.0.1:50001: dial tcp 203.0.113.51:443: i/o timeout",
        ] {
            tracker.observe_log_line(CoreKind::SingBox, line, &model, 605);
        }
        let snap = tracker.snapshot(605);
        assert_eq!(snap.records.len(), 1);
        assert_eq!(snap.records[0].process_name, "Browser");
    }
}
