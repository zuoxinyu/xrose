//! Five-dimension node health scoring (`A / L / S / T / R`), attribution
//! filtering, dual-window EWMA, and 24-hour peak degradation analysis.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::HealthProfile;
use crate::runtime::ClashConnection;

const MAX_SAMPLES_PER_TARGET: usize = 50;
const MAX_PASSIVE_EVENTS: usize = 400;
const MAX_HOURLY_HOURS: i64 = 168; // 7 days
const SHORT_HALF_LIFE_SECS: f64 = 15.0 * 60.0;
const LONG_HALF_LIFE_SECS: f64 = 24.0 * 3600.0;
const REACHABILITY_MAX_AGE_SECS: i64 = 24 * 3600;
const THROUGHPUT_MAX_AGE_SECS: i64 = 24 * 3600;
const PRIOR_SCORE: f32 = 75.0;
const PRIOR_SAMPLES: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthDim {
    Availability,
    Latency,
    Stability,
    Throughput,
    Reachability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthGrade {
    A,
    B,
    C,
    D,
    F,
}

impl HealthGrade {
    pub fn from_score(score: f32) -> Self {
        if score >= 85.0 {
            Self::A
        } else if score >= 70.0 {
            Self::B
        } else if score >= 50.0 {
            Self::C
        } else if score >= 30.0 {
            Self::D
        } else {
            Self::F
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::F => "F",
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::A => "优秀",
            Self::B => "良好",
            Self::C => "一般",
            Self::D => "较差",
            Self::F => "不可用",
        }
    }

    pub const fn label_en(self) -> &'static str {
        match self {
            Self::A => "Excellent",
            Self::B => "Good",
            Self::C => "Fair",
            Self::D => "Poor",
            Self::F => "Unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthGate {
    Down,
    SecurityError,
    Unstable,
    ServiceRestricted,
}

impl HealthGate {
    pub const fn cap(self) -> f32 {
        match self {
            Self::Down => 10.0,
            Self::SecurityError => 20.0,
            Self::Unstable => 30.0,
            Self::ServiceRestricted => 40.0,
        }
    }

    pub const fn label_zh(self) -> &'static str {
        match self {
            Self::Down => "不可用",
            Self::SecurityError => "安全异常",
            Self::Unstable => "严重不稳定",
            Self::ServiceRestricted => "服务受限",
        }
    }

    pub const fn label_en(self) -> &'static str {
        match self {
            Self::Down => "Unavailable",
            Self::SecurityError => "Security issue",
            Self::Unstable => "Unstable",
            Self::ServiceRestricted => "Restricted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Http,
    Download,
    Tcp,
    UdpStun,
    Reachability,
    MeshPeer,
}

impl ProbeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Download => "download",
            Self::Tcp => "tcp",
            Self::UdpStun => "udp_stun",
            Self::Reachability => "reachability",
            Self::MeshPeer => "mesh_peer",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "http" => Self::Http,
            "download" => Self::Download,
            "tcp" => Self::Tcp,
            "udp_stun" => Self::UdpStun,
            "reachability" => Self::Reachability,
            "mesh_peer" => Self::MeshPeer,
            _ => Self::Http,
        }
    }

    pub const fn is_end_to_end(self) -> bool {
        matches!(
            self,
            Self::Http | Self::Download | Self::Reachability | Self::MeshPeer
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrStage {
    #[default]
    None,
    Dns,
    Tcp,
    Tls,
    Reality,
    HttpStatus,
    Timeout,
    Unknown,
}

impl ErrStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dns => "dns",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::Reality => "reality",
            Self::HttpStatus => "http_status",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "dns" => Self::Dns,
            "tcp" => Self::Tcp,
            "tls" => Self::Tls,
            "reality" => Self::Reality,
            "http_status" => Self::HttpStatus,
            "timeout" => Self::Timeout,
            _ => Self::Unknown,
        }
    }
}

pub fn classify_error(message: &str) -> ErrStage {
    let lower = message.to_ascii_lowercase();
    if lower.contains("reality") {
        ErrStage::Reality
    } else if lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("x509")
        || lower.contains("certificate")
        || lower.contains("handshake")
        || message.contains("证书")
    {
        ErrStage::Tls
    } else if lower.contains("dns") || lower.contains("resolve") || message.contains("解析") {
        ErrStage::Dns
    } else if lower.contains("timeout") || lower.contains("timed out") || message.contains("超时")
    {
        ErrStage::Timeout
    } else if lower.contains("http ") || lower.contains("status") || message.contains("返回 http")
    {
        ErrStage::HttpStatus
    } else if lower.contains("connect")
        || lower.contains("refused")
        || lower.contains("reset")
        || message.contains("连接")
        || message.contains("中断")
    {
        ErrStage::Tcp
    } else {
        ErrStage::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeSampleRecord {
    pub target_key: String,
    pub kind: ProbeKind,
    pub ts: i64,
    pub ok: bool,
    pub rtt_ms: Option<u32>,
    pub bps: Option<u64>,
    pub bytes_used: u64,
    pub err_stage: ErrStage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HourlyBucket {
    pub target_key: String,
    pub hour_bucket: i64,
    pub ok_weighted: f64,
    pub total_weighted: f64,
    pub rtt_p50_ms: Option<u32>,
    pub rtt_p90_ms: Option<u32>,
    pub tput_p75_bps: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityCheckStatus {
    Pass,
    Limited,
    Blocked,
    #[default]
    Untested,
}

impl ReachabilityCheckStatus {
    fn points(self, limited_points: f32) -> Option<f32> {
        match self {
            Self::Pass => Some(100.0),
            Self::Limited => Some(limited_points),
            Self::Blocked => Some(0.0),
            Self::Untested => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceCheckResult {
    pub status: ReachabilityCheckStatus,
    pub region: Option<String>,
    pub detail: Option<String>,
}

impl ServiceCheckResult {
    pub fn new(status: ReachabilityCheckStatus) -> Self {
        Self {
            status,
            region: None,
            detail: None,
        }
    }

    pub fn with_region(mut self, region: Option<String>) -> Self {
        self.region = region.filter(|r| !r.trim().is_empty());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let d = detail.into();
        if !d.trim().is_empty() {
            self.detail = Some(d);
        }
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReachabilityReport {
    pub checked_at: i64,
    pub egress_region: Option<String>,
    pub chatgpt: ServiceCheckResult,
    pub claude: ServiceCheckResult,
    pub gemini: ServiceCheckResult,
    pub copilot: ServiceCheckResult,
    pub netflix: ServiceCheckResult,
    pub youtube: ServiceCheckResult,
    pub disney: ServiceCheckResult,
    pub tiktok: ServiceCheckResult,
    pub web_clean: ServiceCheckResult,
    pub scholar: ServiceCheckResult,
    pub dev_hub: ServiceCheckResult,
    pub udp_stun: ServiceCheckResult,
}

impl ReachabilityReport {
    pub fn ai_score(&self) -> Option<f32> {
        weighted_checks(&[
            (0.35, self.chatgpt.status.points(40.0)),
            (0.30, self.claude.status.points(40.0)),
            (0.25, self.gemini.status.points(40.0)),
            (0.10, self.copilot.status.points(40.0)),
        ])
    }

    pub fn media_score(&self) -> Option<f32> {
        weighted_checks(&[
            (0.40, self.netflix.status.points(50.0)),
            (0.25, self.youtube.status.points(40.0)),
            (0.20, self.disney.status.points(40.0)),
            (0.15, self.tiktok.status.points(40.0)),
        ])
    }

    pub fn general_score(&self) -> Option<f32> {
        weighted_checks(&[
            (0.35, self.web_clean.status.points(50.0)),
            (0.20, self.scholar.status.points(50.0)),
            (0.20, self.dev_hub.status.points(50.0)),
            (0.25, self.udp_stun.status.points(75.0)),
        ])
    }

    pub fn score_for_profile(&self, profile: HealthProfile) -> Option<f32> {
        match profile {
            HealthProfile::General => weighted_checks(&[
                (0.40, self.ai_score()),
                (0.35, self.media_score()),
                (0.25, self.general_score()),
            ]),
            HealthProfile::Ai => self.ai_score(),
            HealthProfile::Streaming => self.media_score(),
            HealthProfile::LowLatency => weighted_checks(&[
                (0.40, self.general_score()),
                (0.60, self.udp_stun.status.points(75.0)),
            ]),
            HealthProfile::Bulk => self.general_score(),
            HealthProfile::MeshExit => None,
        }
    }

    pub fn ai_counts(&self) -> Option<(usize, usize)> {
        let items = [
            self.chatgpt.status,
            self.claude.status,
            self.gemini.status,
            self.copilot.status,
        ];
        let tested = items
            .iter()
            .filter(|s| **s != ReachabilityCheckStatus::Untested)
            .count();
        (tested > 0).then(|| {
            let passed = items
                .iter()
                .filter(|s| **s == ReachabilityCheckStatus::Pass)
                .count();
            (passed, tested)
        })
    }

    pub fn masked_for_privacy(&self) -> Self {
        let mut copy = self.clone();
        copy.egress_region = None;
        for item in [
            &mut copy.chatgpt,
            &mut copy.claude,
            &mut copy.gemini,
            &mut copy.copilot,
            &mut copy.netflix,
            &mut copy.youtube,
            &mut copy.disney,
            &mut copy.tiktok,
            &mut copy.web_clean,
            &mut copy.scholar,
            &mut copy.dev_hub,
            &mut copy.udp_stun,
        ] {
            item.region = None;
        }
        copy
    }
}

fn weighted_checks(items: &[(f32, Option<f32>)]) -> Option<f32> {
    let mut sum_w = 0.0f32;
    let mut sum_v = 0.0f32;
    for &(w, v) in items {
        if let Some(score) = v {
            sum_w += w;
            sum_v += w * score;
        }
    }
    (sum_w > 0.0).then(|| (sum_v / sum_w).clamp(0.0, 100.0))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DimBreakdown {
    pub dim: HealthDim,
    pub score: Option<f32>,
    pub effective_samples: f32,
    pub p50_ms: Option<u32>,
    pub p90_ms: Option<u32>,
    pub jitter_ms: Option<u32>,
    pub loss_rate: Option<f32>,
    pub drop_rate: Option<f32>,
    pub success_rate: Option<f32>,
    pub throughput_bps: Option<u64>,
    pub passive_throughput: bool,
    pub active_samples: u32,
    pub passive_samples: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HealthDeduction {
    pub dim: HealthDim,
    pub points_lost: f32,
    pub code: String,
    pub zh: String,
    pub en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeakDegradation {
    pub start_hour: u8,
    pub end_hour: u8,
    pub baseline_p50_ms: u32,
    pub peak_p50_ms: u32,
    pub increase_pct: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HourlySummarySlot {
    pub hour: u8,
    pub avg_p50_ms: Option<u32>,
    pub success_rate: Option<f32>,
    pub sample_days: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileEvaluation {
    pub profile: HealthProfile,
    pub score: f32,
    pub short_score: f32,
    pub long_score: f32,
    pub grade: HealthGrade,
    pub gate: Option<HealthGate>,
    pub confidence: f32,
    pub dims: Vec<DimBreakdown>,
    pub deductions: Vec<HealthDeduction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeHealthView {
    pub target_key: String,
    pub general: Option<ProfileEvaluation>,
    pub ai_score: Option<f32>,
    pub media_score: Option<f32>,
    pub reachability: Option<ReachabilityReport>,
    pub peak_degradation: Option<PeakDegradation>,
    pub hourly_profile: Vec<HourlySummarySlot>,
    pub updated_at: i64,
    pub last_active_at: i64,
    pub is_mesh_exit: bool,
}

impl NodeHealthView {
    pub fn masked_for_privacy(&self) -> Self {
        let mut copy = self.clone();
        copy.reachability = copy.reachability.map(|r| r.masked_for_privacy());
        copy
    }
}

#[derive(Debug, Clone)]
struct PassiveConnEvent {
    ts: i64,
    ok: bool,
    abnormal_drop: bool,
    peak_bps: Option<u64>,
    throughput_only: bool,
}

#[derive(Debug, Clone, Default)]
struct HourlyAccumulator {
    hour_bucket: i64,
    ok_weighted: f64,
    total_weighted: f64,
    rtts: Vec<u32>,
    stored_p50: Option<u32>,
    stored_p90: Option<u32>,
    tputs: Vec<u64>,
    stored_tput_p75: Option<u64>,
}

impl HourlyAccumulator {
    fn from_bucket(bucket: HourlyBucket) -> Self {
        Self {
            hour_bucket: bucket.hour_bucket,
            ok_weighted: bucket.ok_weighted,
            total_weighted: bucket.total_weighted,
            rtts: Vec::new(),
            stored_p50: bucket.rtt_p50_ms,
            stored_p90: bucket.rtt_p90_ms,
            tputs: Vec::new(),
            stored_tput_p75: bucket.tput_p75_bps,
        }
    }

    fn push_probe(&mut self, ok: bool, rtt_ms: Option<u32>, bps: Option<u64>) {
        self.total_weighted += 1.0;
        if ok {
            self.ok_weighted += 1.0;
        }
        if let Some(rtt) = rtt_ms.filter(|&r| r > 0) {
            self.rtts.push(rtt);
        }
        if let Some(rate) = bps.filter(|&b| b > 0) {
            self.tputs.push(rate);
        }
    }

    fn push_passive_throughput(&mut self, bps: u64) {
        if bps > 0 {
            self.tputs.push(bps);
        }
    }

    fn p50_ms(&self) -> Option<u32> {
        if !self.rtts.is_empty() {
            percentile_u32(&self.rtts, 50)
        } else {
            self.stored_p50
        }
    }

    fn p90_ms(&self) -> Option<u32> {
        if !self.rtts.is_empty() {
            percentile_u32(&self.rtts, 90)
        } else {
            self.stored_p90
        }
    }

    fn tput_p75(&self) -> Option<u64> {
        if !self.tputs.is_empty() {
            percentile_u64(&self.tputs, 75)
        } else {
            self.stored_tput_p75
        }
    }

    fn to_bucket(&self, target_key: &str) -> HourlyBucket {
        HourlyBucket {
            target_key: target_key.to_owned(),
            hour_bucket: self.hour_bucket,
            ok_weighted: self.ok_weighted,
            total_weighted: self.total_weighted,
            rtt_p50_ms: self.p50_ms(),
            rtt_p90_ms: self.p90_ms(),
            tput_p75_bps: self.tput_p75(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TargetHealthState {
    samples: VecDeque<ProbeSampleRecord>,
    passive: VecDeque<PassiveConnEvent>,
    hourly: BTreeMap<i64, HourlyAccumulator>,
    reachability: Option<ReachabilityReport>,
    peer_loss: Option<(i64, f32)>,
    last_active_at: i64,
    updated_at: i64,
    is_mesh_exit: bool,
}

#[derive(Debug, Clone)]
struct TrackedFlow {
    target_key: String,
    target_hash: u64,
    is_tcp: bool,
    started_ts: i64,
    last_seen_ts: i64,
    last_upload: u64,
    last_download: u64,
    max_speed_bps: u64,
    ever_received: bool,
    counted_ok: bool,
}

#[derive(Debug)]
struct AttributionWindow {
    salt: u64,
    recent_active: VecDeque<(i64, String, bool, ErrStage)>,
    target_failures: VecDeque<(i64, u64, String)>,
    host_offline_until: i64,
}

impl AttributionWindow {
    fn new() -> Self {
        Self {
            salt: (uuid::Uuid::new_v4().as_u128() & u128::from(u64::MAX)) as u64,
            recent_active: VecDeque::new(),
            target_failures: VecDeque::new(),
            host_offline_until: 0,
        }
    }

    fn hash_target(&self, host: &str) -> u64 {
        let mut hasher = Sha256::new();
        hasher.update(self.salt.to_le_bytes());
        hasher.update(host.trim().to_ascii_lowercase().as_bytes());
        let digest = hasher.finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"))
    }

    fn record_active_probe(
        &mut self,
        target_key: &str,
        ok: bool,
        err_stage: ErrStage,
        now: i64,
    ) -> bool {
        if ok {
            self.host_offline_until = 0;
            self.recent_active
                .push_back((now, target_key.to_owned(), true, ErrStage::None));
            self.prune(now);
            return false;
        }
        self.recent_active
            .push_back((now, target_key.to_owned(), false, err_stage));
        self.prune(now);

        let window_start = now - 15;
        let mut distinct_net_failures = BTreeSet::new();
        for (ts, k, ok, stage) in self.recent_active.iter().rev() {
            if *ts < window_start || *ok {
                break;
            }
            if matches!(stage, ErrStage::Tcp | ErrStage::Timeout | ErrStage::Dns) {
                distinct_net_failures.insert(k.as_str());
            }
        }
        if distinct_net_failures.len() >= 3 {
            self.host_offline_until = now + 30;
        }
        now < self.host_offline_until
    }

    fn is_host_offline(&self, now: i64) -> bool {
        now < self.host_offline_until
    }

    fn record_passive_failure(&mut self, target_hash: u64, target_key: &str, now: i64) {
        self.target_failures
            .push_back((now, target_hash, target_key.to_owned()));
        self.prune(now);
    }

    fn is_cross_node_target_fault(&self, target_hash: u64, now: i64) -> bool {
        let cutoff = now - 300;
        let targets: BTreeSet<&str> = self
            .target_failures
            .iter()
            .filter(|(ts, hash, _)| *ts >= cutoff && *hash == target_hash)
            .map(|(_, _, key)| key.as_str())
            .collect();
        targets.len() >= 2
    }

    fn prune(&mut self, now: i64) {
        while self
            .recent_active
            .front()
            .is_some_and(|(ts, _, _, _)| now.saturating_sub(*ts) > 60)
        {
            self.recent_active.pop_front();
        }
        while self
            .target_failures
            .front()
            .is_some_and(|(ts, _, _)| now.saturating_sub(*ts) > 300)
        {
            self.target_failures.pop_front();
        }
        while self.target_failures.len() > 512 {
            self.target_failures.pop_front();
        }
    }
}

pub struct HealthEngine {
    targets: HashMap<String, TargetHealthState>,
    active_flows: BTreeMap<(String, String), TrackedFlow>,
    attribution: AttributionWindow,
    dirty_buckets: BTreeSet<(String, i64)>,
}

impl Default for HealthEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthEngine {
    pub fn new() -> Self {
        Self {
            targets: HashMap::new(),
            active_flows: BTreeMap::new(),
            attribution: AttributionWindow::new(),
            dirty_buckets: BTreeSet::new(),
        }
    }

    pub fn load_persisted(
        &mut self,
        samples: Vec<ProbeSampleRecord>,
        buckets: Vec<HourlyBucket>,
        reachability: Vec<(String, ReachabilityReport)>,
        now: i64,
    ) {
        self.targets.clear();
        let min_bucket = hour_floor(now) - MAX_HOURLY_HOURS * 3600;
        for sample in samples {
            let entry = self.targets.entry(sample.target_key.clone()).or_default();
            entry.is_mesh_exit |= sample.target_key.starts_with("mesh-http:")
                || sample.target_key.starts_with("mesh-exit-");
            entry.updated_at = entry.updated_at.max(sample.ts);
            entry.samples.push_back(sample);
            while entry.samples.len() > MAX_SAMPLES_PER_TARGET {
                entry.samples.pop_front();
            }
        }
        for bucket in buckets {
            if bucket.hour_bucket < min_bucket {
                continue;
            }
            let entry = self.targets.entry(bucket.target_key.clone()).or_default();
            entry
                .hourly
                .insert(bucket.hour_bucket, HourlyAccumulator::from_bucket(bucket));
        }
        for (key, report) in reachability {
            let entry = self.targets.entry(key).or_default();
            entry.updated_at = entry.updated_at.max(report.checked_at);
            entry.reachability = Some(report);
        }
    }

    /// Records an active probe result. Returns `true` if the sample was stored
    /// (not suppressed by host-offline attribution guard).
    pub fn record_probe(&mut self, sample: ProbeSampleRecord) -> bool {
        let suppressed = self.attribution.record_active_probe(
            &sample.target_key,
            sample.ok,
            sample.err_stage,
            sample.ts,
        );
        if suppressed && !sample.ok {
            return false;
        }
        let key = sample.target_key.clone();
        let ts = sample.ts;
        let bucket_ts = hour_floor(ts);
        let entry = self.targets.entry(key.clone()).or_default();
        entry.is_mesh_exit |= key.starts_with("mesh-http:")
            || key.starts_with("mesh-exit-")
            || sample.kind == ProbeKind::MeshPeer;
        entry.updated_at = entry.updated_at.max(ts);
        if sample.ok {
            entry.last_active_at = entry.last_active_at.max(ts);
        }
        if sample.kind.is_end_to_end() {
            let acc = entry
                .hourly
                .entry(bucket_ts)
                .or_insert_with(|| HourlyAccumulator {
                    hour_bucket: bucket_ts,
                    ..Default::default()
                });
            acc.push_probe(sample.ok, sample.rtt_ms, sample.bps);
            self.dirty_buckets.insert((key.clone(), bucket_ts));
            prune_hourly(&mut entry.hourly, ts);
        }
        entry.samples.push_back(sample);
        while entry.samples.len() > MAX_SAMPLES_PER_TARGET {
            entry.samples.pop_front();
        }
        true
    }

    pub fn set_reachability(&mut self, target_key: &str, report: ReachabilityReport) {
        let ts = report.checked_at;
        let entry = self.targets.entry(target_key.to_owned()).or_default();
        entry.is_mesh_exit |=
            target_key.starts_with("mesh-http:") || target_key.starts_with("mesh-exit-");
        entry.updated_at = entry.updated_at.max(ts);
        entry.reachability = Some(report);
    }

    pub fn set_mesh_peer_loss(&mut self, target_key: &str, loss_rate: f32, now: i64) {
        let entry = self.targets.entry(target_key.to_owned()).or_default();
        entry.is_mesh_exit = true;
        entry.peer_loss = Some((now, loss_rate.clamp(0.0, 1.0)));
    }

    pub fn observe_passive_singbox(
        &mut self,
        connections: &[ClashConnection],
        speeds: &[(u64, u64)],
        resolver: impl Fn(&[String]) -> Option<String>,
        now: i64,
    ) -> BTreeSet<String> {
        let mut changed_targets = BTreeSet::new();
        let mut current_keys = BTreeSet::new();

        for (index, conn) in connections.iter().enumerate() {
            let Some(target_key) = resolver(&conn.chains) else {
                continue;
            };
            let flow_key = (conn.id.clone(), conn.start.clone());
            current_keys.insert(flow_key.clone());
            let down_speed = speeds.get(index).map(|s| s.1).unwrap_or(0);
            if down_speed >= 1024 {
                let entry = self.targets.entry(target_key.clone()).or_default();
                let prev_active = entry.last_active_at;
                entry.last_active_at = entry.last_active_at.max(now);
                if prev_active == 0 || now.saturating_sub(prev_active) >= 15 {
                    changed_targets.insert(target_key.clone());
                }
            }
            let target_hash = self.attribution.hash_target(&conn.host());
            let entry = self
                .active_flows
                .entry(flow_key)
                .or_insert_with(|| TrackedFlow {
                    target_key: target_key.clone(),
                    target_hash,
                    is_tcp: conn.metadata.network.eq_ignore_ascii_case("tcp"),
                    started_ts: now,
                    last_seen_ts: now,
                    last_upload: conn.upload,
                    last_download: conn.download,
                    max_speed_bps: down_speed,
                    ever_received: conn.download > 0,
                    counted_ok: false,
                });
            entry.last_seen_ts = now;
            entry.last_upload = entry.last_upload.max(conn.upload);
            entry.last_download = entry.last_download.max(conn.download);
            entry.ever_received |= conn.download > 0;
            if conn.download >= 2 * 1024 * 1024 {
                entry.max_speed_bps = entry.max_speed_bps.max(down_speed);
            }
            if !entry.counted_ok && conn.download > 0 {
                entry.counted_ok = true;
                if !self.attribution.is_host_offline(now) {
                    self.push_passive_event(
                        &target_key,
                        PassiveConnEvent {
                            ts: now,
                            ok: true,
                            abnormal_drop: false,
                            peak_bps: None,
                            throughput_only: false,
                        },
                    );
                    changed_targets.insert(target_key);
                }
            }
        }

        let finished_keys: Vec<_> = self
            .active_flows
            .keys()
            .filter(|k| !current_keys.contains(*k))
            .cloned()
            .collect();
        let finished_flows: Vec<_> = finished_keys
            .into_iter()
            .filter_map(|key| self.active_flows.remove(&key))
            .collect();

        if self.attribution.is_host_offline(now) {
            return changed_targets;
        }

        // Pass 1: record all zero-RX short TCP failures across targets first so a
        // broken destination domain does not penalize whichever flow is processed later.
        for flow in &finished_flows {
            let duration = flow.last_seen_ts.saturating_sub(flow.started_ts);
            if !(flow.ever_received && flow.last_download > 0)
                && flow.is_tcp
                && flow.last_upload > 0
                && duration <= 2
            {
                self.attribution
                    .record_passive_failure(flow.target_hash, &flow.target_key, now);
            }
        }

        // Pass 2: emit passive success/drop events with full cross-node awareness.
        for flow in finished_flows {
            let duration = flow.last_seen_ts.saturating_sub(flow.started_ts);
            if flow.counted_ok {
                if flow.last_download >= 2 * 1024 * 1024 && flow.max_speed_bps > 0 {
                    self.push_passive_event(
                        &flow.target_key,
                        PassiveConnEvent {
                            ts: now,
                            ok: true,
                            abnormal_drop: false,
                            peak_bps: Some(flow.max_speed_bps),
                            throughput_only: true,
                        },
                    );
                    changed_targets.insert(flow.target_key);
                }
            } else if flow.ever_received && flow.last_download > 0 {
                let peak_bps = (flow.last_download >= 2 * 1024 * 1024 && flow.max_speed_bps > 0)
                    .then_some(flow.max_speed_bps);
                self.push_passive_event(
                    &flow.target_key,
                    PassiveConnEvent {
                        ts: now,
                        ok: true,
                        abnormal_drop: false,
                        peak_bps,
                        throughput_only: false,
                    },
                );
                changed_targets.insert(flow.target_key);
            } else if flow.is_tcp
                && flow.last_upload > 0
                && duration <= 2
                && !self
                    .attribution
                    .is_cross_node_target_fault(flow.target_hash, now)
            {
                self.push_passive_event(
                    &flow.target_key,
                    PassiveConnEvent {
                        ts: now,
                        ok: false,
                        abnormal_drop: true,
                        peak_bps: None,
                        throughput_only: false,
                    },
                );
                changed_targets.insert(flow.target_key);
            }
        }

        changed_targets
    }

    pub fn observe_xray_active(&mut self, target_key: &str, down_speed_bps: u64, now: i64) {
        if down_speed_bps >= 1024 {
            let entry = self.targets.entry(target_key.to_owned()).or_default();
            entry.last_active_at = entry.last_active_at.max(now);
        }
    }

    fn push_passive_event(&mut self, target_key: &str, event: PassiveConnEvent) {
        let ts = event.ts;
        let ok = event.ok;
        let peak_bps = event.peak_bps;
        let bucket_ts = hour_floor(ts);
        let entry = self.targets.entry(target_key.to_owned()).or_default();
        if ok {
            entry.last_active_at = entry.last_active_at.max(ts);
        }
        entry.updated_at = entry.updated_at.max(ts);
        if let Some(rate) = peak_bps.filter(|&b| b > 0) {
            let acc = entry
                .hourly
                .entry(bucket_ts)
                .or_insert_with(|| HourlyAccumulator {
                    hour_bucket: bucket_ts,
                    ..Default::default()
                });
            acc.push_passive_throughput(rate);
            self.dirty_buckets
                .insert((target_key.to_owned(), bucket_ts));
        }
        prune_hourly(&mut entry.hourly, ts);
        entry.passive.push_back(event);
        while entry.passive.len() > MAX_PASSIVE_EVENTS {
            entry.passive.pop_front();
        }
        while entry
            .passive
            .front()
            .is_some_and(|e| ts.saturating_sub(e.ts) > THROUGHPUT_MAX_AGE_SECS)
        {
            entry.passive.pop_front();
        }
    }

    pub fn clear_active_flows(&mut self) {
        self.active_flows.clear();
    }

    pub fn remove_targets(&mut self, keys: &[String]) {
        for key in keys {
            self.targets.remove(key);
        }
        self.active_flows
            .retain(|_, flow| !keys.contains(&flow.target_key));
        self.dirty_buckets.retain(|(key, _)| !keys.contains(key));
    }

    pub fn clear_all(&mut self) {
        self.targets.clear();
        self.active_flows.clear();
        self.dirty_buckets.clear();
    }

    pub fn take_dirty_hourly_buckets(&mut self) -> Vec<HourlyBucket> {
        let dirty = std::mem::take(&mut self.dirty_buckets);
        dirty
            .into_iter()
            .filter_map(|(key, hour_bucket)| {
                self.targets
                    .get(&key)?
                    .hourly
                    .get(&hour_bucket)
                    .map(|acc| acc.to_bucket(&key))
            })
            .collect()
    }

    pub fn evaluate_for_profile(
        &self,
        target_key: &str,
        profile: HealthProfile,
        now: i64,
    ) -> Option<ProfileEvaluation> {
        let state = self.targets.get(target_key)?;
        evaluate_state(state, profile, now)
    }

    pub fn build_view(&self, target_key: &str, now: i64) -> Option<NodeHealthView> {
        let state = self.targets.get(target_key)?;
        let default_profile = if state.is_mesh_exit {
            HealthProfile::MeshExit
        } else {
            HealthProfile::General
        };
        let general = evaluate_state(state, default_profile, now);
        let reachability = state
            .reachability
            .as_ref()
            .filter(|r| now.saturating_sub(r.checked_at) <= REACHABILITY_MAX_AGE_SECS)
            .cloned()
            .or_else(|| state.reachability.clone());
        let ai_score = evaluate_state(state, HealthProfile::Ai, now).map(|e| e.score);
        let media_score = evaluate_state(state, HealthProfile::Streaming, now).map(|e| e.score);
        let hourly_profile = summarize_24h(&state.hourly);
        let peak_degradation = detect_peak_degradation(&hourly_profile);
        if general.is_none() && reachability.is_none() && peak_degradation.is_none() {
            return None;
        }
        Some(NodeHealthView {
            target_key: target_key.to_owned(),
            general,
            ai_score,
            media_score,
            reachability,
            peak_degradation,
            hourly_profile,
            updated_at: state.updated_at,
            last_active_at: state.last_active_at,
            is_mesh_exit: state.is_mesh_exit,
        })
    }

    pub fn build_all_views(&self, now: i64) -> BTreeMap<String, NodeHealthView> {
        self.targets
            .keys()
            .filter_map(|key| self.build_view(key, now).map(|v| (key.clone(), v)))
            .collect()
    }
}

fn hour_floor(ts: i64) -> i64 {
    ts.div_euclid(3600) * 3600
}

fn prune_hourly(hourly: &mut BTreeMap<i64, HourlyAccumulator>, now: i64) {
    let min_bucket = hour_floor(now) - MAX_HOURLY_HOURS * 3600;
    hourly.retain(|&ts, _| ts >= min_bucket);
}

fn decay_weight(dt_secs: i64, half_life: f64) -> f64 {
    let dt = dt_secs.max(0) as f64;
    0.5f64.powf(dt / half_life)
}

fn evaluate_state(
    state: &TargetHealthState,
    profile: HealthProfile,
    now: i64,
) -> Option<ProfileEvaluation> {
    let long_eval = compute_window(state, profile, LONG_HALF_LIFE_SECS, true, now)?;
    let short_eval = compute_window(state, profile, SHORT_HALF_LIFE_SECS, false, now)
        .unwrap_or_else(|| long_eval.clone());

    let gate = long_eval.gate.or(short_eval.gate);
    let mut display_score = 0.40 * short_eval.score + 0.60 * long_eval.score;
    if let Some(gate) = gate {
        display_score = display_score.min(gate.cap());
    }
    display_score = display_score.clamp(0.0, 100.0);

    let mut short_score = short_eval.score;
    if let Some(gate) = gate {
        short_score = short_score.min(gate.cap());
    }

    Some(ProfileEvaluation {
        profile,
        score: display_score,
        short_score: short_score.clamp(0.0, 100.0),
        long_score: long_eval.score.clamp(0.0, 100.0),
        grade: HealthGrade::from_score(display_score),
        gate,
        confidence: long_eval.confidence,
        dims: long_eval.dims,
        deductions: long_eval.deductions,
    })
}

#[derive(Clone)]
struct WindowResult {
    score: f32,
    gate: Option<HealthGate>,
    confidence: f32,
    dims: Vec<DimBreakdown>,
    deductions: Vec<HealthDeduction>,
}

fn compute_window(
    state: &TargetHealthState,
    profile: HealthProfile,
    half_life: f64,
    include_hourly: bool,
    now: i64,
) -> Option<WindowResult> {
    let e2e_samples: Vec<&ProbeSampleRecord> = state
        .samples
        .iter()
        .filter(|s| s.kind.is_end_to_end())
        .collect();
    let has_any_hourly_probes = state
        .hourly
        .values()
        .any(|acc| acc.total_weighted > 0.0 || acc.p50_ms().is_some());
    let tcp_only = e2e_samples.is_empty()
        && !has_any_hourly_probes
        && state.samples.iter().any(|s| s.kind == ProbeKind::Tcp);
    let probe_samples: Vec<&ProbeSampleRecord> = if tcp_only {
        state
            .samples
            .iter()
            .filter(|s| s.kind == ProbeKind::Tcp)
            .collect()
    } else {
        e2e_samples
    };

    let has_hourly_probes = include_hourly && has_any_hourly_probes;
    if probe_samples.is_empty() && state.reachability.is_none() && !has_hourly_probes {
        return None;
    }

    // 1. Availability (A)
    let mut probe_ok_w = 0.0f64;
    let mut probe_total_w = 0.0f64;
    for s in &probe_samples {
        let w = decay_weight(now.saturating_sub(s.ts), half_life);
        probe_total_w += w;
        if s.ok {
            probe_ok_w += w;
        }
    }
    if include_hourly && !tcp_only && probe_samples.is_empty() {
        for acc in state.hourly.values() {
            let w = decay_weight(now.saturating_sub(acc.hour_bucket), half_life);
            probe_total_w += acc.total_weighted * w;
            probe_ok_w += acc.ok_weighted * w;
        }
    }

    let mut conn_ok_w = 0.0f64;
    let mut conn_total_w = 0.0f64;
    let mut conn_drop_w = 0.0f64;
    let mut conn_count = 0u32;
    let mut passive_tputs = Vec::new();
    let mut latest_passive_tput_ts: i64 = i64::MIN;
    for ev in &state.passive {
        if !include_hourly && now.saturating_sub(ev.ts) > 3600 {
            continue;
        }
        if ev.throughput_only {
            if let Some(bps) = ev.peak_bps {
                passive_tputs.push(bps);
                latest_passive_tput_ts = latest_passive_tput_ts.max(ev.ts);
            }
            continue;
        }
        let w = decay_weight(now.saturating_sub(ev.ts), half_life);
        conn_total_w += w;
        conn_count += 1;
        if ev.ok {
            conn_ok_w += w;
        }
        if ev.abnormal_drop {
            conn_drop_w += w;
        }
        if let Some(bps) = ev.peak_bps {
            passive_tputs.push(bps);
            latest_passive_tput_ts = latest_passive_tput_ts.max(ev.ts);
        }
    }

    let p_probe = (probe_total_w > 0.0).then_some((probe_ok_w / probe_total_w) as f32);
    let p_conn =
        (conn_count >= 10 && conn_total_w > 0.0).then_some((conn_ok_w / conn_total_w) as f32);
    let fused_p = match (p_probe, p_conn) {
        (Some(pp), Some(pc)) => {
            let lambda = (conn_count as f32 / 200.0).min(0.70);
            Some(lambda * pc + (1.0 - lambda) * pp)
        }
        (Some(pp), None) => Some(pp),
        (None, Some(pc)) => {
            let lambda = (conn_count as f32 / 200.0).min(0.70);
            Some(lambda * pc + (1.0 - lambda) * 0.95)
        }
        (None, None) => None,
    };
    let active_count = probe_samples.len() as u32;
    let s_avail = fused_p.map(|p| 100.0 * ((p - 0.80) / 0.20).clamp(0.0, 1.0).powi(2));

    // 2. Latency (L)
    let latency_candidates: Vec<(i64, u32)> = probe_samples
        .iter()
        .filter(|s| {
            s.ok && matches!(
                s.kind,
                ProbeKind::Http | ProbeKind::Reachability | ProbeKind::MeshPeer | ProbeKind::Tcp
            )
        })
        .filter_map(|s| s.rtt_ms.filter(|&r| r > 0).map(|rtt| (s.ts, rtt)))
        .collect();
    let mut rtts: Vec<u32> = if !include_hourly {
        let recent: Vec<u32> = latency_candidates
            .iter()
            .filter(|(ts, _)| now.saturating_sub(*ts) <= 1800)
            .map(|(_, rtt)| *rtt)
            .collect();
        if recent.is_empty() {
            latency_candidates
                .iter()
                .rev()
                .take(3)
                .map(|(_, rtt)| *rtt)
                .collect()
        } else {
            recent
        }
    } else {
        latency_candidates.iter().map(|(_, rtt)| *rtt).collect()
    };
    if rtts.is_empty() && include_hourly {
        for acc in state.hourly.values() {
            if let Some(p50) = acc.p50_ms() {
                rtts.push(p50);
            }
        }
    }
    let (p50_ms, p90_ms, s_lat) = if !rtts.is_empty() {
        let p50 = percentile_u32(&rtts, 50).unwrap_or(0);
        let p90 = percentile_u32(&rtts, 90).unwrap_or(p50);
        let mix = (0.60 * p50 as f64 + 0.40 * p90 as f64).max(1.0);
        let (l_good, l_bad) = if state.is_mesh_exit {
            (30.0f64, 300.0f64)
        } else if tcp_only {
            (60.0f64, 400.0f64)
        } else {
            (150.0f64, 1500.0f64)
        };
        let norm = ((l_bad.ln() - mix.ln()) / (l_bad.ln() - l_good.ln())).clamp(0.0, 1.0);
        (Some(p50), Some(p90), Some((norm * 100.0) as f32))
    } else {
        (None, None, None)
    };

    // 3. Stability (S)
    let ok_rtt_candidates: Vec<(i64, u32)> = probe_samples
        .iter()
        .filter(|s| s.ok)
        .filter_map(|s| s.rtt_ms.filter(|&r| r > 0).map(|rtt| (s.ts, rtt)))
        .collect();
    let ok_rtts: Vec<u32> = if !include_hourly {
        let recent: Vec<u32> = ok_rtt_candidates
            .iter()
            .filter(|(ts, _)| now.saturating_sub(*ts) <= 1800)
            .map(|(_, rtt)| *rtt)
            .collect();
        if recent.is_empty() {
            let skip = ok_rtt_candidates.len().saturating_sub(3);
            ok_rtt_candidates[skip..]
                .iter()
                .map(|(_, rtt)| *rtt)
                .collect()
        } else {
            recent
        }
    } else {
        ok_rtt_candidates.iter().map(|(_, rtt)| *rtt).collect()
    };
    let jitter_ms = if ok_rtts.len() >= 2 {
        let mut diffs = 0.0f64;
        for w in ok_rtts.windows(2) {
            diffs += (w[1] as f64 - w[0] as f64).abs();
        }
        Some((diffs / (ok_rtts.len() - 1) as f64).round() as u32)
    } else {
        None
    };
    let s_jitter = jitter_ms.zip(p50_ms).map(|(j, p50)| {
        let denom = (p50 as f32).max(80.0);
        let ratio = j as f32 / denom;
        100.0 * (1.0 - ratio / 0.50).clamp(0.0, 1.0)
    });
    let loss_rate = state
        .peer_loss
        .filter(|(ts, _)| now.saturating_sub(*ts) <= 3600)
        .map(|(_, l)| l);
    let s_loss = loss_rate.map(|loss| 100.0 * (-loss / 0.03).exp());
    let drop_rate =
        (conn_count >= 10 && conn_total_w > 0.0).then_some((conn_drop_w / conn_total_w) as f32);
    let s_drop = drop_rate.map(|rate| 100.0 * (-rate / 0.04).exp());
    let s_stab = weighted_checks(&[(0.40, s_jitter), (0.30, s_loss), (0.30, s_drop)]);

    // 4. Throughput (T)
    let active_tputs: Vec<(i64, u64)> = state
        .samples
        .iter()
        .filter(|s| {
            s.ok && s.kind == ProbeKind::Download
                && now.saturating_sub(s.ts) <= THROUGHPUT_MAX_AGE_SECS
        })
        .filter_map(|s| s.bps.filter(|&b| b > 0).map(|b| (s.ts, b)))
        .collect();
    let latest_active_tput = active_tputs.iter().copied().max_by_key(|&(ts, _)| ts);
    let (throughput_bps, passive_throughput) = match (passive_tputs.is_empty(), latest_active_tput)
    {
        (false, Some((active_ts, active_bps))) if active_ts >= latest_passive_tput_ts => {
            (Some(active_bps), false)
        }
        (false, _) => (percentile_u64(&passive_tputs, 75), true),
        (true, Some((_, active_bps))) => (Some(active_bps), false),
        (true, None) => (None, false),
    };
    let s_tput = throughput_bps.map(|bps| {
        let low = 500_000.0f64;
        let high = 20_000_000.0f64;
        let val = (bps as f64).max(1.0);
        let norm = ((val / low).ln() / (high / low).ln()).clamp(0.0, 1.0);
        (norm * 100.0) as f32
    });

    // 5. Reachability (R)
    let fresh_reachability = state
        .reachability
        .as_ref()
        .filter(|r| now.saturating_sub(r.checked_at) <= REACHABILITY_MAX_AGE_SECS);
    let s_reach = fresh_reachability.and_then(|r| r.score_for_profile(profile));

    // Confidence & asymmetric shrinkage
    let n_eff = active_count as f32 + conn_count as f32 * 0.05;
    let mut confidence = 1.0 - (-n_eff / 5.0).exp();
    if tcp_only {
        confidence = confidence.min(0.25);
    }

    let dim_specs = [
        (
            HealthDim::Availability,
            s_avail,
            active_count as f32 + conn_count as f32 * 0.05,
        ),
        (HealthDim::Latency, s_lat, rtts.len() as f32),
        (
            HealthDim::Stability,
            s_stab,
            ok_rtts.len().saturating_sub(1) as f32 + if drop_rate.is_some() { 2.0 } else { 0.0 },
        ),
        (
            HealthDim::Throughput,
            s_tput,
            if passive_throughput {
                passive_tputs.len() as f32
            } else {
                active_tputs.len() as f32
            },
        ),
        (
            HealthDim::Reachability,
            s_reach,
            if s_reach.is_some() { 2.0 } else { 0.0 },
        ),
    ];

    let weights = profile_weights(profile);
    let mut weight_sum = 0.0f32;
    let mut log_sum = 0.0f64;
    let mut dims = Vec::with_capacity(5);
    let mut deductions = Vec::new();

    for ((dim, raw_score, dim_n), weight) in dim_specs.into_iter().zip(weights) {
        let shrunk = raw_score.map(|s| shrink_high_only(s, dim_n));
        if let Some(s) = shrunk {
            if weight > 0.0 {
                weight_sum += weight;
                log_sum += f64::from(weight) * f64::from(s.max(1.0)).ln();
                if s < 80.0 {
                    let lost = weight * (100.0 - s);
                    if let Some((code, zh, en)) = deduction_reason(
                        dim,
                        s,
                        fused_p,
                        p50_ms,
                        p90_ms,
                        jitter_ms,
                        loss_rate,
                        drop_rate,
                        throughput_bps,
                        fresh_reachability,
                    ) {
                        deductions.push(HealthDeduction {
                            dim,
                            points_lost: (lost * 10.0).round() / 10.0,
                            code: code.to_owned(),
                            zh,
                            en,
                        });
                    }
                }
            }
        }
        dims.push(DimBreakdown {
            dim,
            score: shrunk,
            effective_samples: dim_n,
            p50_ms: (dim == HealthDim::Latency).then_some(p50_ms).flatten(),
            p90_ms: (dim == HealthDim::Latency).then_some(p90_ms).flatten(),
            jitter_ms: (dim == HealthDim::Stability).then_some(jitter_ms).flatten(),
            loss_rate: (dim == HealthDim::Stability).then_some(loss_rate).flatten(),
            drop_rate: (dim == HealthDim::Stability).then_some(drop_rate).flatten(),
            success_rate: (dim == HealthDim::Availability)
                .then_some(fused_p)
                .flatten(),
            throughput_bps: (dim == HealthDim::Throughput)
                .then_some(throughput_bps)
                .flatten(),
            passive_throughput: dim == HealthDim::Throughput && passive_throughput,
            active_samples: match dim {
                HealthDim::Availability => active_count,
                HealthDim::Latency => rtts.len() as u32,
                HealthDim::Stability => ok_rtts.len() as u32,
                HealthDim::Throughput => active_tputs.len() as u32,
                HealthDim::Reachability => u32::from(s_reach.is_some()),
            },
            passive_samples: match dim {
                HealthDim::Availability | HealthDim::Stability => conn_count,
                HealthDim::Throughput => passive_tputs.len() as u32,
                _ => 0,
            },
        });
    }

    let mut score = if weight_sum > 0.0 {
        (log_sum / f64::from(weight_sum)).exp() as f32
    } else {
        0.0
    };

    // Hard Gates
    let last_3_all_fail =
        probe_samples.len() >= 3 && probe_samples.iter().rev().take(3).all(|s| !s.ok);
    let recent_passive_ok = state
        .passive
        .iter()
        .any(|e| !e.throughput_only && e.ok && now.saturating_sub(e.ts) <= 900);
    let live_traffic_ok = recent_passive_ok
        || (state.last_active_at > 0 && now.saturating_sub(state.last_active_at) <= 120);
    let last_security_err = probe_samples
        .iter()
        .rev()
        .next()
        .is_some_and(|s| !s.ok && matches!(s.err_stage, ErrStage::Tls | ErrStage::Reality));
    let service_restricted = match profile {
        HealthProfile::Ai => fresh_reachability
            .and_then(|r| r.ai_score())
            .is_some_and(|s| s <= 1.0),
        HealthProfile::Streaming => fresh_reachability
            .and_then(|r| r.media_score())
            .is_some_and(|s| s <= 1.0),
        HealthProfile::LowLatency => fresh_reachability
            .is_some_and(|r| r.udp_stun.status == ReachabilityCheckStatus::Blocked),
        _ => false,
    };

    let gate = if !live_traffic_ok
        && (last_3_all_fail || (probe_samples.len() == 1 && !probe_samples[0].ok))
    {
        if last_3_all_fail {
            Some(HealthGate::Down)
        } else if last_security_err {
            Some(HealthGate::SecurityError)
        } else {
            Some(HealthGate::Unstable)
        }
    } else if last_security_err {
        Some(HealthGate::SecurityError)
    } else if active_count >= 3 && !live_traffic_ok && fused_p.is_some_and(|p| p < 0.80) {
        Some(HealthGate::Unstable)
    } else if service_restricted {
        Some(HealthGate::ServiceRestricted)
    } else {
        None
    };

    if let Some(g) = gate {
        score = score.min(g.cap());
    }

    deductions.sort_by(|a, b| {
        b.points_lost
            .partial_cmp(&a.points_lost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Some(WindowResult {
        score: score.clamp(0.0, 100.0),
        gate,
        confidence,
        dims,
        deductions,
    })
}

fn shrink_high_only(score: f32, n: f32) -> f32 {
    if score > PRIOR_SCORE {
        (n * score + PRIOR_SAMPLES * PRIOR_SCORE) / (n + PRIOR_SAMPLES)
    } else {
        score
    }
}

const fn profile_weights(profile: HealthProfile) -> [f32; 5] {
    match profile {
        HealthProfile::General => [0.30, 0.20, 0.20, 0.15, 0.15],
        HealthProfile::LowLatency => [0.25, 0.35, 0.30, 0.05, 0.05],
        HealthProfile::Bulk => [0.25, 0.10, 0.20, 0.40, 0.05],
        HealthProfile::Ai => [0.25, 0.15, 0.20, 0.05, 0.35],
        HealthProfile::Streaming => [0.20, 0.05, 0.20, 0.30, 0.25],
        HealthProfile::MeshExit => [0.30, 0.25, 0.30, 0.15, 0.00],
    }
}

#[allow(clippy::too_many_arguments)]
fn deduction_reason(
    dim: HealthDim,
    _score: f32,
    fused_p: Option<f32>,
    p50_ms: Option<u32>,
    p90_ms: Option<u32>,
    jitter_ms: Option<u32>,
    loss_rate: Option<f32>,
    drop_rate: Option<f32>,
    throughput_bps: Option<u64>,
    reach: Option<&ReachabilityReport>,
) -> Option<(&'static str, String, String)> {
    match dim {
        HealthDim::Availability => {
            let pct = (fused_p? * 100.0).round() as u32;
            Some((
                "low_availability",
                format!("可用率偏低：近期成功率为 {pct}%"),
                format!("Low availability: {pct}% recent success rate"),
            ))
        }
        HealthDim::Latency => {
            let p50 = p50_ms?;
            let p90 = p90_ms.unwrap_or(p50);
            Some((
                "high_latency",
                format!("延迟偏高：P50 {p50} ms · P90 {p90} ms"),
                format!("High latency: P50 {p50} ms · P90 {p90} ms"),
            ))
        }
        HealthDim::Stability => {
            let mut zh_parts = Vec::new();
            let mut en_parts = Vec::new();
            if let Some(j) = jitter_ms.filter(|&v| v > 0) {
                zh_parts.push(format!("平均抖动 ±{j} ms"));
                en_parts.push(format!("avg jitter ±{j} ms"));
            }
            if let Some(loss) = loss_rate.filter(|&v| v > 0.0) {
                zh_parts.push(format!("丢包率 {:.1}%", loss * 100.0));
                en_parts.push(format!("loss {:.1}%", loss * 100.0));
            }
            if let Some(drop) = drop_rate.filter(|&v| v > 0.0) {
                zh_parts.push(format!("异常短连接率 {:.1}%", drop * 100.0));
                en_parts.push(format!("drop rate {:.1}%", drop * 100.0));
            }
            if zh_parts.is_empty() {
                zh_parts.push("平均抖动 ±0 ms".into());
                en_parts.push("avg jitter ±0 ms".into());
            }
            Some((
                "unstable",
                format!("链路波动：{}", zh_parts.join(" · ")),
                format!("Unstable link: {}", en_parts.join(" · ")),
            ))
        }
        HealthDim::Throughput => {
            let speed = crate::util::format_speed(throughput_bps?);
            Some((
                "low_throughput",
                format!("带宽有限：实测约 {speed}"),
                format!("Limited throughput: ~{speed}"),
            ))
        }
        HealthDim::Reachability => {
            let r = reach?;
            let mut blocked_zh = Vec::new();
            let mut blocked_en = Vec::new();
            for (zh, en, res) in [
                ("ChatGPT", "ChatGPT", &r.chatgpt),
                ("Claude", "Claude", &r.claude),
                ("Gemini", "Gemini", &r.gemini),
                ("Netflix", "Netflix", &r.netflix),
                ("YouTube", "YouTube", &r.youtube),
                ("谷歌学术", "Google Scholar", &r.scholar),
                ("UDP", "UDP", &r.udp_stun),
            ] {
                if matches!(
                    res.status,
                    ReachabilityCheckStatus::Blocked | ReachabilityCheckStatus::Limited
                ) {
                    blocked_zh.push(zh);
                    blocked_en.push(en);
                }
            }
            if blocked_zh.is_empty() {
                None
            } else {
                Some((
                    "reachability_limited",
                    format!("部分服务受限：{}", blocked_zh.join("、")),
                    format!("Limited services: {}", blocked_en.join(", ")),
                ))
            }
        }
    }
}

fn summarize_24h(hourly: &BTreeMap<i64, HourlyAccumulator>) -> Vec<HourlySummarySlot> {
    let mut per_hour: [Vec<(u32, f32)>; 24] = std::array::from_fn(|_| Vec::new());
    for acc in hourly.values() {
        let Some(dt) = Utc.timestamp_opt(acc.hour_bucket, 0).single() else {
            continue;
        };
        let local_dt: DateTime<Local> = dt.with_timezone(&Local);
        let hour = local_dt
            .format("%H")
            .to_string()
            .parse::<usize>()
            .unwrap_or(0)
            % 24;
        if let Some(p50) = acc.p50_ms() {
            let sr = if acc.total_weighted > 0.0 {
                (acc.ok_weighted / acc.total_weighted) as f32
            } else {
                1.0
            };
            per_hour[hour].push((p50, sr));
        }
    }
    per_hour
        .into_iter()
        .enumerate()
        .map(|(h, entries)| {
            if entries.is_empty() {
                HourlySummarySlot {
                    hour: h as u8,
                    avg_p50_ms: None,
                    success_rate: None,
                    sample_days: 0,
                }
            } else {
                let sum_rtt: u64 = entries.iter().map(|e| u64::from(e.0)).sum();
                let sum_sr: f32 = entries.iter().map(|e| e.1).sum();
                let count = entries.len() as u64;
                HourlySummarySlot {
                    hour: h as u8,
                    avg_p50_ms: Some((sum_rtt / count) as u32),
                    success_rate: Some(sum_sr / entries.len() as f32),
                    sample_days: entries.len().min(u8::MAX as usize) as u8,
                }
            }
        })
        .collect()
}

fn detect_peak_degradation(slots: &[HourlySummarySlot]) -> Option<PeakDegradation> {
    let valid: Vec<(u8, u32, u8)> = slots
        .iter()
        .filter_map(|s| Some((s.hour, s.avg_p50_ms?, s.sample_days)))
        .collect();
    if valid.len() < 4 {
        return None;
    }
    let multi_day = valid.iter().filter(|(_, _, days)| *days >= 3).count();
    if multi_day < 2 {
        return None;
    }
    let all_rtts: Vec<u32> = valid.iter().map(|(_, rtt, _)| *rtt).collect();
    let median = percentile_u32(&all_rtts, 50)?;
    if median == 0 {
        return None;
    }
    let threshold = (median as f64 * 1.50).ceil() as u32;
    let degraded: Vec<(u8, u32)> = valid
        .iter()
        .filter(|(_, rtt, days)| *days >= 3 && *rtt > threshold)
        .map(|(h, rtt, _)| (*h, *rtt))
        .collect();
    if degraded.len() < 2 {
        return None;
    }
    let start_hour = degraded.first()?.0;
    let end_hour = (degraded.last()?.0 + 1) % 24;
    let peak_p50_ms =
        (degraded.iter().map(|(_, r)| u64::from(*r)).sum::<u64>() / degraded.len() as u64) as u32;
    let increase_pct =
        (((peak_p50_ms as f64 - median as f64) / median as f64) * 100.0).round() as u32;
    Some(PeakDegradation {
        start_hour,
        end_hour,
        baseline_p50_ms: median,
        peak_p50_ms,
        increase_pct,
    })
}

fn percentile_u32(values: &[u32], pct: usize) -> Option<u32> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) * pct) / 100;
    Some(sorted[idx])
}

fn percentile_u64(values: &[u64], pct: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) * pct) / 100;
    Some(sorted[idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DIRECT_VIRTUAL_NODE_ID;

    fn http_sample(target: &str, ts: i64, ok: bool, rtt_ms: Option<u32>) -> ProbeSampleRecord {
        ProbeSampleRecord {
            target_key: target.into(),
            kind: ProbeKind::Http,
            ts,
            ok,
            rtt_ms,
            bps: None,
            bytes_used: 512,
            err_stage: if ok { ErrStage::None } else { ErrStage::Tcp },
        }
    }

    #[test]
    fn clean_active_probes_reach_grade_a_without_beta_ceiling() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        for i in 0..5 {
            assert!(engine.record_probe(http_sample("node-a", now - (4 - i) * 30, true, Some(80))));
        }
        let view = engine.build_view("node-a", now).unwrap();
        let eval = view.general.unwrap();
        assert_eq!(eval.grade, HealthGrade::A);
        assert!(eval.score >= 85.0, "expected >= 85, got {}", eval.score);
        assert!(eval.confidence >= 0.6);
        assert!(eval.gate.is_none());
    }

    #[test]
    fn single_failed_probe_is_never_inflated_by_prior() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        assert!(engine.record_probe(http_sample("node-dead", now, false, None)));
        let eval = engine
            .evaluate_for_profile("node-dead", HealthProfile::General, now)
            .unwrap();
        assert!(eval.score <= 30.0, "expected <= 30, got {}", eval.score);
        assert!(eval.gate.is_some());
    }

    #[test]
    fn geometric_mean_penalizes_single_weak_dimension_harder_than_arithmetic() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        // 4 ok + 1 failed => availability p = 0.80 => s_A = 0 -> gate Unstable (30),
        // also check 9 ok + 1 failed => p = 0.90 => s_A = 25 (no gate), L=100, S=100.
        for i in 0..9 {
            engine.record_probe(http_sample(
                "node-flaky",
                now - (9 - i) * 60,
                true,
                Some(80),
            ));
        }
        engine.record_probe(http_sample("node-flaky", now - 30, false, None));
        let eval = engine
            .evaluate_for_profile("node-flaky", HealthProfile::General, now)
            .unwrap();
        assert!(
            eval.score < 70.0,
            "weak availability must pull score down: {}",
            eval.score
        );
    }

    #[test]
    fn jitter_floor_prevents_overpenalizing_fast_nodes() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        for (i, rtt) in [25, 33, 27, 34, 28].into_iter().enumerate() {
            engine.record_probe(http_sample(
                "node-fast",
                now - (4 - i as i64) * 30,
                true,
                Some(rtt),
            ));
        }
        let eval = engine
            .evaluate_for_profile("node-fast", HealthProfile::General, now)
            .unwrap();
        let stab = eval
            .dims
            .iter()
            .find(|d| d.dim == HealthDim::Stability)
            .unwrap()
            .score
            .unwrap();
        assert!(
            stab >= 75.0,
            "80ms floor should protect fast node stability, got {stab}"
        );
        assert_eq!(eval.grade, HealthGrade::A);
    }

    #[test]
    fn host_offline_guard_freezes_simultaneous_network_failures() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        engine.record_probe(http_sample("n1", now - 10, true, Some(80)));
        engine.record_probe(http_sample("n1", now - 5, true, Some(80)));
        // Now 3 distinct nodes fail simultaneously within 15s (e.g. Wi-Fi disconnected)
        assert!(engine.record_probe(http_sample("n2", now, false, None)));
        assert!(engine.record_probe(http_sample("n3", now, false, None)));
        // 3rd distinct failure triggers host offline guard and suppresses penalty on n1
        assert!(!engine.record_probe(http_sample("n1", now, false, None)));
        let eval = engine
            .evaluate_for_profile("n1", HealthProfile::General, now)
            .unwrap();
        assert_eq!(eval.grade, HealthGrade::A);
    }

    #[test]
    fn cross_node_target_fault_exempts_passive_failures() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        let resolver = |chains: &[String]| chains.first().cloned();
        let mut broken_on_direct = ClashConnection::default();
        broken_on_direct.id = "c1".into();
        broken_on_direct.metadata.network = "tcp".into();
        broken_on_direct.metadata.host = "broken.example.com".into();
        broken_on_direct.upload = 128;
        broken_on_direct.download = 0;
        broken_on_direct.chains = vec![DIRECT_VIRTUAL_NODE_ID.into()];

        let mut broken_on_node = broken_on_direct.clone();
        broken_on_node.id = "c2".into();
        broken_on_node.chains = vec!["node-a".into()];

        engine.observe_passive_singbox(
            &[broken_on_direct, broken_on_node],
            &[(0, 0), (0, 0)],
            resolver,
            now,
        );
        // Both connections disappear next tick (zero RX short flow)
        engine.observe_passive_singbox(&[], &[], resolver, now + 1);
        // First one counted; second saw cross-node failure on same target hash and was exempted!
        let node_state = engine.targets.get("node-a");
        assert!(node_state.is_none_or(|s| s.passive.is_empty()));
    }

    #[test]
    fn peak_hour_degradation_is_detected_across_multiple_days() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        let mut buckets = Vec::new();
        // Construct 4 days of local hours 10..12 fast (60ms) and 20..22 slow (220ms)
        let base_day = (now / 86400) * 86400;
        for day in 0..4 {
            for hour in [10i64, 11, 20, 21] {
                let dt = Local
                    .with_ymd_and_hms(2026, 11, 10 + day as u32, hour as u32, 0, 0)
                    .unwrap();
                let ts = dt.timestamp();
                let rtt = if hour >= 20 { 220 } else { 60 };
                buckets.push(HourlyBucket {
                    target_key: "node-vps".into(),
                    hour_bucket: hour_floor(ts),
                    ok_weighted: 10.0,
                    total_weighted: 10.0,
                    rtt_p50_ms: Some(rtt),
                    rtt_p90_ms: Some(rtt + 20),
                    tput_p75_bps: None,
                });
            }
        }
        let _ = base_day;
        let ref_ts = Local
            .with_ymd_and_hms(2026, 11, 14, 12, 0, 0)
            .unwrap()
            .timestamp();
        engine.load_persisted(Vec::new(), buckets, Vec::new(), ref_ts);
        let view = engine.build_view("node-vps", ref_ts).unwrap();
        let peak = view
            .peak_degradation
            .expect("should detect evening peak degradation");
        assert_eq!(peak.start_hour, 20);
        assert_eq!(peak.end_hour, 22);
        assert!(peak.increase_pct >= 50);
    }

    #[test]
    fn reachability_profiles_compute_ai_and_streaming_scores_independently() {
        let report = ReachabilityReport {
            checked_at: 100,
            egress_region: Some("JP".into()),
            chatgpt: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            claude: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            gemini: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            copilot: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            netflix: ServiceCheckResult::new(ReachabilityCheckStatus::Blocked),
            youtube: ServiceCheckResult::new(ReachabilityCheckStatus::Limited),
            disney: ServiceCheckResult::new(ReachabilityCheckStatus::Blocked),
            tiktok: ServiceCheckResult::new(ReachabilityCheckStatus::Blocked),
            web_clean: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            scholar: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            dev_hub: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            udp_stun: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
        };
        assert_eq!(report.ai_score(), Some(100.0));
        assert_eq!(report.media_score(), Some(10.0));
        assert_eq!(report.general_score(), Some(100.0));
        let masked = report.masked_for_privacy();
        assert!(masked.egress_region.is_none());
    }

    #[test]
    fn passive_short_closes_without_active_probes_never_produce_false_grade_f() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        let resolver = |chains: &[String]| chains.first().cloned();
        let mut short_close = ClashConnection::default();
        short_close.id = "c1".into();
        short_close.metadata.network = "tcp".into();
        short_close.metadata.host = "ok.example.com".into();
        short_close.upload = 56;
        short_close.download = 0;
        short_close.chains = vec!["node-vision".into()];

        engine.observe_passive_singbox(&[short_close], &[(0, 0)], resolver, now);
        engine.observe_passive_singbox(&[], &[], resolver, now + 1);
        assert!(
            engine.build_view("node-vision", now + 2).is_none(),
            "passive-only nodes must remain untested ([—]), never F 1"
        );
    }

    #[test]
    fn active_downloading_flow_protects_working_node_from_false_down_gate() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        let resolver = |chains: &[String]| chains.first().cloned();
        let mut live_conn = ClashConnection::default();
        live_conn.id = "c-live".into();
        live_conn.metadata.network = "tcp".into();
        live_conn.metadata.host = "stream.example.com".into();
        live_conn.upload = 512;
        live_conn.download = 4096;
        live_conn.chains = vec!["node-vision".into()];

        // Still open and actively downloading.
        engine.observe_passive_singbox(&[live_conn], &[(0, 8192)], resolver, now);
        // Transient failed probe must not hard-gate a node carrying live download traffic.
        engine.record_probe(http_sample("node-vision", now + 1, false, None));
        let eval = engine
            .evaluate_for_profile("node-vision", HealthProfile::General, now + 2)
            .unwrap();
        assert!(
            eval.gate.is_none(),
            "active downloading flow must protect node from false Down/Unstable gate, got {:?}",
            eval.gate
        );
    }

    #[test]
    fn newer_active_download_overrides_older_passive_throughput() {
        let mut engine = HealthEngine::new();
        let now = 1_700_000_000;
        let tput_dim = |view: NodeHealthView| {
            view.general
                .unwrap()
                .dims
                .into_iter()
                .find(|d| d.dim == HealthDim::Throughput)
                .unwrap()
        };
        engine.record_probe(http_sample("node-vision", now - 200, true, Some(120)));
        engine.push_passive_event(
            "node-vision",
            PassiveConnEvent {
                ts: now - 100,
                ok: true,
                abnormal_drop: false,
                peak_bps: Some(4_725_000),
                throughput_only: true,
            },
        );
        let before = tput_dim(engine.build_view("node-vision", now).unwrap());
        assert_eq!(before.throughput_bps, Some(4_725_000));
        assert!(before.passive_throughput);

        // Active throughput probe after the older passive flow must show up immediately.
        engine.record_probe(ProbeSampleRecord {
            target_key: "node-vision".into(),
            kind: ProbeKind::Download,
            ts: now - 10,
            ok: true,
            rtt_ms: None,
            bps: Some(8_400_000),
            bytes_used: 8_400_000,
            err_stage: ErrStage::None,
        });
        let after = tput_dim(engine.build_view("node-vision", now).unwrap());
        assert_eq!(after.throughput_bps, Some(8_400_000));
        assert!(!after.passive_throughput);

        // A later passive >2 MiB flow resumes passive dominance (P75 of passive samples).
        engine.push_passive_event(
            "node-vision",
            PassiveConnEvent {
                ts: now - 5,
                ok: true,
                abnormal_drop: false,
                peak_bps: Some(6_100_000),
                throughput_only: true,
            },
        );
        let resumed = tput_dim(engine.build_view("node-vision", now).unwrap());
        assert!(resumed.passive_throughput);
        assert_eq!(resumed.throughput_bps, Some(4_725_000));
    }
}
