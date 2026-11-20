//! Configuration-import jobs, in-memory preview state, and transactional commit.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::{AppService, ToastLevel};
use crate::config_import::{
    self, ConfigImportSummary, ConfigImportView, ImportMode, ImportOptions, ImportResult,
    PreparedConfigImport,
};
use crate::error::{Error, Result};

pub(super) struct PendingConfigImport {
    pub id: String,
    profile: Option<String>,
    pub prepared: PreparedConfigImport,
}

impl PendingConfigImport {
    pub(super) fn view(&self) -> ConfigImportView {
        ConfigImportView {
            id: self.id.clone(),
            summary: self.prepared.summary(),
        }
    }
}

impl AppService {
    /// Parses configuration text into an in-memory review state without modifying storage.
    pub fn preview_config_import(
        &self,
        text: &str,
        source_name: Option<String>,
    ) -> Result<(String, ConfigImportSummary)> {
        let profile = self.inner.active_profile.read().clone();
        let prepared = config_import::prepare(text, source_name)?;
        let summary = prepared.summary();
        let id = crate::util::new_id("cfg-import");
        *self.inner.config_import.lock() = Some(PendingConfigImport {
            id: id.clone(),
            profile,
            prepared,
        });
        Ok((id, summary))
    }

    /// Switches the active profile inside a multi-profile Xrose backup preview.
    pub fn select_config_import_profile(
        &self,
        id: &str,
        profile_index: usize,
    ) -> Result<ConfigImportSummary> {
        let mut guard = self.inner.config_import.lock();
        let pending = guard
            .as_mut()
            .filter(|p| p.id == id)
            .ok_or_else(|| Error::config("导入预览已失效，请重新选择配置文件"))?;
        pending.prepared.select_profile(profile_index)
    }

    /// Discards an active configuration import preview from memory.
    pub fn cancel_config_import(&self, id: &str) {
        let mut guard = self.inner.config_import.lock();
        if guard.as_ref().is_some_and(|p| p.id == id) {
            guard.take();
        }
    }

    /// Discards any active configuration import preview regardless of id.
    pub fn clear_config_import(&self) {
        self.inner.config_import.lock().take();
    }

    /// Applies the reviewed configuration import in one encrypted SQLite transaction.
    pub fn commit_config_import(&self, id: &str, options: &ImportOptions) -> Result<ImportResult> {
        let (profile, prepared) = {
            let guard = self.inner.config_import.lock();
            let pending = guard
                .as_ref()
                .filter(|p| p.id == id)
                .ok_or_else(|| Error::config("导入预览已失效，请重新选择配置文件"))?;
            (pending.profile.clone(), pending.prepared.clone())
        };

        let active_profile = self.inner.active_profile.read().clone();
        if profile != active_profile {
            return Err(Error::config("当前配置已切换，请重新预览后导入"));
        }
        let profile_id = active_profile.ok_or_else(|| Error::config("未找到当前配置档，请重试"))?;

        let mut model_guard = self.inner.model.write();
        let mut subs_guard = self.inner.subs.write();

        let mut next_model = model_guard.clone();
        let mut next_subs = subs_guard.clone();
        let result = prepared.apply(&mut next_model, &mut next_subs, options)?;

        let model_changed = next_model != *model_guard;
        let subs_changed = next_subs != *subs_guard;
        let replace_subs = options.mode == ImportMode::Replace
            && options
                .sections
                .contains(&config_import::ImportSection::Subscriptions);

        if model_changed || subs_changed {
            self.inner.store.commit_config_import(
                &profile_id,
                model_changed.then_some(&next_model),
                subs_changed.then_some((&next_subs, replace_subs)),
            )?;
        }

        let removed_node_ids: Vec<String> = if model_changed {
            let next_ids: BTreeSet<&str> = next_model.nodes.iter().map(|n| n.id.as_str()).collect();
            model_guard
                .nodes
                .iter()
                .filter(|n| !next_ids.contains(n.id.as_str()))
                .map(|n| n.id.clone())
                .collect()
        } else {
            Vec::new()
        };

        *model_guard = next_model;
        *subs_guard = next_subs;
        drop(subs_guard);
        drop(model_guard);

        self.inner.config_import.lock().take_if(|p| p.id == id);

        if !removed_node_ids.is_empty() {
            let mut latency = self.inner.latency.write();
            for node_id in &removed_node_ids {
                latency.remove(node_id);
            }
            drop(latency);
            self.remove_health_targets(&removed_node_ids);
        }

        if model_changed {
            self.refresh_profiles();
            self.refresh_doctor();
            self.mark_dirty();
        }

        Ok(result)
    }

    /// Exports the decrypted Xrose backup bundle JSON as text (for CLI / scripting).
    pub fn export_backup_json(&self) -> Result<String> {
        self.inner.store.export_json_string()
    }

    fn config_import_job(
        &self,
        task: impl FnOnce(&Self) -> Result<Option<String>> + Send + 'static,
    ) {
        if !self.inner.busy.write().insert("config-import".into()) {
            return;
        }
        let service = self.clone();
        self.inner.rt.spawn_blocking(move || {
            match task(&service) {
                Ok(Some(message)) => service.toast(ToastLevel::Success, message),
                Ok(None) => {}
                Err(err) => service.toast(ToastLevel::Error, err.to_string()),
            }
            service.set_busy("config-import", false);
        });
    }

    /// Reads and parses a configuration file on a worker thread for UI review.
    pub fn open_config_import_file(&self, path: PathBuf) {
        let source_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        self.config_import_job(move |service| {
            let text = config_import::read_file(&path)?;
            service.preview_config_import(&text, source_name)?;
            Ok(None)
        });
    }

    /// Parses configuration text (e.g. from clipboard) on a worker thread for UI review.
    pub fn open_config_import_text(&self, text: String, source_name: Option<String>) {
        self.config_import_job(move |service| {
            service.preview_config_import(&text, source_name)?;
            Ok(None)
        });
    }

    /// Commits a reviewed configuration import on a worker thread and shows a summary toast.
    pub fn confirm_config_import(&self, id: String, options: ImportOptions) {
        self.config_import_job(move |service| {
            let result = service.commit_config_import(&id, &options)?;
            let mut parts = Vec::new();
            if result.nodes_added > 0 {
                parts.push(format!("{} 个节点", result.nodes_added));
            }
            if result.groups_applied > 0 {
                parts.push(format!("{} 个出站组", result.groups_applied));
            }
            if result.rules_applied > 0 || result.rule_sets_applied > 0 {
                parts.push(format!(
                    "{} 条规则 / {} 个规则集",
                    result.rules_applied, result.rule_sets_applied
                ));
            }
            if result.dns_applied {
                parts.push("DNS 配置".into());
            }
            if result.subscriptions_added > 0 {
                parts.push(format!("{} 条订阅", result.subscriptions_added));
            }
            let message = if parts.is_empty() {
                "所选配置与本机一致，未产生变更".into()
            } else {
                format!("已导入：{}", parts.join("、"))
            };
            Ok(Some(message))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_import::{ConfigFormat, ImportSection};
    use crate::paths::AppPaths;

    #[test]
    fn preview_cancel_and_transactional_commit_work_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let initial_nodes = service.model().nodes.len();

        let clash = r#"
proxies:
  - name: Node-A
    type: socks5
    server: node-a.example
    port: 1080
proxy-groups:
  - name: 节点选择
    type: select
    proxies:
      - Node-A
rules:
  - DOMAIN-SUFFIX,work.example,节点选择
  - MATCH,节点选择
"#;

        let (first_id, summary) = service
            .preview_config_import(clash, Some("demo.yaml".into()))
            .unwrap();
        assert_eq!(summary.format, ConfigFormat::ClashYaml);
        assert_eq!(summary.counts.nodes, 1);
        assert!(service.snapshot().config_import.is_some());

        service.cancel_config_import(&first_id);
        assert!(service.snapshot().config_import.is_none());
        assert!(
            service
                .commit_config_import(&first_id, &ImportOptions::default())
                .is_err()
        );
        assert_eq!(service.model().nodes.len(), initial_nodes);

        let (id, _) = service
            .preview_config_import(clash, Some("demo.yaml".into()))
            .unwrap();
        let result = service
            .commit_config_import(
                &id,
                &ImportOptions {
                    mode: ImportMode::Merge,
                    sections: [
                        ImportSection::Nodes,
                        ImportSection::Groups,
                        ImportSection::Routing,
                    ]
                    .into_iter()
                    .collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(result.nodes_added, 1);
        assert_eq!(result.groups_applied, 1);
        assert_eq!(result.rules_applied, 1);
        assert!(service.snapshot().config_import.is_none());

        // Verify durability in encrypted SQLite storage.
        let active_id = service.snapshot().active_profile.unwrap();
        let stored = service
            .store()
            .load_profile(&active_id)
            .unwrap()
            .unwrap()
            .model;
        assert_eq!(stored.nodes.len(), 1);
        assert_eq!(stored.nodes[0].name, "Node-A");
        assert!(
            stored
                .routing
                .rule_groups
                .iter()
                .any(|g| g.name == "Clash 导入规则")
        );

        // Export as Xrose backup and re-import in replace mode.
        let backup_json = service.export_backup_json().unwrap();
        let (backup_id, backup_summary) = service
            .preview_config_import(&backup_json, Some("backup.json".into()))
            .unwrap();
        assert_eq!(backup_summary.format, ConfigFormat::XroseBackup);
        assert_eq!(backup_summary.counts.nodes, 1);
        let replaced = service
            .commit_config_import(
                &backup_id,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(replaced.nodes_added, 1);
        assert_eq!(service.model().nodes.len(), 1);
    }

    #[test]
    fn commit_rejects_when_active_profile_changes() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let singbox = r#"{"outbounds":[{"type":"socks","tag":"s1","server":"s.example","server_port":1080}]}"#;
        let (id, _) = service.preview_config_import(singbox, None).unwrap();

        let other_id = service
            .store()
            .save_profile(&crate::model::AppModel::new("其他配置"))
            .unwrap();
        service.switch_profile(&other_id).unwrap();

        let err = service
            .commit_config_import(&id, &ImportOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("已切换"));
    }
}
