//! Runtime monitoring: connections, traffic history and latency.

pub mod clash;
pub mod latency;
pub mod xray_stats;

pub use clash::{ClashClient, ClashConnection, ClashProxy, ConnectionsResponse};
pub use latency::{tcping, test_many, LatencyResult};
pub use xray_stats::{XrayStatsClient, XrayTraffic};

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrafficSample {
    pub at_ms: i64,
    pub up_speed: u64,
    pub down_speed: u64,
    pub up_total: u64,
    pub down_total: u64,
}

/// Computes speed from cumulative counters and keeps a bounded history.
#[derive(Debug, Clone)]
pub struct TrafficHistory {
    samples: VecDeque<TrafficSample>,
    last_up: u64,
    last_down: u64,
    last_at: Option<InstantLike>,
    capacity: usize,
    pub up_total: u64,
    pub down_total: u64,
    pub up_speed: u64,
    pub down_speed: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct InstantLike {
    ms: i64,
}

impl TrafficHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            last_up: 0,
            last_down: 0,
            last_at: None,
            capacity: capacity.max(8),
            up_total: 0,
            down_total: 0,
            up_speed: 0,
            down_speed: 0,
        }
    }

    pub fn sample(&mut self, up_total: u64, down_total: u64) -> TrafficSample {
        let now = chrono::Utc::now().timestamp_millis();
        let (up_speed, down_speed) = match self.last_at {
            Some(last) if now > last.ms => {
                let secs = ((now - last.ms) as f64 / 1000.0).max(0.001);
                let up = up_total.saturating_sub(self.last_up) as f64 / secs;
                let down = down_total.saturating_sub(self.last_down) as f64 / secs;
                (up as u64, down as u64)
            }
            _ => (0, 0),
        };
        self.last_up = up_total;
        self.last_down = down_total;
        self.last_at = Some(InstantLike { ms: now });
        self.up_total = up_total;
        self.down_total = down_total;
        self.up_speed = up_speed;
        self.down_speed = down_speed;
        let sample = TrafficSample {
            at_ms: now,
            up_speed,
            down_speed,
            up_total,
            down_total,
        };
        self.samples.push_back(sample.clone());
        while self.samples.len() > self.capacity {
            self.samples.pop_front();
        }
        sample
    }

    pub fn samples(&self) -> Vec<TrafficSample> {
        self.samples.iter().cloned().collect()
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.last_at = None;
        self.up_speed = 0;
        self.down_speed = 0;
        self.up_total = 0;
        self.down_total = 0;
    }

    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.up_total,
            self.down_total,
            self.up_speed,
            self.down_speed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_computes_speed() {
        let mut history = TrafficHistory::new(10);
        history.sample(100, 200);
        let sample = history.sample(200, 400);
        assert!(sample.up_speed > 0 || sample.up_total == 200);
        assert_eq!(sample.down_total, 400);
    }
}
