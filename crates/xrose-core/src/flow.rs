//! Core-independent live flow rates, process aggregation and connection rankings.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One active flow as reported by a telemetry source. `start` is RFC 3339.
#[derive(Clone, Copy)]
pub struct FlowObservation<'a> {
    pub id: &'a str,
    pub start: &'a str,
    pub process: &'a str,
    pub process_path: &'a str,
    pub upload: u64,
    pub download: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionMetrics {
    pub up_speed: u64,
    pub down_speed: u64,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessLiveTraffic {
    pub process_name: String,
    /// Index into this sample's connections, so process rows can reuse the executable path.
    #[serde(skip)]
    pub connection_with_path: Option<usize>,
    pub active_connections: usize,
    pub up_speed: u64,
    pub down_speed: u64,
    pub up_total: u64,
    pub down_total: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionSort {
    #[default]
    Newest,
    Speed,
    Total,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiveTraffic {
    /// Metrics and ranking indices refer to the order of the source sample.
    pub metrics: Vec<ConnectionMetrics>,
    pub processes: Vec<ProcessLiveTraffic>,
    newest: Vec<usize>,
    speed: Vec<usize>,
    total: Vec<usize>,
}

impl LiveTraffic {
    pub fn ordered(&self, sort: ConnectionSort) -> &[usize] {
        match sort {
            ConnectionSort::Newest => &self.newest,
            ConnectionSort::Speed => &self.speed,
            ConnectionSort::Total => &self.total,
        }
    }
}

/// Drop account annotations and paths before grouping traffic or creating a process rule.
pub(crate) fn process_basename<'a>(process: &'a str, path: &'a str) -> &'a str {
    let source = if process.is_empty() { path } else { process };
    let source = source
        .rsplit_once(" (")
        .filter(|(_, suffix)| suffix.ends_with(')'))
        .map_or(source, |(name, _)| name);
    let name = source.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.chars().all(|c| c.is_ascii_digit()) {
        ""
    } else {
        name
    }
}

struct Counters {
    upload: u64,
    download: u64,
    started_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct FlowRateTracker {
    last_sample: Option<Instant>,
    previous: HashMap<(String, String), Counters>,
}

fn bytes_per_sec(current: u64, previous: u64, interval: Duration) -> u64 {
    let bytes = u128::from(current.saturating_sub(previous));
    ((bytes * 1_000_000_000)
        .checked_div(interval.as_nanos())
        .unwrap_or(0))
    .min(u128::from(u64::MAX)) as u64
}

impl FlowRateTracker {
    pub fn observe<'a>(
        &mut self,
        flows: impl IntoIterator<Item = FlowObservation<'a>>,
        at: Instant,
        wall_time: DateTime<Utc>,
    ) -> LiveTraffic {
        let interval = self.last_sample.map(|t| at.saturating_duration_since(t));
        let mut next = HashMap::new();
        let mut live = LiveTraffic::default();
        let mut processes = BTreeMap::<&str, ProcessLiveTraffic>::new();
        let mut starts = Vec::new();
        let mut totals = Vec::new();
        for (index, flow) in flows.into_iter().enumerate() {
            let key = (flow.id.to_owned(), flow.start.to_owned());
            let last = self.previous.remove(&key);
            let started_at = last.as_ref().map(|v| v.started_at).unwrap_or_else(|| {
                DateTime::parse_from_rfc3339(flow.start)
                    .map(|t| t.to_utc())
                    .unwrap_or(wall_time)
            });
            // A decrease in either counter starts a fresh baseline for the whole flow.
            let previous = last
                .as_ref()
                .filter(|v| flow.upload >= v.upload && flow.download >= v.download);
            let (up_speed, down_speed) = previous.zip(interval).map_or((0, 0), |(v, dt)| {
                (
                    bytes_per_sec(flow.upload, v.upload, dt),
                    bytes_per_sec(flow.download, v.download, dt),
                )
            });
            live.metrics.push(ConnectionMetrics {
                up_speed,
                down_speed,
                duration_secs: (wall_time - started_at).num_seconds().max(0) as u64,
            });
            starts.push(started_at);
            totals.push(flow.upload.saturating_add(flow.download));
            next.insert(
                key,
                Counters {
                    upload: flow.upload,
                    download: flow.download,
                    started_at,
                },
            );
            let proc = processes
                .entry(flow.process)
                .or_insert_with(|| ProcessLiveTraffic {
                    process_name: flow.process.into(),
                    ..Default::default()
                });
            if proc.connection_with_path.is_none() && !flow.process_path.is_empty() {
                proc.connection_with_path = Some(index);
            }
            proc.active_connections += 1;
            proc.up_speed = proc.up_speed.saturating_add(up_speed);
            proc.down_speed = proc.down_speed.saturating_add(down_speed);
            proc.up_total = proc.up_total.saturating_add(flow.upload);
            proc.down_total = proc.down_total.saturating_add(flow.download);
        }
        self.previous = next;
        self.last_sample = Some(at);
        live.processes = processes.into_values().collect();
        live.processes
            .sort_by_key(|p| Reverse(p.up_speed.saturating_add(p.down_speed)));
        live.newest = (0..live.metrics.len()).collect();
        live.newest.sort_by_key(|&i| Reverse(starts[i]));
        live.speed = live.newest.clone();
        live.speed.sort_by_key(|&i| {
            Reverse(
                live.metrics[i]
                    .up_speed
                    .saturating_add(live.metrics[i].down_speed),
            )
        });
        live.total = live.newest.clone();
        live.total.sort_by_key(|&i| Reverse(totals[i]));
        live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_elapsed_time_resets_and_only_current_connections() {
        let at = Instant::now();
        let wall = DateTime::parse_from_rfc3339("2025-01-01T00:00:10Z")
            .unwrap()
            .to_utc();
        let a = FlowObservation {
            id: "a",
            start: "2025-01-01T00:00:02Z",
            process: "Reader",
            process_path: "",
            upload: 30,
            download: 100,
        };
        let b = FlowObservation {
            id: "b",
            process_path: "/opt/example/Reader",
            start: "",
            download: 10_000,
            ..a
        };
        let c = FlowObservation {
            id: "c",
            process: "Updater",
            download: 0,
            ..b
        };
        let mut tracker = FlowRateTracker::default();
        let first = tracker.observe([a, b, c], at, wall);
        assert_eq!(
            first
                .processes
                .iter()
                .find(|p| p.process_name == "Reader")
                .unwrap()
                .connection_with_path,
            Some(1)
        );
        assert_eq!(
            first.metrics[0],
            ConnectionMetrics {
                duration_secs: 8,
                ..Default::default()
            }
        );
        let dt = Duration::from_millis(1250);
        let a = FlowObservation { download: 225, ..a };
        let c = FlowObservation { upload: 530, ..c };
        let second = tracker.observe([a, b, c], at + dt, wall + dt);
        assert_eq!(
            (second.metrics[0].down_speed, second.metrics[2].up_speed),
            (100, 400)
        );
        assert_eq!(second.ordered(ConnectionSort::Speed), &[2, 0, 1]);
        assert_eq!(second.ordered(ConnectionSort::Total), &[1, 2, 0]);
        assert_eq!(second.ordered(ConnectionSort::Newest), &[1, 2, 0]);
        assert_eq!(second.processes[0].process_name, "Updater");
        let reader = &second.processes[1];
        assert_eq!(
            (
                reader.active_connections,
                reader.up_total,
                reader.down_total
            ),
            (2, 60, 10_225)
        );
        let reused = FlowObservation {
            start: "2025-01-01T00:00:12Z",
            download: 1_000_000,
            ..a
        };
        let reset = FlowObservation {
            upload: 0,
            download: 20_000,
            ..c
        };
        let third = tracker.observe([reused, reset], at + 2 * dt, wall + 2 * dt);
        assert_eq!(third.metrics[0].down_speed, 0);
        assert_eq!(
            (third.metrics[1].up_speed, third.metrics[1].down_speed),
            (0, 0)
        );
        assert_eq!(
            third
                .processes
                .iter()
                .map(|p| p.active_connections)
                .sum::<usize>(),
            2
        );
        assert!(
            tracker
                .observe([], at + 3 * dt, wall + 3 * dt)
                .processes
                .is_empty()
        );
        assert_eq!(
            tracker.observe([a], at + 4 * dt, wall + 4 * dt).metrics[0].down_speed,
            0
        );
    }
}
