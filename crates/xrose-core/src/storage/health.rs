//! Persistence for node health samples, encrypted reachability cache, and 7-day hourly buckets.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::Store;
use super::crypto::SecretBox;
use crate::error::{Error, Result};
use crate::runtime::health::{
    ErrStage, HourlyBucket, ProbeKind, ProbeSampleRecord, ReachabilityCheckStatus,
    ReachabilityReport, ServiceCheckResult,
};

pub(crate) const HEALTH_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS health_probe_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    target_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    ts INTEGER NOT NULL,
    ok INTEGER NOT NULL,
    rtt_ms INTEGER,
    bps INTEGER,
    bytes_used INTEGER NOT NULL DEFAULT 0,
    err_stage TEXT NOT NULL DEFAULT 'none'
);
CREATE INDEX IF NOT EXISTS health_probe_samples_target ON health_probe_samples(target_key, ts DESC, id DESC);

CREATE TABLE IF NOT EXISTS health_reachability_cache (
    egress_hash TEXT PRIMARY KEY,
    ts INTEGER NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS health_target_reachability (
    target_key TEXT PRIMARY KEY,
    egress_hash TEXT NOT NULL,
    udp_status TEXT NOT NULL DEFAULT 'untested',
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS health_hourly_buckets (
    target_key TEXT NOT NULL,
    hour_bucket INTEGER NOT NULL,
    ok_weighted REAL NOT NULL,
    total_weighted REAL NOT NULL,
    rtt_p50_ms INTEGER,
    rtt_p90_ms INTEGER,
    tput_p75_bps INTEGER,
    PRIMARY KEY (target_key, hour_bucket)
);
CREATE INDEX IF NOT EXISTS health_hourly_buckets_time ON health_hourly_buckets(hour_bucket);
"#;

const BUDGET_KEY: &str = "probe_budget_usage";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BudgetUsage {
    day: String,
    bytes: u64,
}

fn status_str(status: ReachabilityCheckStatus) -> &'static str {
    match status {
        ReachabilityCheckStatus::Pass => "pass",
        ReachabilityCheckStatus::Limited => "limited",
        ReachabilityCheckStatus::Blocked => "blocked",
        ReachabilityCheckStatus::Untested => "untested",
    }
}

fn parse_status(value: &str) -> ReachabilityCheckStatus {
    match value {
        "pass" => ReachabilityCheckStatus::Pass,
        "limited" => ReachabilityCheckStatus::Limited,
        "blocked" => ReachabilityCheckStatus::Blocked,
        _ => ReachabilityCheckStatus::Untested,
    }
}

impl Store {
    pub(crate) fn load_health_snapshot(
        &self,
        now: i64,
    ) -> Result<(
        Vec<ProbeSampleRecord>,
        Vec<HourlyBucket>,
        Vec<(String, ReachabilityReport)>,
    )> {
        let min_bucket = (now.div_euclid(3600) - 168) * 3600;
        let conn = self.conn.lock();

        let mut samples = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT target_key, kind, ts, ok, rtt_ms, bps, bytes_used, err_stage
                 FROM health_probe_samples
                 ORDER BY ts ASC, id ASC",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let kind: String = row.get(1)?;
                let stage: String = row.get(7)?;
                samples.push(ProbeSampleRecord {
                    target_key: row.get(0)?,
                    kind: ProbeKind::parse(&kind),
                    ts: row.get(2)?,
                    ok: row.get::<_, i64>(3)? != 0,
                    rtt_ms: row
                        .get::<_, Option<i64>>(4)?
                        .and_then(|v| u32::try_from(v).ok()),
                    bps: row
                        .get::<_, Option<i64>>(5)?
                        .and_then(|v| u64::try_from(v).ok()),
                    bytes_used: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    err_stage: ErrStage::parse(&stage),
                });
            }
        }

        let mut buckets = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT target_key, hour_bucket, ok_weighted, total_weighted, rtt_p50_ms, rtt_p90_ms, tput_p75_bps
                 FROM health_hourly_buckets
                 WHERE hour_bucket >= ?1",
            )?;
            let mut rows = stmt.query([min_bucket])?;
            while let Some(row) = rows.next()? {
                buckets.push(HourlyBucket {
                    target_key: row.get(0)?,
                    hour_bucket: row.get(1)?,
                    ok_weighted: row.get(2)?,
                    total_weighted: row.get(3)?,
                    rtt_p50_ms: row
                        .get::<_, Option<i64>>(4)?
                        .and_then(|v| u32::try_from(v).ok()),
                    rtt_p90_ms: row
                        .get::<_, Option<i64>>(5)?
                        .and_then(|v| u32::try_from(v).ok()),
                    tput_p75_bps: row
                        .get::<_, Option<i64>>(6)?
                        .and_then(|v| u64::try_from(v).ok()),
                });
            }
        }

        let mut reachability_rows = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT t.target_key, t.udp_status, c.payload
                 FROM health_target_reachability t
                 JOIN health_reachability_cache c ON c.egress_hash = t.egress_hash",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                reachability_rows.push((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ));
            }
        }
        drop(conn);

        let mut reachability = Vec::new();
        for (target_key, udp_str, payload) in reachability_rows {
            if !SecretBox::is_encrypted(&payload) {
                continue;
            }
            if let Ok(json) = self.secrets.decrypt(&payload)
                && let Ok(mut report) = serde_json::from_str::<ReachabilityReport>(&json)
            {
                let udp_status = parse_status(&udp_str);
                if udp_status != ReachabilityCheckStatus::Untested {
                    report.udp_stun = ServiceCheckResult::new(udp_status);
                }
                reachability.push((target_key, report));
            }
        }

        Ok((samples, buckets, reachability))
    }

    pub(crate) fn record_health_probe(&self, sample: &ProbeSampleRecord) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO health_probe_samples (target_key, kind, ts, ok, rtt_ms, bps, bytes_used, err_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                sample.target_key,
                sample.kind.as_str(),
                sample.ts,
                i64::from(sample.ok),
                sample.rtt_ms.map(i64::from),
                sample.bps.and_then(|v| i64::try_from(v).ok()),
                i64::try_from(sample.bytes_used).unwrap_or(i64::MAX),
                sample.err_stage.as_str(),
            ],
        )?;
        tx.execute(
            "DELETE FROM health_probe_samples
             WHERE target_key = ?1
               AND id NOT IN (
                   SELECT id FROM health_probe_samples
                   WHERE target_key = ?1
                   ORDER BY ts DESC, id DESC
                   LIMIT 50
               )",
            [&sample.target_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn save_hourly_buckets(
        &self,
        buckets: &[HourlyBucket],
        min_bucket: i64,
    ) -> Result<()> {
        if buckets.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        for bucket in buckets {
            tx.execute(
                "INSERT INTO health_hourly_buckets
                 (target_key, hour_bucket, ok_weighted, total_weighted, rtt_p50_ms, rtt_p90_ms, tput_p75_bps)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(target_key, hour_bucket) DO UPDATE SET
                     ok_weighted = excluded.ok_weighted,
                     total_weighted = excluded.total_weighted,
                     rtt_p50_ms = excluded.rtt_p50_ms,
                     rtt_p90_ms = excluded.rtt_p90_ms,
                     tput_p75_bps = excluded.tput_p75_bps",
                params![
                    bucket.target_key,
                    bucket.hour_bucket,
                    bucket.ok_weighted,
                    bucket.total_weighted,
                    bucket.rtt_p50_ms.map(i64::from),
                    bucket.rtt_p90_ms.map(i64::from),
                    bucket.tput_p75_bps.and_then(|v| i64::try_from(v).ok()),
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM health_hourly_buckets WHERE hour_bucket < ?1",
            [min_bucket],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn get_cached_reachability(
        &self,
        egress_ip: &str,
        max_age_secs: i64,
        now: i64,
    ) -> Result<Option<ReachabilityReport>> {
        let egress_hash = self.secrets.keyed_hash("egress-v1", egress_ip);
        let min_ts = now.saturating_sub(max_age_secs);
        let payload: Option<String> = {
            let conn = self.conn.lock();
            conn.query_row(
                "SELECT payload FROM health_reachability_cache WHERE egress_hash = ?1 AND ts >= ?2",
                params![egress_hash, min_ts],
                |row| row.get(0),
            )
            .optional()?
        };
        let Some(payload) = payload else {
            return Ok(None);
        };
        if !SecretBox::is_encrypted(&payload) {
            return Ok(None);
        }
        let json = self.secrets.decrypt(&payload)?;
        Ok(serde_json::from_str(&json).ok())
    }

    pub(crate) fn save_reachability(
        &self,
        target_key: &str,
        egress_ip: Option<&str>,
        report: &ReachabilityReport,
    ) -> Result<()> {
        let egress_hash = match egress_ip {
            Some(ip) if !ip.trim().is_empty() => self.secrets.keyed_hash("egress-v1", ip.trim()),
            _ => self.secrets.keyed_hash("target-v1", target_key),
        };
        let mut stored_report = report.clone();
        // Per-node UDP capability lives on `health_target_reachability`.
        stored_report.udp_stun = ServiceCheckResult::default();
        let encrypted = self
            .secrets
            .encrypt(&serde_json::to_string(&stored_report)?);
        if !SecretBox::is_encrypted(&encrypted) {
            return Err(Error::other("可达性检测结果加密失败，请重试"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO health_reachability_cache (egress_hash, ts, payload)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(egress_hash) DO UPDATE SET ts = excluded.ts, payload = excluded.payload",
            params![egress_hash, report.checked_at, encrypted],
        )?;
        tx.execute(
            "INSERT INTO health_target_reachability (target_key, egress_hash, udp_status, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(target_key) DO UPDATE SET
                 egress_hash = excluded.egress_hash,
                 udp_status = excluded.udp_status,
                 updated_at = excluded.updated_at",
            params![
                target_key,
                egress_hash,
                status_str(report.udp_stun.status),
                report.checked_at,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn remove_health_targets(&self, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        for key in keys {
            tx.execute(
                "DELETE FROM health_probe_samples WHERE target_key = ?1",
                [key],
            )?;
            tx.execute(
                "DELETE FROM health_target_reachability WHERE target_key = ?1",
                [key],
            )?;
            tx.execute(
                "DELETE FROM health_hourly_buckets WHERE target_key = ?1",
                [key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn clear_health_data(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM health_probe_samples;
             DELETE FROM health_target_reachability;
             DELETE FROM health_reachability_cache;
             DELETE FROM health_hourly_buckets;",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn probe_budget_usage(&self, day: &str) -> u64 {
        let conn = self.conn.lock();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                [BUDGET_KEY],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        raw.and_then(|text| serde_json::from_str::<BudgetUsage>(&text).ok())
            .filter(|usage| usage.day == day)
            .map(|usage| usage.bytes)
            .unwrap_or(0)
    }

    pub(crate) fn add_probe_budget_usage(&self, day: &str, bytes: u64) -> u64 {
        let conn = self.conn.lock();
        let current = conn
            .query_row(
                "SELECT value FROM app_state WHERE key = ?1",
                [BUDGET_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<BudgetUsage>(&text).ok())
            .filter(|usage| usage.day == day)
            .map(|usage| usage.bytes)
            .unwrap_or(0);
        let next = current.saturating_add(bytes);
        let usage = BudgetUsage {
            day: day.to_owned(),
            bytes: next,
        };
        if let Ok(json) = serde_json::to_string(&usage) {
            let _ = conn.execute(
                "INSERT INTO app_state (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![BUDGET_KEY, json],
            );
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;
    use crate::util::now_unix;

    fn open_store() -> (std::sync::Arc<Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        (Store::open(&paths).unwrap(), dir)
    }

    #[test]
    fn health_samples_prune_to_latest_50_and_delete_cleanly() {
        let (store, _dir) = open_store();
        let now = now_unix();
        for i in 0..65 {
            store
                .record_health_probe(&ProbeSampleRecord {
                    target_key: "node-a".into(),
                    kind: ProbeKind::Http,
                    ts: now + i,
                    ok: true,
                    rtt_ms: Some(90),
                    bps: None,
                    bytes_used: 512,
                    err_stage: ErrStage::None,
                })
                .unwrap();
        }
        let (samples, _, _) = store.load_health_snapshot(now + 100).unwrap();
        assert_eq!(samples.len(), 50);
        assert_eq!(samples[0].ts, now + 15);
        store.remove_health_targets(&["node-a".into()]).unwrap();
        let (samples, _, _) = store.load_health_snapshot(now + 100).unwrap();
        assert!(samples.is_empty());
    }

    #[test]
    fn reachability_cache_encrypts_egress_ip_and_region_and_shares_across_nodes() {
        let (store, _dir) = open_store();
        let now = now_unix();
        let report = ReachabilityReport {
            checked_at: now,
            egress_region: Some("JP".into()),
            chatgpt: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            udp_stun: ServiceCheckResult::new(ReachabilityCheckStatus::Pass),
            ..Default::default()
        };
        store
            .save_reachability("node-1", Some("198.51.100.24"), &report)
            .unwrap();
        let raw_payload: String = store
            .conn
            .lock()
            .query_row(
                "SELECT payload FROM health_reachability_cache LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let raw_hash: String = store
            .conn
            .lock()
            .query_row(
                "SELECT egress_hash FROM health_reachability_cache LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(SecretBox::is_encrypted(&raw_payload));
        assert!(!raw_payload.contains("egress_region"));
        assert!(!raw_payload.contains("\"JP\""));
        assert!(!raw_hash.contains("198.51.100.24"));

        let cached = store
            .get_cached_reachability("198.51.100.24", 6 * 3600, now + 60)
            .unwrap()
            .unwrap();
        assert_eq!(cached.egress_region.as_deref(), Some("JP"));
        assert_eq!(cached.chatgpt.status, ReachabilityCheckStatus::Pass);
        // UDP status is tracked per node, not shared via egress cache.
        assert_eq!(cached.udp_stun.status, ReachabilityCheckStatus::Untested);

        let (_, _, loaded) = store.load_health_snapshot(now + 60).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, "node-1");
        assert_eq!(loaded[0].1.udp_stun.status, ReachabilityCheckStatus::Pass);
    }

    #[test]
    fn v4_databases_migrate_to_v5_with_health_tables() {
        let (store, dir) = open_store();
        let paths = AppPaths::from_root(dir.path());
        {
            let conn = store.conn.lock();
            conn.execute_batch(
                "DROP TABLE IF EXISTS health_probe_samples;
                 DROP TABLE IF EXISTS health_reachability_cache;
                 DROP TABLE IF EXISTS health_target_reachability;
                 DROP TABLE IF EXISTS health_hourly_buckets;",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }
        drop(store);
        let reopened = Store::open(&paths).unwrap();
        let version: i64 = reopened
            .conn
            .lock()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, crate::storage::SCHEMA_VERSION);
        assert!(reopened.load_health_snapshot(now_unix()).is_ok());
    }
}
