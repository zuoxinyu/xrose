//! Kernel-independent traffic aggregates. Raw connection metadata stays in memory.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Days, Local, Months, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::model::{AppModel, CoreKind};
use crate::runtime::{ConnectionsResponse, XrayTraffic};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficDimension {
    #[default]
    Node,
    Process,
    Domain,
}
impl TrafficDimension {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Process => "process",
            Self::Domain => "domain",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficWindow {
    #[default]
    Session,
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrafficQuery {
    pub dimension: TrafficDimension,
    pub window: TrafficWindow,
    /// None follows the current local date, including midnight rollover.
    pub date: Option<NaiveDate>,
    pub limit: usize,
}
impl Default for TrafficQuery {
    fn default() -> Self {
        Self {
            dimension: TrafficDimension::Node,
            window: TrafficWindow::Session,
            date: None,
            limit: 50,
        }
    }
}
impl TrafficQuery {
    pub fn range(&self, today: NaiveDate) -> (NaiveDate, NaiveDate) {
        let date = self.date.unwrap_or(today).min(today);
        match self.window {
            TrafficWindow::Session => (NaiveDate::MIN, today),
            TrafficWindow::Day => (date, date),
            TrafficWindow::Week => {
                let start = date - Days::new(date.weekday().num_days_from_monday() as u64);
                (start, (start + Days::new(6)).min(today))
            }
            TrafficWindow::Month => {
                let start = date.with_day(1).unwrap();
                (start, (start + Months::new(1) - Days::new(1)).min(today))
            }
        }
    }
    pub fn shifted(&self, forward: bool, today: NaiveDate) -> Self {
        let mut query = self.clone();
        let date = self.date.unwrap_or(today).min(today);
        let next = match self.window {
            TrafficWindow::Session => today,
            TrafficWindow::Day => {
                if forward {
                    date + Days::new(1)
                } else {
                    date - Days::new(1)
                }
            }
            TrafficWindow::Week => {
                if forward {
                    date + Days::new(7)
                } else {
                    date - Days::new(7)
                }
            }
            TrafficWindow::Month => {
                if forward {
                    date + Months::new(1)
                } else {
                    date - Months::new(1)
                }
            }
        };
        query.date = Some(next.min(today));
        query
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrafficBytes {
    pub upload: u64,
    pub download: u64,
}
impl TrafficBytes {
    pub fn total(self) -> u64 {
        self.upload.saturating_add(self.download)
    }
    pub(crate) fn add(&mut self, other: Self) {
        self.upload = self.upload.saturating_add(other.upload);
        self.download = self.download.saturating_add(other.download);
    }
    fn delta(self, old: Self) -> Self {
        Self {
            upload: self.upload.saturating_sub(old.upload),
            download: self.download.saturating_sub(old.download),
        }
    }
    fn decreased(self, old: Self) -> bool {
        self.upload < old.upload || self.download < old.download
    }
}
impl From<(u64, u64)> for TrafficBytes {
    fn from(value: (u64, u64)) -> Self {
        Self {
            upload: value.0,
            download: value.1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficEntityKind {
    #[default]
    Named,
    Direct,
    Unknown,
    NoDomain,
    Unattributed,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficEntity {
    pub kind: TrafficEntityKind,
    /// Stable model identity or normalized name; encrypted in the entity table.
    pub key: String,
    pub label: String,
}
impl TrafficEntity {
    fn special(kind: TrafficEntityKind) -> Self {
        Self {
            kind,
            key: String::new(),
            label: String::new(),
        }
    }
    fn named(key: String, label: String) -> Self {
        Self {
            kind: TrafficEntityKind::Named,
            key,
            label,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficRow {
    pub core: CoreKind,
    pub entity: TrafficEntity,
    pub bytes: TrafficBytes,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrafficStatisticsView {
    pub query: TrafficQuery,
    pub range_start: Option<NaiveDate>,
    pub range_end: Option<NaiveDate>,
    pub total: TrafficBytes,
    pub rows: Vec<TrafficRow>,
    pub row_count: usize,
    pub has_singbox: bool,
    pub has_xray: bool,
    pub has_gaps: bool,
    pub last_sample_at: Option<i64>,
    pub loading: bool,
    pub query_error: Option<String>,
    pub save_error: Option<String>,
    pub collection_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoreEpoch {
    pub core: CoreKind,
    pub pid: Option<u32>,
    pub started_at: Option<i64>,
    pub restarts: u32,
    pub helper_session: Option<String>,
}
impl CoreEpoch {
    pub(crate) fn from_state(state: &crate::core::ProcessState) -> Option<Self> {
        state
            .phase
            .is_alive()
            .then(|| {
                Some(Self {
                    core: state.core?,
                    pid: state.pid,
                    started_at: state.started_at,
                    restarts: state.restarts,
                    helper_session: state.helper_session.clone(),
                })
            })
            .flatten()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TrafficBucket {
    pub session: String,
    pub date: NaiveDate,
    pub core: CoreKind,
    pub dimension: String,
    pub entity_id: String,
    pub entity: TrafficEntity,
    pub bytes: TrafficBytes,
    pub last_sample_at: i64,
    pub has_gaps: bool,
}
impl TrafficBucket {
    pub(crate) fn identity(&self) -> (String, NaiveDate, String) {
        (self.session.clone(), self.date, self.entity_id.clone())
    }
}

pub(crate) struct TrafficAccumulator {
    pub session: String,
    pub started_at: i64,
    pub enabled: bool,
    pub save_error: Option<String>,
    pub collection_error: bool,
    epoch: Option<CoreEpoch>,
    baseline_epoch: Option<CoreEpoch>,
    total: TrafficBytes,
    counters: BTreeMap<String, TrafficBytes>,
    entities: BTreeMap<(String, String, TrafficEntityKind, String), String>,
    pub buckets: BTreeMap<(NaiveDate, String), TrafficBucket>,
    pub dirty: BTreeSet<(NaiveDate, String)>,
}
impl TrafficAccumulator {
    pub fn new(enabled: bool) -> Self {
        Self {
            session: crate::util::new_id("traffic"),
            started_at: crate::util::now_unix(),
            enabled,
            save_error: None,
            collection_error: false,
            epoch: None,
            baseline_epoch: None,
            total: TrafficBytes::default(),
            counters: BTreeMap::new(),
            entities: BTreeMap::new(),
            buckets: BTreeMap::new(),
            dirty: BTreeSet::new(),
        }
    }
    pub fn rebase(&mut self, enabled: bool, epoch: Option<CoreEpoch>) {
        self.enabled = enabled;
        self.epoch = None;
        self.baseline_epoch = epoch;
        self.total = TrafficBytes::default();
        self.counters.clear();
        self.collection_error = false;
    }
    pub fn clear(&mut self, epoch: Option<CoreEpoch>) {
        self.buckets.clear();
        self.entities.clear();
        self.dirty.clear();
        self.save_error = None;
        self.rebase(self.enabled, epoch);
    }
    pub fn mark_collection_gap(&mut self) {
        self.collection_error = true;
        // Preserve the gap even if the application exits before another sample.
        let latest = self
            .buckets
            .iter()
            .filter(|(_, bucket)| bucket.dimension == "summary")
            .max_by_key(|(_, bucket)| bucket.last_sample_at)
            .map(|(key, _)| key.clone());
        if let Some(key) = latest {
            self.buckets.get_mut(&key).unwrap().has_gaps = true;
            self.dirty.insert(key);
        }
    }
    fn begin(&mut self, epoch: CoreEpoch, total: TrafficBytes) -> Option<bool> {
        if !self.enabled {
            return None;
        }
        let changed = self.epoch.as_ref() != Some(&epoch);
        let baseline = (changed && self.baseline_epoch.as_ref() == Some(&epoch))
            || (!changed && total.decreased(self.total));
        if !changed && total.decreased(self.total) {
            self.collection_error = true;
        }
        if changed || baseline {
            self.counters.clear();
        }
        self.epoch = Some(epoch);
        if changed {
            self.total = TrafficBytes::default();
        }
        Some(baseline)
    }
    fn add(
        &mut self,
        date: NaiveDate,
        at: i64,
        core: CoreKind,
        dimension: &str,
        entity: TrafficEntity,
        bytes: TrafficBytes,
    ) {
        if bytes.total() == 0 && entity.kind != TrafficEntityKind::Summary {
            return;
        }
        let key = (
            core.as_str().to_string(),
            dimension.to_string(),
            entity.kind.clone(),
            entity.key.clone(),
        );
        let id = self
            .entities
            .entry(key)
            .or_insert_with(|| crate::util::new_id("entity"))
            .clone();
        let bucket_key = (date, id.clone());
        let bucket = self
            .buckets
            .entry(bucket_key.clone())
            .or_insert_with(|| TrafficBucket {
                session: self.session.clone(),
                date,
                core,
                dimension: dimension.into(),
                entity_id: id,
                entity: entity.clone(),
                bytes: TrafficBytes::default(),
                last_sample_at: at,
                has_gaps: false,
            });
        bucket.entity.label = entity.label;
        bucket.bytes.add(bytes);
        bucket.last_sample_at = at;
        bucket.has_gaps |= self.collection_error;
        self.dirty.insert(bucket_key);
    }
    pub fn observe_singbox(
        &mut self,
        epoch: CoreEpoch,
        model: &AppModel,
        response: &ConnectionsResponse,
        date: NaiveDate,
        at: i64,
    ) {
        let total = (response.upload_total, response.download_total).into();
        let Some(baseline) = self.begin(epoch, total) else {
            return;
        };
        let delta = if baseline {
            TrafficBytes::default()
        } else {
            total.delta(self.total)
        };
        let mut attributed = TrafficBytes::default();
        for connection in &response.connections {
            let current: TrafficBytes = (connection.upload, connection.download).into();
            let key = format!("{}:{}", connection.id, connection.start);
            let previous = self.counters.insert(key, current);
            let bytes = if baseline || previous.is_some_and(|old| current.decreased(old)) {
                TrafficBytes::default()
            } else {
                current.delta(previous.unwrap_or_default())
            };
            // Global and connection counters are not read atomically by the kernel.
            // Keep each sample within the authoritative total without inventing bytes.
            let remaining = delta.delta(attributed);
            let bytes = TrafficBytes {
                upload: bytes.upload.min(remaining.upload),
                download: bytes.download.min(remaining.download),
            };
            attributed.add(bytes);
            let node = connection
                .chains
                .iter()
                .find_map(|tag| node_entity(model, tag))
                .unwrap_or_else(|| TrafficEntity::special(TrafficEntityKind::Unknown));
            let process = process_name(
                &connection.metadata.process,
                &connection.metadata.process_path,
            );
            let process = if process.is_empty() {
                TrafficEntity::special(TrafficEntityKind::Unknown)
            } else {
                TrafficEntity::named(process.clone(), process)
            };
            let domain = main_domain(if connection.metadata.host.is_empty() {
                &connection.metadata.sniff_host
            } else {
                &connection.metadata.host
            });
            let domain = domain
                .map(|name| TrafficEntity::named(name.clone(), name))
                .unwrap_or_else(|| TrafficEntity::special(TrafficEntityKind::NoDomain));
            for (dimension, entity) in [
                (TrafficDimension::Node, node),
                (TrafficDimension::Process, process),
                (TrafficDimension::Domain, domain),
            ] {
                self.add(date, at, CoreKind::SingBox, dimension.key(), entity, bytes);
            }
        }
        // Only active connections can appear in subsequent snapshots.
        let active: BTreeSet<_> = response
            .connections
            .iter()
            .map(|c| format!("{}:{}", c.id, c.start))
            .collect();
        self.counters.retain(|id, _| active.contains(id));
        for dimension in [
            TrafficDimension::Node,
            TrafficDimension::Process,
            TrafficDimension::Domain,
        ] {
            self.add(
                date,
                at,
                CoreKind::SingBox,
                dimension.key(),
                TrafficEntity::special(TrafficEntityKind::Unattributed),
                delta.delta(attributed),
            );
        }
        self.add(
            date,
            at,
            CoreKind::SingBox,
            "summary",
            TrafficEntity::special(TrafficEntityKind::Summary),
            delta,
        );
        self.total = total;
        self.collection_error = false;
    }
    pub fn observe_xray(
        &mut self,
        epoch: CoreEpoch,
        model: &AppModel,
        traffic: &XrayTraffic,
        inbound_tags: &[String],
        date: NaiveDate,
        at: i64,
    ) {
        let mut total = TrafficBytes::default();
        for tag in inbound_tags {
            if let Some(bytes) = traffic.per_inbound.get(tag) {
                total.add((*bytes).into());
            }
        }
        let Some(baseline) = self.begin(epoch, total) else {
            return;
        };
        let delta = if baseline {
            TrafficBytes::default()
        } else {
            total.delta(self.total)
        };
        for (tag, pair) in &traffic.per_outbound {
            let Some(entity) = node_entity(model, tag) else {
                continue;
            };
            let current: TrafficBytes = (*pair).into();
            let previous = self.counters.insert(tag.clone(), current);
            let bytes = if baseline || previous.is_some_and(|old| current.decreased(old)) {
                TrafficBytes::default()
            } else {
                current.delta(previous.unwrap_or_default())
            };
            self.add(date, at, CoreKind::Xray, "node", entity, bytes);
        }
        self.add(
            date,
            at,
            CoreKind::Xray,
            "summary",
            TrafficEntity::special(TrafficEntityKind::Summary),
            delta,
        );
        self.total = total;
        self.collection_error = false;
    }
}

fn node_entity(model: &AppModel, tag: &str) -> Option<TrafficEntity> {
    if tag == "direct" {
        return Some(TrafficEntity::special(TrafficEntityKind::Direct));
    }
    model
        .nodes
        .iter()
        .find(|node| node.outbound_tag() == tag)
        .map(|node| TrafficEntity::named(node.id.clone(), node.name.clone()))
}

/// Some kernels append account metadata to the path; never retain that suffix.
fn process_name(process: &str, path: &str) -> String {
    let name = crate::flow::process_basename(process, path);
    #[cfg(windows)]
    return name.to_lowercase();
    #[cfg(not(windows))]
    name.to_string()
}

fn main_domain(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.');
    if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let domain = match url::Host::parse(host).ok()? {
        url::Host::Domain(domain) => domain.to_lowercase(),
        _ => return None,
    };
    if psl::suffix(domain.as_bytes()).is_some_and(|suffix| suffix.is_known()) {
        Some(psl::domain_str(&domain).unwrap_or(&domain).to_string())
    } else {
        Some(domain)
    }
}

pub(crate) fn build_view(
    query: TrafficQuery,
    session: &str,
    buckets: impl IntoIterator<Item = TrafficBucket>,
    today: NaiveDate,
) -> TrafficStatisticsView {
    let (start, end) = query.range(today);
    let mut view = TrafficStatisticsView {
        query: query.clone(),
        range_start: Some(start),
        range_end: Some(end),
        ..Default::default()
    };
    let mut rows: BTreeMap<(String, TrafficEntityKind, String), (TrafficRow, i64)> =
        BTreeMap::new();
    for bucket in buckets {
        let included = if query.window == TrafficWindow::Session {
            bucket.session == session
        } else {
            bucket.date >= start && bucket.date <= end
        };
        if !included {
            continue;
        }
        if bucket.dimension == "summary" {
            view.total.add(bucket.bytes);
            view.has_singbox |= bucket.core == CoreKind::SingBox;
            view.has_xray |= bucket.core == CoreKind::Xray;
            view.has_gaps |= bucket.has_gaps;
            view.last_sample_at = Some(view.last_sample_at.unwrap_or(0).max(bucket.last_sample_at));
        } else if bucket.dimension == query.dimension.key() {
            let key = (
                bucket.core.as_str().to_string(),
                bucket.entity.kind.clone(),
                bucket.entity.key.clone(),
            );
            let (row, updated) = rows.entry(key).or_insert_with(|| {
                (
                    TrafficRow {
                        core: bucket.core,
                        entity: bucket.entity.clone(),
                        bytes: TrafficBytes::default(),
                    },
                    0,
                )
            });
            row.bytes.add(bucket.bytes);
            if bucket.last_sample_at >= *updated {
                row.entity = bucket.entity;
                *updated = bucket.last_sample_at;
            }
        }
    }
    view.rows = rows.into_values().map(|(row, _)| row).collect();
    view.rows.sort_by(|a, b| {
        b.bytes
            .total()
            .cmp(&a.bytes.total())
            .then_with(|| a.entity.label.cmp(&b.entity.label))
            .then_with(|| a.entity.key.cmp(&b.entity.key))
            .then_with(|| a.core.as_str().cmp(b.core.as_str()))
    });
    view.row_count = view.rows.len();
    view.rows.truncate(query.limit.max(50));
    view
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ClashConnection;
    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn epoch(n: u32) -> CoreEpoch {
        CoreEpoch {
            core: CoreKind::SingBox,
            pid: Some(n),
            started_at: Some(n as i64),
            restarts: 0,
            helper_session: None,
        }
    }
    fn sample(up: u64, known: u64) -> ConnectionsResponse {
        let mut connection = ClashConnection {
            id: "fixture".into(),
            upload: known,
            chains: vec!["direct".into()],
            ..Default::default()
        };
        connection.metadata.host = "api.example.com".into();
        connection.metadata.process_path = "/opt/example/browser (example-account)".into();
        ConnectionsResponse {
            upload_total: up,
            connections: vec![connection],
            ..Default::default()
        }
    }
    #[test]
    fn delta_restart_toggle_and_missing_connections() {
        let mut a = TrafficAccumulator::new(true);
        let model = AppModel::default();
        let day = date("2026-11-06");
        a.observe_singbox(epoch(1), &model, &sample(100, 80), day, 1);
        a.observe_singbox(epoch(1), &model, &sample(100, 80), day, 2);
        a.observe_singbox(epoch(2), &model, &sample(30, 20), day, 3);
        let view = build_view(
            TrafficQuery::default(),
            &a.session,
            a.buckets.values().cloned(),
            day,
        );
        assert_eq!(view.total.upload, 130);
        assert_eq!(view.rows.iter().map(|r| r.bytes.upload).sum::<u64>(), 130);
        assert_eq!(
            view.rows
                .iter()
                .find(|r| r.entity.kind == TrafficEntityKind::Unattributed)
                .unwrap()
                .bytes
                .upload,
            30
        );
        a.rebase(false, Some(epoch(2)));
        a.observe_singbox(epoch(2), &model, &sample(70, 60), day, 4);
        a.rebase(true, Some(epoch(2)));
        a.observe_singbox(epoch(2), &model, &sample(90, 80), day, 5);
        a.observe_singbox(epoch(2), &model, &sample(100, 90), day, 6);
        assert_eq!(
            build_view(
                TrafficQuery::default(),
                &a.session,
                a.buckets.values().cloned(),
                day
            )
            .total
            .upload,
            140
        );
        a.observe_singbox(epoch(2), &model, &sample(1, 1), day, 7);
        assert_eq!(
            build_view(
                TrafficQuery::default(),
                &a.session,
                a.buckets.values().cloned(),
                day
            )
            .total
            .upload,
            140
        );
    }
    #[test]
    fn names_do_not_retain_paths_accounts_or_ip_addresses() {
        assert_eq!(
            process_name("", "/opt/example/browser (example-account)"),
            "browser"
        );
        assert_eq!(process_name("", "C:\\Example\\browser.exe"), "browser.exe");
        assert_eq!(process_name("", "1000"), "");
        assert_eq!(main_domain("API.Example.COM."), Some("example.com".into()));
        assert_eq!(
            main_domain("api.example.co.uk"),
            Some("example.co.uk".into())
        );
        assert_eq!(
            main_domain("bücher.example"),
            Some("xn--bcher-kva.example".into())
        );
        assert_eq!(main_domain("localhost"), Some("localhost".into()));
        assert_eq!(
            main_domain("API.Printer.LOCAL."),
            Some("api.printer.local".into())
        );
        assert_eq!(main_domain("192.0.2.1"), None);
        assert_eq!(main_domain("::1"), None);
    }
    #[test]
    fn natural_periods_and_future_dates() {
        let today = date("2024-03-01");
        let mut q = TrafficQuery {
            window: TrafficWindow::Month,
            date: Some(date("2024-02-29")),
            ..Default::default()
        };
        assert_eq!(q.range(today), (date("2024-02-01"), date("2024-02-29")));
        q.window = TrafficWindow::Week;
        assert_eq!(q.range(today), (date("2024-02-26"), today));
        q.date = Some(date("2025-01-01"));
        q.window = TrafficWindow::Day;
        assert_eq!(q.range(today), (today, today));
    }
    #[test]
    fn sessions_calendar_and_limits_keep_full_totals() {
        let day = date("2026-11-06");
        let mut a = TrafficAccumulator::new(true);
        a.observe_singbox(epoch(1), &AppModel::default(), &sample(100, 80), day, 1);
        let mut buckets: Vec<_> = a.buckets.values().cloned().collect();
        let mut previous = buckets.clone();
        for b in &mut previous {
            b.session = "previous".into();
        }
        buckets.extend(previous);
        assert_eq!(
            build_view(TrafficQuery::default(), &a.session, buckets.clone(), day)
                .total
                .upload,
            100
        );
        // Moving to an earlier local date must not hide this session's samples.
        assert_eq!(
            build_view(
                TrafficQuery::default(),
                &a.session,
                buckets.clone(),
                day - Days::new(1)
            )
            .total
            .upload,
            100
        );
        let q = TrafficQuery {
            window: TrafficWindow::Day,
            ..Default::default()
        };
        assert_eq!(build_view(q, &a.session, buckets, day).total.upload, 200);
    }
    #[test]
    fn cross_day_process_aggregation_and_clear_rebase() {
        let mut a = TrafficAccumulator::new(true);
        let model = AppModel::default();
        let first = date("2026-10-31");
        let second = date("2026-11-01");
        a.observe_singbox(epoch(1), &model, &sample(100, 100), first, 1);
        let mut next = sample(180, 150);
        let mut another = next.connections[0].clone();
        another.id = "second-instance".into();
        another.upload = 30;
        next.connections.push(another);
        a.observe_singbox(epoch(1), &model, &next, second, 2);
        let query = TrafficQuery {
            dimension: TrafficDimension::Process,
            window: TrafficWindow::Day,
            date: Some(second),
            ..Default::default()
        };
        let view = build_view(
            query.clone(),
            &a.session,
            a.buckets.values().cloned(),
            second,
        );
        assert_eq!(view.total.upload, 80);
        assert_eq!(view.rows.len(), 1);
        assert_eq!(view.rows[0].entity.label, "browser");
        assert_eq!(view.rows[0].bytes.upload, 80);
        a.clear(Some(epoch(1)));
        a.observe_singbox(epoch(1), &model, &next, second, 3);
        next.upload_total += 10;
        next.connections[0].upload += 10;
        a.observe_singbox(epoch(1), &model, &next, second, 4);
        assert_eq!(
            build_view(query, &a.session, a.buckets.values().cloned(), second)
                .total
                .upload,
            10
        );
    }
    #[test]
    fn xray_business_inbounds_node_rename_and_missing_categories() {
        let mut a = TrafficAccumulator::new(true);
        let day = date("2026-11-06");
        let mut model = AppModel::default();
        let node = crate::model::Node::new(
            "Example node",
            crate::model::Protocol::Socks,
            "proxy.example",
            1080,
        );
        let tag = node.outbound_tag();
        model.nodes.push(node);
        let mut source = epoch(1);
        source.core = CoreKind::Xray;
        let mut traffic = XrayTraffic::default();
        traffic.per_inbound.insert("api-in".into(), (9000, 9000));
        traffic.per_inbound.insert("business".into(), (100, 200));
        traffic.per_outbound.insert(tag.clone(), (120, 220));
        traffic.per_outbound.insert("direct".into(), (10, 20));
        a.observe_xray(
            source.clone(),
            &model,
            &traffic,
            &["business".into()],
            day,
            1,
        );
        model.nodes[0].name = "Renamed node".into();
        traffic.per_outbound.insert(tag, (130, 230));
        traffic.per_inbound.insert("business".into(), (110, 210));
        a.observe_xray(source, &model, &traffic, &["business".into()], day, 2);
        model.nodes.clear();
        let view = build_view(
            TrafficQuery::default(),
            &a.session,
            a.buckets.values().cloned(),
            day,
        );
        assert_eq!(
            view.total,
            TrafficBytes {
                upload: 110,
                download: 210
            }
        );
        assert_eq!(view.rows[0].entity.label, "Renamed node");
        assert_eq!(view.rows[0].bytes.upload, 130);
        let query = TrafficQuery {
            dimension: TrafficDimension::Domain,
            ..Default::default()
        };
        let view = build_view(query, &a.session, a.buckets.values().cloned(), day);
        assert!(view.has_xray);
        assert!(view.rows.is_empty());
        assert_eq!(view.total.upload, 110);
    }
    #[test]
    fn missing_metadata_and_failure_gaps_remain_visible() {
        let mut a = TrafficAccumulator::new(true);
        let day = date("2026-11-06");
        let mut response = sample(20, 10);
        response.connections[0].metadata = Default::default();
        response.connections[0].chains.clear();
        a.collection_error = true;
        a.observe_singbox(epoch(1), &AppModel::default(), &response, day, 1);
        for (dimension, kind) in [
            (TrafficDimension::Node, TrafficEntityKind::Unknown),
            (TrafficDimension::Process, TrafficEntityKind::Unknown),
            (TrafficDimension::Domain, TrafficEntityKind::NoDomain),
        ] {
            let view = build_view(
                TrafficQuery {
                    dimension,
                    ..Default::default()
                },
                &a.session,
                a.buckets.values().cloned(),
                day,
            );
            assert!(view.has_gaps);
            assert_eq!(view.rows.iter().map(|r| r.bytes.upload).sum::<u64>(), 20);
            assert!(view.rows.iter().any(|r| r.entity.kind == kind));
        }
        a.dirty.clear();
        a.mark_collection_gap();
        assert_eq!(a.dirty.len(), 1);
        assert!(
            a.buckets
                .values()
                .filter(|b| b.dimension == "summary")
                .all(|b| b.has_gaps)
        );
    }

    #[test]
    fn ranking_limits_preserve_totals_and_sort_order() {
        let day = date("2026-11-06");
        let mut a = TrafficAccumulator::new(true);
        for i in 1..=75 {
            a.add(
                day,
                i,
                CoreKind::SingBox,
                "node",
                TrafficEntity::named(format!("node-{i}"), format!("Example {i}")),
                TrafficBytes {
                    upload: i as u64,
                    download: i as u64 * 2,
                },
            );
        }
        a.add(
            day,
            75,
            CoreKind::SingBox,
            "summary",
            TrafficEntity::special(TrafficEntityKind::Summary),
            TrafficBytes {
                upload: 2850,
                download: 5700,
            },
        );
        let first = build_view(
            TrafficQuery::default(),
            &a.session,
            a.buckets.values().cloned(),
            day,
        );
        let all = build_view(
            TrafficQuery {
                limit: 100,
                ..Default::default()
            },
            &a.session,
            a.buckets.values().cloned(),
            day,
        );
        assert_eq!(first.rows.len(), 50);
        assert_eq!(first.row_count, 75);
        assert_eq!(first.total, all.total);
        assert_eq!(first.rows[0].entity.key, "node-75");
        assert!(
            all.rows
                .windows(2)
                .all(|rows| rows[0].bytes.total() >= rows[1].bytes.total())
        );
    }
}
