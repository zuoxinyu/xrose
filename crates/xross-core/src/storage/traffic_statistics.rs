//! Encrypted entity metadata and idempotent daily traffic aggregates.

use chrono::NaiveDate;
use rusqlite::{Transaction, params};

use super::Store;
use crate::error::{Error, Result};
use crate::model::CoreKind;
use crate::storage::crypto::SecretBox;
use crate::traffic_statistics::{TrafficBucket, TrafficBytes, TrafficQuery, TrafficWindow};

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS traffic_sessions (id TEXT PRIMARY KEY, started_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS traffic_entities (id TEXT PRIMARY KEY, payload TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS traffic_daily (
            session_id TEXT NOT NULL REFERENCES traffic_sessions(id),
            day TEXT NOT NULL,
            core TEXT NOT NULL,
            dimension TEXT NOT NULL,
            entity_id TEXT NOT NULL REFERENCES traffic_entities(id),
            upload INTEGER NOT NULL CHECK(upload >= 0),
            download INTEGER NOT NULL CHECK(download >= 0),
            sampled_at INTEGER NOT NULL,
            has_gaps INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (session_id, day, entity_id)
        );
        CREATE INDEX IF NOT EXISTS traffic_daily_range ON traffic_daily(dimension, day);
    ",
    )?;
    Ok(())
}

impl Store {
    pub(crate) fn write_traffic_statistics(
        &self,
        session: &str,
        started_at: i64,
        buckets: &[TrafficBucket],
    ) -> Result<()> {
        if buckets.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO traffic_sessions(id, started_at) VALUES (?1, ?2)",
            params![session, started_at],
        )?;
        for bucket in buckets {
            let encrypted = self
                .secrets
                .encrypt(&serde_json::to_string(&bucket.entity)?);
            if !SecretBox::is_encrypted(&encrypted) {
                return Err(Error::other("统计信息加密失败，请重试保存"));
            }
            let upload = i64::try_from(bucket.bytes.upload)
                .map_err(|_| Error::other("统计数值超出保存范围"))?;
            let download = i64::try_from(bucket.bytes.download)
                .map_err(|_| Error::other("统计数值超出保存范围"))?;
            tx.execute("INSERT INTO traffic_entities(id,payload) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload", params![bucket.entity_id, encrypted])?;
            tx.execute("INSERT INTO traffic_daily(session_id,day,core,dimension,entity_id,upload,download,sampled_at,has_gaps)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                ON CONFLICT(session_id,day,entity_id) DO UPDATE SET upload=excluded.upload,download=excluded.download,sampled_at=excluded.sampled_at,has_gaps=excluded.has_gaps",
                params![bucket.session,bucket.date.to_string(),bucket.core.as_str(),bucket.dimension,bucket.entity_id,upload,download,bucket.last_sample_at,bucket.has_gaps])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn read_traffic_statistics(
        &self,
        query: &TrafficQuery,
        session: &str,
        today: NaiveDate,
    ) -> Result<Vec<TrafficBucket>> {
        let (start, end) = query.range(today);
        let conn = self.conn.lock();
        let sql = if query.window == TrafficWindow::Session {
            "SELECT d.session_id,d.day,d.core,d.dimension,d.entity_id,e.payload,d.upload,d.download,d.sampled_at,d.has_gaps
             FROM traffic_daily d JOIN traffic_entities e ON e.id=d.entity_id
             WHERE d.session_id=?1 AND d.dimension IN (?4,'summary')"
        } else {
            "SELECT d.session_id,d.day,d.core,d.dimension,d.entity_id,e.payload,d.upload,d.download,d.sampled_at,d.has_gaps
             FROM traffic_daily d JOIN traffic_entities e ON e.id=d.entity_id
             WHERE d.day>=?2 AND d.day<=?3 AND d.dimension IN (?4,'summary')"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(params![
            session,
            start.to_string(),
            end.to_string(),
            query.dimension.key()
        ])?;
        let mut buckets = Vec::new();
        while let Some(row) = rows.next()? {
            let day: String = row.get(1)?;
            let core: String = row.get(2)?;
            let payload: String = row.get(5)?;
            if !SecretBox::is_encrypted(&payload) {
                return Err(Error::other("统计记录未加密，请恢复有效备份或清空统计"));
            }
            buckets.push(TrafficBucket {
                session: row.get(0)?,
                date: day
                    .parse()
                    .map_err(|_| Error::other("统计日期无效，请恢复有效备份或清空统计"))?,
                core: match core.as_str() {
                    "sing-box" => CoreKind::SingBox,
                    "xray" => CoreKind::Xray,
                    _ => return Err(Error::other("统计记录的内核类型无效")),
                },
                dimension: row.get(3)?,
                entity_id: row.get(4)?,
                entity: serde_json::from_str(&self.secrets.decrypt(&payload)?)?,
                bytes: TrafficBytes {
                    upload: row
                        .get::<_, i64>(6)?
                        .try_into()
                        .map_err(|_| Error::other("统计计数无效"))?,
                    download: row
                        .get::<_, i64>(7)?
                        .try_into()
                        .map_err(|_| Error::other("统计计数无效"))?,
                },
                last_sample_at: row.get(8)?,
                has_gaps: row.get(9)?,
            });
        }
        Ok(buckets)
    }

    pub(crate) fn clear_traffic_statistics(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute_batch("DELETE FROM traffic_daily; DELETE FROM traffic_entities; DELETE FROM traffic_sessions;")?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{ClashConnection, ConnectionsResponse};
    use crate::{
        paths::AppPaths,
        traffic_statistics::{CoreEpoch, TrafficAccumulator, TrafficDimension, build_view},
    };
    #[test]
    fn encrypted_roundtrip_retry_history_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let store = Store::open(&paths).unwrap();
        let mut a = TrafficAccumulator::new(true);
        let day: NaiveDate = "2026-11-06".parse().unwrap();
        let mut conn = ClashConnection {
            id: "fixture".into(),
            upload: 123,
            ..Default::default()
        };
        conn.metadata.host = "api.example.com".into();
        let response = ConnectionsResponse {
            upload_total: 123,
            connections: vec![conn],
            ..Default::default()
        };
        let epoch = CoreEpoch {
            core: CoreKind::SingBox,
            pid: Some(1),
            started_at: Some(1),
            restarts: 0,
            helper_session: None,
        };
        a.observe_singbox(epoch, &Default::default(), &response, day, 1);
        let rows: Vec<_> = a.buckets.values().cloned().collect();
        for _ in 0..2 {
            store
                .write_traffic_statistics(&a.session, a.started_at, &rows)
                .unwrap();
        }
        let plaintext: String = store
            .conn
            .lock()
            .query_row("SELECT payload FROM traffic_entities LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(SecretBox::is_encrypted(&plaintext));
        assert!(!plaintext.contains("example.com"));
        drop(store);
        let store = Store::open(&paths).unwrap();
        let session_rows = store
            .read_traffic_statistics(
                &TrafficQuery::default(),
                &a.session,
                day - chrono::Days::new(1),
            )
            .unwrap();
        assert_eq!(
            build_view(
                TrafficQuery::default(),
                &a.session,
                session_rows,
                day - chrono::Days::new(1)
            )
            .total
            .upload,
            123
        );
        let query = TrafficQuery {
            dimension: TrafficDimension::Domain,
            window: TrafficWindow::Day,
            date: Some(day),
            ..Default::default()
        };
        let loaded = store
            .read_traffic_statistics(&query, "another-session", day)
            .unwrap();
        let view = build_view(query.clone(), "another-session", loaded, day);
        assert_eq!(view.total.upload, 123);
        assert_eq!(view.rows[0].entity.label, "example.com");
        store.clear_traffic_statistics().unwrap();
        assert!(
            store
                .read_traffic_statistics(&query, &a.session, day)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn version_seven_migration_preserves_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        let store = Store::open(&paths).unwrap();
        let profiles = store.profiles().unwrap();
        store.conn.lock().execute_batch("DROP TABLE traffic_daily; DROP TABLE traffic_entities; DROP TABLE traffic_sessions; PRAGMA user_version=7;").unwrap();
        drop(store);
        let store = Store::open(&paths).unwrap();
        assert_eq!(store.profiles().unwrap(), profiles);
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            8
        );
    }
    #[test]
    fn failed_batch_rolls_back_and_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&AppPaths::from_root(dir.path())).unwrap();
        let day: NaiveDate = "2026-11-06".parse().unwrap();
        let row = TrafficBucket {
            session: "fixture-session".into(),
            date: day,
            core: CoreKind::SingBox,
            dimension: "summary".into(),
            entity_id: "fixture-entity".into(),
            entity: crate::traffic_statistics::TrafficEntity {
                kind: crate::traffic_statistics::TrafficEntityKind::Summary,
                key: String::new(),
                label: String::new(),
            },
            bytes: TrafficBytes {
                upload: 10,
                download: 20,
            },
            last_sample_at: 1,
            has_gaps: false,
        };
        store.conn.lock().execute_batch("CREATE TRIGGER fail_traffic BEFORE INSERT ON traffic_daily BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        assert!(
            store
                .write_traffic_statistics(&row.session, 1, std::slice::from_ref(&row))
                .is_err()
        );
        assert_eq!(
            store
                .conn
                .lock()
                .query_row("SELECT COUNT(*) FROM traffic_entities", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER fail_traffic;")
            .unwrap();
        store
            .write_traffic_statistics(&row.session, 1, std::slice::from_ref(&row))
            .unwrap();
        store
            .write_traffic_statistics(&row.session, 1, std::slice::from_ref(&row))
            .unwrap();
        assert_eq!(
            store
                .read_traffic_statistics(&TrafficQuery::default(), &row.session, day)
                .unwrap()[0]
                .bytes,
            row.bytes
        );
    }
}
