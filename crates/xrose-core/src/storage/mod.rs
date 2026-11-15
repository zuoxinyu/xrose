//! SQLite persistence with migrations, encrypted payloads and JSON backups.

pub mod crypto;
mod mesh_sync;
mod traffic_statistics;

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

pub const SCHEMA_VERSION: i64 = 4;
// Distinguish the release schema from overlapping pre-release version numbers.
const APPLICATION_ID: i64 = i32::from_be_bytes(*b"XROS") as i64;

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
        store.initialize_schema()?;
        store.seed_default_profile()?;
        Ok(Arc::new(store))
    }

    fn backup_before_migration(&self, conn: &Connection, version: i64) -> Result<()> {
        std::fs::create_dir_all(&self.backup_dir).map_err(|e| io_context(&self.backup_dir, e))?;
        let target = self.backup_dir.join(format!(
            "xrose-v{version}-{}.db",
            crate::util::new_id("backup")
        ));
        // Include committed WAL frames and produce a self-contained SQLite snapshot.
        conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
        Ok(())
    }

    fn initialize_schema(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let app_id: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match (app_id, version) {
            (APPLICATION_ID, SCHEMA_VERSION) => return Ok(()),
            // Old profile JSON may contain a chained node ID. Serde drops it when saving;
            // the application-level SOCKS5 endpoint starts empty in the separate settings file.
            (APPLICATION_ID, 1) => {}
            // Earlier releases share these tables; migrate legacy routing JSON below.
            (APPLICATION_ID, 2 | 3) => {}
            (0, 0) => tx.execute_batch(include_str!("schema_v1.sql"))?,
            (0, 14 | 15) => {
                // These builds already use the complete v1 table layout. The two routing
                // fields introduced in development v15 have serde defaults.
                // VACUUM INTO needs a separate reader while the transaction holds the write lock.
                let reader = Connection::open_with_flags(
                    &self.db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?;
                self.backup_before_migration(&reader, version)?;
            }
            (0, 1..=13) => {
                return Err(Error::config(format!(
                    "开发版数据库 v{version} 需先用支持数据库 v14 或 v15 的开发版 Xrose 打开，或使用新的数据目录"
                )));
            }
            (0 | APPLICATION_ID, _) => {
                return Err(Error::config(format!(
                    "此版本的 Xrose 不支持数据库 v{version}，请使用与数据库匹配的版本或新的数据目录"
                )));
            }
            _ => return Err(Error::config("数据库标识与 Xrose 不符，请检查数据目录")),
        }
        let profiles = {
            let mut stmt = tx.prepare("SELECT id, model FROM profiles")?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, payload) in profiles {
            let mut model = serde_json::from_str(&self.secrets.decrypt(&payload)?)?;
            if migrate_routing_rules(&mut model)? {
                let payload = self.secrets.encrypt(&serde_json::to_string(&model)?);
                if !SecretBox::is_encrypted(&payload) {
                    return Err(Error::config("路由配置迁移加密失败，请重试"));
                }
                tx.execute(
                    "UPDATE profiles SET model = ?1 WHERE id = ?2",
                    params![payload, id],
                )?;
            }
        }
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
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
            "xrose-{}.db",
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
        for mut profile in bundle.profiles {
            migrate_work_mode(&mut profile.model_json)?;
            migrate_routing_rules(&mut profile.model_json)?;
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

/// Convert legacy switches to visible rules without resetting user order or unknown fields.
fn migrate_routing_rules(model: &mut serde_json::Value) -> Result<bool> {
    use crate::model::{Matchers, RouteAction, RouteRule, RoutingConfig, lan_direct_rules};
    use serde_json::{Value, json};

    let Some(routing) = model.get_mut("routing").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let lan = routing.remove("bypass_lan");
    let china = routing.remove("bypass_china_ips");
    if lan.is_none() && china.is_none() {
        return Ok(false);
    }
    let lan_rules = lan_direct_rules();
    let mut legacy_lan = lan_rules[0].matchers.clone();
    legacy_lan.domain_suffixes = lan_rules[1].matchers.domain_suffixes.clone();

    let split_lan = |rules: &mut Value| -> Result<()> {
        let Some(rules) = rules.as_array_mut() else {
            return Err(Error::config("路由规则格式无效，请从备份恢复"));
        };
        let mut index = 0;
        while index < rules.len() {
            let rule: RouteRule = serde_json::from_value(rules[index].clone())?;
            if rule.name == "局域网直连"
                && rule.action == RouteAction::direct()
                && rule.matchers == legacy_lan
            {
                rules[index]["matchers"] = serde_json::to_value(&lan_rules[0].matchers)?;
                let mut domains = rules[index].clone();
                domains["id"] = crate::util::new_id("rule").into();
                domains["name"] = lan_rules[1].name.clone().into();
                domains["matchers"] = serde_json::to_value(&lan_rules[1].matchers)?;
                rules.insert(index + 1, domains);
                index += 1;
            }
            index += 1;
        }
        Ok(())
    };
    split_lan(routing.entry("rules").or_insert(json!([])))?;
    if let Some(groups) = routing.get_mut("rule_groups").and_then(Value::as_array_mut) {
        for group in groups {
            if let Some(rules) = group.get_mut("rules") {
                split_lan(rules)?;
            }
        }
    }

    let mut required = Vec::new();
    if lan.as_ref().and_then(Value::as_bool).unwrap_or(true) {
        required.extend(lan_rules);
    }
    if china.as_ref().and_then(Value::as_bool).unwrap_or(false) {
        required.push(RouteRule::new(
            "中国大陆 IP 直连",
            Matchers {
                geoips: vec!["cn".into()],
                ..Default::default()
            },
            RouteAction::direct(),
        ));
    }
    for required in required {
        let config: RoutingConfig = serde_json::from_value(Value::Object(routing.clone()))?;
        let same_rule =
            |rule: &RouteRule| rule.matchers == required.matchers && rule.action == required.action;
        if config.effective_rules().any(same_rule) {
            continue;
        }
        let rules = routing["rules"].as_array_mut().unwrap();
        if let Some(index) = config.rules.iter().position(same_rule) {
            rules[index]["enabled"] = true.into();
        } else {
            rules.push(serde_json::to_value(required)?);
        }
    }
    Ok(true)
}

/// Retain existing choices and preserve unknown fields in older profiles.
fn migrate_work_mode(model: &mut serde_json::Value) -> Result<bool> {
    let object = model
        .as_object_mut()
        .ok_or_else(|| Error::config("配置格式无效，请从备份恢复"))?;
    if object.contains_key("work_mode") {
        return Ok(false);
    }
    let has_nodes = object
        .get("nodes")
        .and_then(|n| n.as_array())
        .is_some_and(|n| !n.is_empty());
    object.insert(
        "work_mode".into(),
        if has_nodes { "proxy" } else { "direct" }.into(),
    );
    Ok(true)
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
    fn legacy_routing_switches_migrate_to_visible_rules_once() {
        use crate::model::{RouteAction, RoutePreset, RouteRuleGroup, lan_direct_rules};
        use serde_json::json;
        let (store, dir) = store();
        let paths = AppPaths::from_root(dir.path());
        let mut model = AppModel::default();
        model.apply_preset(RoutePreset::GfwList);
        // Simulate the old GFWList template, whose LAN and mainland IP rules were implicit.
        model.routing.rules.retain(|rule| {
            rule.matchers.ip_cidrs.is_empty()
                && rule.matchers.domain_suffixes.is_empty()
                && rule.matchers.geoips.is_empty()
        });
        let mut legacy = serde_json::to_value(&model).unwrap();
        legacy["routing"]["bypass_lan"] = true.into();
        legacy["routing"]["bypass_china_ips"] = true.into();
        legacy["routing"]["fixture_extra"] = json!({"keep": true});
        let mut ids = Vec::new();
        for name in ["Fixture active", "Fixture inactive"] {
            let id = store.save_profile(&AppModel::new(name)).unwrap();
            let payload = store.secrets.encrypt(&legacy.to_string());
            store
                .conn
                .lock()
                .execute(
                    "UPDATE profiles SET model = ?1 WHERE id = ?2",
                    params![payload, id],
                )
                .unwrap();
            ids.push(id);
        }
        store.set_active_profile(&ids[0]).unwrap();
        store
            .conn
            .lock()
            .pragma_update(None, "user_version", 3)
            .unwrap();
        drop(store);
        for _ in 0..2 {
            let store = Store::open(&paths).unwrap();
            for id in &ids {
                let saved = store.load_profile(id).unwrap().unwrap();
                assert_eq!(saved.model.routing.rules.len(), 6);
                assert_eq!(saved.model.routing.rules[1].name, "GFWList 代理");
                for lan in lan_direct_rules() {
                    assert_eq!(
                        saved
                            .model
                            .routing
                            .effective_rules()
                            .filter(
                                |rule| rule.matchers == lan.matchers && rule.action == lan.action
                            )
                            .count(),
                        1
                    );
                }
                let raw: String = store
                    .conn
                    .lock()
                    .query_row("SELECT model FROM profiles WHERE id = ?1", [id], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert!(SecretBox::is_encrypted(&raw));
                let value: serde_json::Value =
                    serde_json::from_str(&store.secrets.decrypt(&raw).unwrap()).unwrap();
                assert!(value["routing"].get("bypass_lan").is_none());
                assert!(value["routing"].get("bypass_china_ips").is_none());
                assert_eq!(value["routing"]["fixture_extra"], json!({"keep": true}));
            }
        }

        // Split only the old mixed LAN preset rule, retaining its identity and disabled state.
        let mut model = AppModel::default();
        let mut lan = lan_direct_rules().remove(0);
        lan.matchers.domain_suffixes = lan_direct_rules()[1].matchers.domain_suffixes.clone();
        lan.enabled = false;
        let lan_id = lan.id.clone();
        model.routing.rules = vec![lan];
        model.routing.rule_groups = vec![RouteRuleGroup {
            enabled: false,
            ..Default::default()
        }];
        let mut legacy = serde_json::to_value(&model).unwrap();
        legacy["routing"]["bypass_lan"] = false.into();
        legacy["routing"]["bypass_china_ips"] = false.into();
        assert!(migrate_routing_rules(&mut legacy).unwrap());
        let migrated: AppModel = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(migrated.routing.rules.len(), 2);
        assert_eq!(migrated.routing.rules[0].id, lan_id);
        assert!(
            migrated
                .routing
                .rules
                .iter()
                .all(|rule| !rule.enabled && rule.action == RouteAction::direct())
        );
        assert!(!migrated.routing.rule_groups[0].enabled);
        assert!(!migrate_routing_rules(&mut legacy).unwrap());
    }

    #[test]
    fn routing_migration_reuses_existing_direct_rules_and_imports_old_backups() {
        use crate::model::{RoutePreset, RouteRuleGroup};
        let mut model = AppModel::default();
        model.apply_preset(RoutePreset::BypassMainland);
        model
            .routing
            .rules
            .iter_mut()
            .find(|rule| rule.matchers.geoips == ["cn"])
            .unwrap()
            .enabled = false;
        let mut grouped = crate::model::lan_direct_rules();
        model
            .routing
            .rules
            .retain(|rule| !grouped.iter().any(|lan| lan.matchers == rule.matchers));
        grouped[0].name = "Fixture renamed LAN".into();
        model.routing.rule_groups.push(RouteRuleGroup {
            rules: grouped,
            ..Default::default()
        });
        let mut legacy = serde_json::to_value(&model).unwrap();
        legacy["routing"]["bypass_lan"] = true.into();
        legacy["routing"]["bypass_china_ips"] = true.into();
        let (store, dir) = store();
        let path = dir.path().join("fixture-backup.json");
        let bundle = BackupBundle {
            format: 1,
            exported_at: now_rfc3339(),
            profiles: vec![BackupProfile {
                id: "fixture-import".into(),
                name: "Fixture backup".into(),
                model_json: legacy,
            }],
            subscriptions: vec![],
        };
        atomic_write_json(&path, &bundle).unwrap();
        assert_eq!(store.import_json(&path).unwrap(), 1);
        let loaded = store.load_profile("fixture-import").unwrap().unwrap().model;
        assert_eq!(loaded.routing.rules.len(), model.routing.rules.len());
        assert_eq!(loaded.routing.rule_groups, model.routing.rule_groups);
        assert!(
            loaded
                .routing
                .rules
                .iter()
                .find(|rule| rule.matchers.geoips == ["cn"])
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn failed_routing_migration_keeps_schema_and_encrypted_profiles_unchanged() {
        let (store, dir) = store();
        let paths = AppPaths::from_root(dir.path());
        let id = store.active_profile_id().unwrap();
        let mut legacy = serde_json::to_value(AppModel::default()).unwrap();
        legacy["routing"]["bypass_lan"] = true.into();
        legacy["routing"]["rules"] = serde_json::json!([{"fixture_invalid_rule": true}]);
        let encrypted = store.secrets.encrypt(&legacy.to_string());
        {
            let conn = store.conn.lock();
            conn.execute(
                "UPDATE profiles SET model = ?1 WHERE id = ?2",
                params![encrypted, id],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }
        drop(store);
        assert!(Store::open(&paths).is_err());
        let conn = Connection::open(paths.database()).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        let raw: String = conn
            .query_row("SELECT model FROM profiles WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(raw, encrypted);
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
    fn development_databases_are_backed_up_and_rebased_once() {
        for version in [14, 15] {
            let (store, dir) = store();
            let paths = AppPaths::from_root(dir.path());
            let original = store.profiles().unwrap();
            let id = store.active_profile_id().unwrap();
            let mut model = serde_json::to_value(AppModel::default()).unwrap();
            if version == 14 {
                let routing = model["routing"].as_object_mut().unwrap();
                routing.remove("rule_set_source");
                routing.remove("custom_rule_set_template");
            }
            model["routing"]["rule_sets"][0]["url"] = "https://fixture.example/domains.srs".into();
            let payload = store.secrets.encrypt(&model.to_string());
            {
                let conn = store.conn.lock();
                conn.execute(
                    "UPDATE profiles SET model=?1 WHERE id=?2",
                    params![payload, id],
                )
                .unwrap();
                conn.pragma_update(None, "application_id", 0).unwrap();
                conn.pragma_update(None, "user_version", version).unwrap();
            }
            drop(store);
            for _ in 0..2 {
                let reopened = Store::open(&paths).unwrap();
                assert_eq!(reopened.profiles().unwrap(), original);
                let routing = reopened.load_profile(&id).unwrap().unwrap().model.routing;
                assert_eq!(
                    routing.rule_set_source,
                    crate::model::RuleSetSource::Upstream
                );
                assert_eq!(
                    routing.rule_sets[0].url,
                    "https://fixture.example/domains.srs"
                );
                let conn = reopened.conn.lock();
                let saved: String = conn
                    .query_row("SELECT model FROM profiles WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })
                    .unwrap();
                assert!(saved == payload);
                assert_eq!(
                    conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                        .unwrap(),
                    SCHEMA_VERSION
                );
            }
            let backups: Vec<_> = std::fs::read_dir(paths.backup_dir()).unwrap().collect();
            assert_eq!(backups.len(), 1);
            let backup = Connection::open(backups[0].as_ref().unwrap().path()).unwrap();
            assert_eq!(
                backup
                    .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                version
            );
        }
    }

    #[test]
    fn v1_profiles_drop_the_obsolete_chained_node_on_save() {
        let (store, dir) = store();
        let paths = AppPaths::from_root(dir.path());
        let id = store.active_profile_id().unwrap();
        let mut profile = serde_json::to_value(AppModel::default()).unwrap();
        profile["front_proxy"] = "obsolete-node".into();
        let encrypted = store.secrets.encrypt(&profile.to_string());
        {
            let conn = store.conn.lock();
            conn.execute(
                "UPDATE profiles SET model=?1 WHERE id=?2",
                params![encrypted, id],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        drop(store);
        let reopened = Store::open(&paths).unwrap();
        let profile = reopened.load_profile(&id).unwrap().unwrap();
        reopened.save_profile_with_id(&id, &profile.model).unwrap();
        let raw: String = reopened
            .conn
            .lock()
            .query_row("SELECT model FROM profiles WHERE id=?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            !reopened
                .secrets
                .decrypt(&raw)
                .unwrap()
                .contains("front_proxy")
        );
    }

    #[test]
    fn unsupported_versions_are_not_relabelled() {
        for (app_id, version) in [(0, 1), (APPLICATION_ID, 14), (0, 16)] {
            let (store, dir) = store();
            let paths = AppPaths::from_root(dir.path());
            let conn = Connection::open(store.db_path()).unwrap();
            conn.pragma_update(None, "application_id", app_id).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
            drop(store);
            assert!(Store::open(&paths).is_err());
            assert_eq!(
                conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                version
            );
        }
    }
}
