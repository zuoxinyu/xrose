//! SQLite persistence with migrations, encrypted payloads and JSON backups.

pub mod crypto;
mod mesh_sync;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_context};
use crate::model::AppModel;
use crate::paths::AppPaths;
use crate::subscription::Subscription;
use crate::util::{atomic_write_json, now_rfc3339, now_unix};

use crypto::SecretBox;

pub const SCHEMA_VERSION: i64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredProfile {
    pub id: String,
    pub name: String,
    pub model: AppModel,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LatencyEntry {
    pub key: String,
    pub delay_ms: Option<u32>,
    pub checked_at: i64,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupBundle {
    pub format: u32,
    pub exported_at: String,
    pub profiles: Vec<BackupProfile>,
    pub subscriptions: Vec<BackupSubscription>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupProfile {
    pub id: String,
    pub name: String,
    pub model_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSubscription {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

pub struct Store {
    conn: Mutex<Connection>,
    secrets: SecretBox,
    db_path: PathBuf,
    backup_dir: PathBuf,
}

impl Store {
    pub fn open(paths: &AppPaths) -> Result<Arc<Self>> {
        paths.ensure()?;
        let db_path = paths.database();
        let secrets = SecretBox::load_or_create(&paths.secrets_key())?;
        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let store = Self {
            conn: Mutex::new(conn),
            secrets,
            db_path,
            backup_dir: paths.backup_dir(),
        };
        store.migrate()?;
        store.seed_default_profile()?;
        Ok(Arc::new(store))
    }

    fn backup_before_migration(&self, version: i64) -> Result<()> {
        std::fs::create_dir_all(&self.backup_dir).map_err(|e| io_context(&self.backup_dir, e))?;
        let target = self
            .backup_dir
            .join(format!("xross-v{version}-{}.db", now_unix()));
        std::fs::copy(&self.db_path, &target).map_err(|e| io_context(&target, e))?;
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        if version > 0 && self.db_path.exists() {
            let _ = self.backup_before_migration(version);
        }
        let tx = conn.transaction()?;
        if version < 1 {
            tx.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS profiles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    model TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS subscriptions (
                    id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    sort INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS app_state (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                "#,
            )?;
        }
        if version < 2 {
            tx.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS latency (
                    key TEXT PRIMARY KEY,
                    delay_ms INTEGER,
                    checked_at INTEGER NOT NULL,
                    detail TEXT
                );
                "#,
            )?;
        }
        if version < 3 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS mesh_profiles (id TEXT PRIMARY KEY, payload TEXT NOT NULL, updated_at INTEGER NOT NULL);")?;
        }
        if version < 4 {
            // The old UI persisted a literal default UA for every subscription.
            // Empty now means inherit. Preserve custom agents and unknown fields.
            let mut stmt = tx.prepare("SELECT id, payload FROM subscriptions")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            for (id, payload) in rows {
                let mut sub: serde_json::Value =
                    serde_json::from_str(&self.secrets.decrypt(&payload)?)?;
                if sub["user_agent"] == crate::subscription::DEFAULT_USER_AGENT {
                    sub["user_agent"] = "".into();
                    let encrypted = self.secrets.encrypt(&serde_json::to_string(&sub)?);
                    if !SecretBox::is_encrypted(&encrypted) {
                        return Err(Error::other("订阅加密失败，请重试迁移"));
                    }
                    tx.execute(
                        "UPDATE subscriptions SET payload = ?1 WHERE id = ?2",
                        params![encrypted, id],
                    )?;
                }
            }
        }
        if version < 5 {
            // Patch only missing group policies, preserving unknown fields in encrypted profiles.
            let mut stmt = tx.prepare("SELECT id, model FROM profiles")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            let default = serde_json::to_value(crate::model::NodeSwitchPolicy::default())?;
            for (id, payload) in rows {
                let mut model: serde_json::Value =
                    serde_json::from_str(&self.secrets.decrypt(&payload)?)?;
                let mut changed = false;
                for group in model["groups"].as_array_mut().into_iter().flatten() {
                    if group.get("switch_policy").is_none() {
                        group["switch_policy"] = default.clone();
                        changed = true;
                    }
                }
                if changed {
                    let encrypted = self.secrets.encrypt(&serde_json::to_string(&model)?);
                    if !SecretBox::is_encrypted(&encrypted) {
                        return Err(Error::other("配置加密失败，请重试迁移"));
                    }
                    tx.execute(
                        "UPDATE profiles SET model = ?1 WHERE id = ?2",
                        params![encrypted, id],
                    )?;
                }
            }
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    fn seed_default_profile(&self) -> Result<()> {
        // Never call another method that locks the connection while holding it:
        // parking_lot mutexes are not reentrant.
        let (count, active, first_id): (i64, Option<String>, Option<String>) = {
            let conn = self.conn.lock();
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM profiles", [], |row| row.get(0))?;
            let active: Option<String> = conn
                .query_row(
                    "SELECT value FROM app_state WHERE key = 'active_profile'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let first_id: Option<String> = conn
                .query_row(
                    "SELECT id FROM profiles ORDER BY created_at LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            (count, active, first_id)
        };
        if count == 0 {
            let id = self.save_profile(&AppModel::default())?;
            self.set_active_profile(&id)?;
        } else if active.is_none() {
            if let Some(id) = first_id {
                self.set_active_profile(&id)?;
            }
        }
        Ok(())
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn mesh_profiles(&self) -> Result<Vec<crate::mesh::MeshProfile>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT payload FROM mesh_profiles ORDER BY updated_at, id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut profiles = Vec::new();
        for row in rows {
            profiles.push(serde_json::from_str(&self.secrets.decrypt(&row?)?)?);
        }
        Ok(profiles)
    }

    pub fn save_mesh_profile(&self, profile: &crate::mesh::MeshProfile) -> Result<()> {
        profile.validate()?;
        let payload = self.secrets.encrypt(&serde_json::to_string(profile)?);
        self.conn.lock().execute("INSERT INTO mesh_profiles (id,payload,updated_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at", params![profile.id, payload, now_unix()])?;
        Ok(())
    }

    /// Publish the gateway and the selector choice together after the worker confirms the exit.
    pub(crate) fn save_mesh_exit_selection(
        &self,
        profile: &crate::mesh::MeshProfile,
        model: &AppModel,
        active_id: &str,
    ) -> Result<()> {
        profile.validate()?;
        let mesh = self.secrets.encrypt(&serde_json::to_string(profile)?);
        let proxy = self.secrets.encrypt(&serde_json::to_string(model)?);
        if !SecretBox::is_encrypted(&mesh) || !SecretBox::is_encrypted(&proxy) {
            return Err(Error::other("出口配置加密失败，请重试"));
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let changed = tx.execute("UPDATE profiles SET model = ?1, updated_at = ?2 WHERE id = ?3 AND id = (SELECT value FROM app_state WHERE key = 'active_profile')", params![proxy, now_unix(), active_id])?;
        if changed != 1
            || tx.execute(
                "UPDATE mesh_profiles SET payload = ?1, updated_at = ?2 WHERE id = ?3",
                params![mesh, now_unix(), profile.id],
            )? != 1
        {
            return Err(Error::other("当前配置已更改，请重新选择出口"));
        }
        tx.execute(
            "DELETE FROM latency WHERE key = ?1",
            [format!("mesh-exit-{}", profile.id)],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_mesh_profile(&self, id: &str) -> Result<()> {
        self.conn
            .lock()
            .execute("DELETE FROM mesh_profiles WHERE id=?1", [id])?;
        Ok(())
    }

    // -- profiles ---------------------------------------------------------

    pub fn profiles(&self) -> Result<Vec<ProfileMeta>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT id, name, created_at, updated_at FROM profiles ORDER BY created_at")?;
        let rows = stmt.query_map([], |row| {
            Ok(ProfileMeta {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn load_profile(&self, id: &str) -> Result<Option<StoredProfile>> {
        let conn = self.conn.lock();
        let payload: Option<(String, String, i64, i64)> = conn
            .query_row(
                "SELECT model, name, created_at, updated_at FROM profiles WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        drop(conn);
        let Some((payload, name, created_at, updated_at)) = payload else {
            return Ok(None);
        };
        let json = self.secrets.decrypt(&payload)?;
        let model: AppModel = serde_json::from_str(&json)?;
        Ok(Some(StoredProfile {
            id: id.to_string(),
            name,
            model,
            created_at,
            updated_at,
        }))
    }

    pub fn save_profile(&self, model: &AppModel) -> Result<String> {
        let id = crate::util::new_id("profile");
        self.save_profile_with_id(&id, model)?;
        Ok(id)
    }

    pub fn save_profile_with_id(&self, id: &str, model: &AppModel) -> Result<()> {
        let id = if id.trim().is_empty() {
            crate::util::new_id("profile")
        } else {
            id.to_string()
        };
        let json = serde_json::to_string(model)?;
        let encrypted = self.secrets.encrypt(&json);
        let conn = self.conn.lock();
        conn.execute(
            r#"INSERT INTO profiles (id, name, model, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?4)
               ON CONFLICT(id) DO UPDATE SET name = ?2, model = ?3, updated_at = ?4"#,
            params![id, model.name, encrypted, now_unix()],
        )?;
        Ok(())
    }

    /// Persists the currently active profile, creating one if needed.
    pub fn save_active_profile(&self, model: &AppModel) -> Result<String> {
        if let Some(id) = self.active_profile_id() {
            self.save_profile_with_id(&id, model)?;
            Ok(id)
        } else {
            let id = self.save_profile(model)?;
            self.set_active_profile(&id)?;
            Ok(id)
        }
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM profiles WHERE id = ?1", [id])?;
        drop(conn);
        if self.active_profile_id().as_deref() == Some(id) {
            if let Some(next) = self.profiles()?.first() {
                self.set_active_profile(&next.id)?;
            } else {
                let conn = self.conn.lock();
                conn.execute("DELETE FROM app_state WHERE key = 'active_profile'", [])?;
            }
        }
        Ok(())
    }

    pub fn active_profile_id(&self) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT value FROM app_state WHERE key = 'active_profile'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_active_profile(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('active_profile', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            [id],
        )?;
        Ok(())
    }

    // -- subscriptions ----------------------------------------------------

    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT payload FROM subscriptions ORDER BY sort, rowid")?;
        let payloads: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        let mut result = Vec::new();
        for payload in payloads {
            let json = self.secrets.decrypt(&payload)?;
            if let Ok(sub) = serde_json::from_str::<Subscription>(&json) {
                result.push(sub);
            }
        }
        Ok(result)
    }

    pub fn save_subscription(&self, subscription: &Subscription) -> Result<()> {
        let encrypted = self.secrets.encrypt(&serde_json::to_string(subscription)?);
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO subscriptions (id, payload, sort) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET payload = ?2, sort = ?3",
            params![subscription.id, encrypted, subscription.sort],
        )?;
        Ok(())
    }

    pub fn delete_subscription(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM subscriptions WHERE id = ?1", [id])?;
        Ok(())
    }

    // -- latency cache ----------------------------------------------------

    pub fn latency_cache(&self) -> Result<Vec<LatencyEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT key, delay_ms, checked_at, detail FROM latency")?;
        let rows = stmt.query_map([], |row| {
            Ok(LatencyEntry {
                key: row.get(0)?,
                delay_ms: row.get(1)?,
                checked_at: row.get(2)?,
                detail: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_latency(&self, entry: &LatencyEntry) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO latency (key, delay_ms, checked_at, detail) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET delay_ms = ?2, checked_at = ?3, detail = ?4",
            params![entry.key, entry.delay_ms, entry.checked_at, entry.detail],
        )?;
        Ok(())
    }

    pub fn clear_latency(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM latency", [])?;
        Ok(())
    }

    pub fn remove_latency(&self, key: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM latency WHERE key = ?1", [key])?;
        Ok(())
    }

    // -- backup / export --------------------------------------------------

    /// Copies the SQLite database (including WAL) to the backup directory.
    pub fn backup_database(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.backup_dir).map_err(|e| io_context(&self.backup_dir, e))?;
        let target = self.backup_dir.join(format!(
            "xross-{}.db",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        {
            let conn = self.conn.lock();
            conn.execute("PRAGMA wal_checkpoint(FULL)", []).ok();
        }
        std::fs::copy(&self.db_path, &target).map_err(|e| io_context(&target, e))?;
        Ok(target)
    }

    /// Human readable export with decrypted contents (for migration/backup).
    pub fn export_json(&self, path: &Path) -> Result<()> {
        let mut profiles = Vec::new();
        for meta in self.profiles()? {
            if let Some(profile) = self.load_profile(&meta.id)? {
                profiles.push(BackupProfile {
                    id: profile.id,
                    name: profile.name,
                    model_json: serde_json::to_value(&profile.model)?,
                });
            }
        }
        let subscriptions = self
            .subscriptions()?
            .into_iter()
            .map(|sub| BackupSubscription {
                name: sub.name,
                url: sub.url,
                user_agent: sub.user_agent,
                headers: sub.headers,
            })
            .collect();
        let bundle = BackupBundle {
            format: 1,
            exported_at: now_rfc3339(),
            profiles,
            subscriptions,
        };
        atomic_write_json(path, &bundle)
    }

    /// Imports profiles/subscriptions from an export file. Existing ids are kept
    /// when free; collisions get a fresh id.
    pub fn import_json(&self, path: &Path) -> Result<usize> {
        let data = std::fs::read(path).map_err(|e| io_context(path, e))?;
        let bundle: BackupBundle = serde_json::from_slice(&data)?;
        if bundle.format != 1 {
            return Err(Error::other(format!("不支持的备份格式：{}", bundle.format)));
        }
        let mut imported = 0;
        for profile in bundle.profiles {
            let model: AppModel = serde_json::from_value(profile.model_json)?;
            let id = if self.load_profile(&profile.id)?.is_some() {
                crate::util::new_id("profile")
            } else {
                profile.id
            };
            self.save_profile_with_id(&id, &model)?;
            imported += 1;
        }
        for sub in bundle.subscriptions {
            let mut s = Subscription::new(sub.name, sub.url);
            if sub.user_agent != crate::subscription::DEFAULT_USER_AGENT {
                s.user_agent = sub.user_agent;
            }
            s.headers = sub.headers;
            self.save_subscription(&s)?;
            imported += 1;
        }
        Ok(imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Arc<Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        (Store::open(&paths).unwrap(), dir)
    }

    #[test]
    fn seeds_default_profile() {
        let (store, _dir) = store();
        let profiles = store.profiles().unwrap();
        assert_eq!(profiles.len(), 1);
        assert!(store.active_profile_id().is_some());
        let loaded = store.load_profile(&profiles[0].id).unwrap().unwrap();
        assert_eq!(loaded.model.name, profiles[0].name);
    }

    #[test]
    fn profiles_are_encrypted_on_disk() {
        let (store, _dir) = store();
        let mut model = AppModel::default();
        let mut node =
            crate::model::Node::new("secret", crate::model::Protocol::Vless, "example.com", 443);
        node.credentials.uuid = Some("super-secret-uuid".into());
        model.add_nodes(vec![node]);
        let id = store.save_profile(&model).unwrap();
        let conn = Connection::open(store.db_path()).unwrap();
        let raw: String = conn
            .query_row("SELECT model FROM profiles WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!raw.contains("super-secret-uuid"));
        assert!(SecretBox::is_encrypted(&raw));
        let loaded = store.load_profile(&id).unwrap().unwrap();
        assert_eq!(
            loaded.model.nodes[0].credentials.uuid.as_deref(),
            Some("super-secret-uuid")
        );
    }

    #[test]
    fn subscriptions_roundtrip() {
        let (store, _dir) = store();
        let mut sub = Subscription::new("机场", "https://example.com/sub?token=abc");
        sub.node_count = 12;
        store.save_subscription(&sub).unwrap();
        let list = store.subscriptions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "机场");
        store.delete_subscription(&sub.id).unwrap();
        assert!(store.subscriptions().unwrap().is_empty());
    }

    #[test]
    fn latency_cache_roundtrip() {
        let (store, _dir) = store();
        store
            .set_latency(&LatencyEntry {
                key: "node-a".into(),
                delay_ms: Some(120),
                checked_at: now_unix(),
                detail: None,
            })
            .unwrap();
        assert_eq!(store.latency_cache().unwrap()[0].delay_ms, Some(120));
        store.clear_latency().unwrap();
        assert!(store.latency_cache().unwrap().is_empty());
    }

    #[test]
    fn backup_and_export() {
        let (store, dir) = store();
        store
            .save_subscription(&crate::subscription::Subscription::new(
                "backup-sub",
                "https://example.com/sub",
            ))
            .unwrap();
        let db_backup = store.backup_database().unwrap();
        assert!(db_backup.exists());
        let export = dir.path().join("export.json");
        store.export_json(&export).unwrap();
        assert!(export.exists());
        let imported = store.import_json(&export).unwrap();
        assert!(imported >= 2);
    }

    #[test]
    fn v3_subscriptions_inherit_global_agent_but_keep_custom_overrides() {
        let (store, dir) = store();
        for agent in [crate::subscription::DEFAULT_USER_AGENT, "Custom/1"] {
            let mut s = Subscription::new("fixture", "https://updates.invalid/");
            s.user_agent = agent.into();
            store.save_subscription(&s).unwrap();
        }
        store
            .conn
            .lock()
            .pragma_update(None, "user_version", 3)
            .unwrap();
        drop(store);
        let reopened = Store::open(&AppPaths::from_root(dir.path())).unwrap();
        let subs = reopened.subscriptions().unwrap();
        assert!(subs[0].user_agent.is_empty());
        assert_eq!(subs[1].user_agent, "Custom/1");
    }

    #[test]
    fn v4_migration_adds_policy_only_and_preserves_custom_group_fields() {
        let (store, dir) = store();
        let id = store.active_profile_id().unwrap();
        let mut profile = serde_json::to_value(AppModel::default()).unwrap();
        let group = &mut profile["groups"][0];
        group.as_object_mut().unwrap().remove("switch_policy");
        group["extra"] = "fixture".into();
        let mut expected = profile.clone();
        expected["groups"][0]["switch_policy"] =
            serde_json::to_value(crate::model::NodeSwitchPolicy::default()).unwrap();
        let payload = store.secrets.encrypt(&profile.to_string());
        store
            .conn
            .lock()
            .execute(
                "UPDATE profiles SET model=?1 WHERE id=?2",
                params![payload, id],
            )
            .unwrap();
        store
            .conn
            .lock()
            .pragma_update(None, "user_version", 4)
            .unwrap();
        drop(store);
        let reopened = Store::open(&AppPaths::from_root(dir.path())).unwrap();
        let raw: String = reopened
            .conn
            .lock()
            .query_row("SELECT model FROM profiles WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(SecretBox::is_encrypted(&raw));
        let result: serde_json::Value =
            serde_json::from_str(&reopened.secrets.decrypt(&raw).unwrap()).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn migrations_set_version() {
        let (store, _dir) = store();
        let conn = Connection::open(store.db_path()).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn mesh_payload_is_encrypted_and_independent_of_proxy_profile() {
        let (store, _dir) = store();
        let proxy_id = store.active_profile_id();
        let profile = crate::mesh::MeshProfile {
            network_name: "test-net".into(),
            network_secret: crate::mesh::MeshSecret("fake-mesh-secret".into()),
            peers: vec!["tcp://192.0.2.1:11010".into()],
            ..Default::default()
        };
        store.save_mesh_profile(&profile).unwrap();
        let raw: String = store
            .conn
            .lock()
            .query_row(
                "SELECT payload FROM mesh_profiles WHERE id=?1",
                [&profile.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(SecretBox::is_encrypted(&raw));
        assert!(!raw.contains("fake-mesh-secret"));
        assert!(!raw.contains("test-net"));
        assert_eq!(store.mesh_profiles().unwrap(), vec![profile.clone()]);
        assert_eq!(store.active_profile_id(), proxy_id);
        store.delete_mesh_profile(&profile.id).unwrap();
        assert!(store.mesh_profiles().unwrap().is_empty());
    }

    #[test]
    fn upgrades_v2_without_rewriting_proxy_profile() {
        let (store, dir) = store();
        let id = store.active_profile_id().unwrap();
        let before: String = store
            .conn
            .lock()
            .query_row("SELECT model FROM profiles WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        store
            .conn
            .lock()
            .execute_batch(
                "DROP TABLE mesh_profiles; PRAGMA user_version=2; PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        drop(store);
        let upgraded = Store::open(&AppPaths::from_root(dir.path())).unwrap();
        assert!(upgraded.mesh_profiles().unwrap().is_empty());
        let after: String = upgraded
            .conn
            .lock()
            .query_row("SELECT model FROM profiles WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(upgraded.active_profile_id(), Some(id));
    }
}
