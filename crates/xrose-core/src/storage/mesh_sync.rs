//! Persist a reviewed merge in one transaction, detecting concurrent subscription and network edits.
use super::*;
use crate::mesh::MeshProfile;

impl Store {
    pub(crate) fn commit_mesh_sync(
        &self,
        profile_id: &str,
        model: Option<&AppModel>,
        subs: (&[Subscription], &[Subscription]),
        mesh: (&[MeshProfile], &[MeshProfile]),
    ) -> Result<()> {
        fn stale() -> Error {
            Error::config("本机配置已更改，请重新预览后合并")
        }
        let seal = |json: String| -> Result<String> {
            let value = self.secrets.encrypt(&json);
            SecretBox::is_encrypted(&value)
                .then_some(value)
                .ok_or_else(|| Error::other("配置加密失败"))
        };
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let active: Option<String> = tx
            .query_row(
                "SELECT value FROM app_state WHERE key='active_profile'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if active.as_deref() != Some(profile_id) {
            return Err(stale());
        }
        if let Some(m) = model {
            if tx.execute(
                "UPDATE profiles SET model=?1, updated_at=?2 WHERE id=?3",
                params![seal(serde_json::to_string(m)?)?, now_unix(), profile_id],
            )? != 1
            {
                return Err(stale());
            }
        }
        for s in subs.1 {
            let old = subs.0.iter().find(|v| v.id == s.id);
            if old == Some(s) {
                continue;
            }
            let current: Option<String> = tx
                .query_row(
                    "SELECT payload FROM subscriptions WHERE id=?1",
                    [&s.id],
                    |r| r.get(0),
                )
                .optional()?;
            let stored = current
                .as_ref()
                .map(|v| -> Result<Subscription> {
                    Ok(serde_json::from_str(&self.secrets.decrypt(v)?)?)
                })
                .transpose()?;
            if stored.as_ref() != old {
                return Err(stale());
            }
            tx.execute("INSERT INTO subscriptions(id,payload,sort) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload,sort=excluded.sort",params![s.id,seal(serde_json::to_string(s)?)?,s.sort])?;
        }
        for p in mesh.1 {
            let old = mesh.0.iter().find(|v| v.id == p.id);
            if old == Some(p) {
                continue;
            }
            let current: Option<String> = tx
                .query_row(
                    "SELECT payload FROM mesh_profiles WHERE id=?1",
                    [&p.id],
                    |r| r.get(0),
                )
                .optional()?;
            let stored = current
                .as_ref()
                .map(|v| -> Result<MeshProfile> {
                    Ok(serde_json::from_str(&self.secrets.decrypt(v)?)?)
                })
                .transpose()?;
            if stored.as_ref() != old {
                return Err(stale());
            }
            tx.execute("INSERT INTO mesh_profiles(id,payload,updated_at) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",params![p.id,seal(serde_json::to_string(p)?)?,now_unix()])?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
impl Store {
    pub(crate) fn with_sync_test_failure(&self) {
        self.conn.lock().execute_batch("CREATE TEMP TRIGGER reject_mesh BEFORE INSERT ON mesh_profiles BEGIN SELECT RAISE(ABORT, 'fixture failure'); END").unwrap();
    }
    pub(crate) fn remove_sync_test_failure(&self) {
        self.conn
            .lock()
            .execute_batch("DROP TRIGGER reject_mesh")
            .unwrap();
    }
}
