//! Route-file jobs and their review/commit boundary.
use super::{AppService, ToastLevel};
use crate::error::{Error, Result};
use crate::route::transfer::{self, ExportScope, ImportSummary, PreparedImport};
use std::path::PathBuf;

pub(super) struct PendingImport {
    pub id: String,
    profile: Option<String>,
    pub plan: PreparedImport,
}

impl AppService {
    pub fn preview_route_import(&self, text: &str) -> Result<(String, ImportSummary)> {
        let profile = self.inner.active_profile.read().clone();
        let plan = transfer::prepare(text, &self.inner.model.read())?;
        let summary = plan.summary.clone();
        let id = crate::util::new_id("import");
        *self.inner.route_import.lock() = Some(PendingImport {
            id: id.clone(),
            profile,
            plan,
        });
        Ok((id, summary))
    }

    pub fn cancel_route_import(&self, id: &str) {
        let mut pending = self.inner.route_import.lock();
        if pending.as_ref().is_some_and(|p| p.id == id) {
            pending.take();
        }
    }

    pub fn commit_route_import(&self, id: &str) -> Result<bool> {
        let pending = self
            .inner
            .route_import
            .lock()
            .take_if(|p| p.id == id)
            .ok_or_else(|| Error::config("导入预览已失效，请重新选择文件"))?;
        self.commit_model_change(|model| {
            if pending.profile != *self.inner.active_profile.read() {
                return Err(Error::config("当前配置已切换，请重新预览后导入"));
            }
            pending.plan.apply(model)
        })
    }

    pub fn export_route_text(&self, scope: &ExportScope) -> Result<String> {
        transfer::export(&self.inner.model.read(), scope)
    }

    pub fn save_route_export(&self, scope: &ExportScope, path: &std::path::Path) -> Result<()> {
        let text = self.export_route_text(scope)?;
        crate::util::atomic_write_private(path, text.as_bytes())
            .map_err(|_| Error::config("导出失败，请选择可写的文件位置"))
    }

    fn route_file_job(&self, task: impl FnOnce(&Self) -> Result<&'static str> + Send + 'static) {
        if !self.inner.busy.write().insert("route-file".into()) {
            return;
        }
        let service = self.clone();
        self.inner.rt.spawn_blocking(move || {
            match task(&service) {
                Ok(message) => service.toast(ToastLevel::Success, message),
                Err(err) => service.toast(ToastLevel::Error, err.to_string()),
            }
            service.set_busy("route-file", false);
        });
    }

    pub fn open_route_file(&self, path: PathBuf) {
        self.route_file_job(move |service| {
            service.inner.route_import.lock().take();
            service.preview_route_import(&transfer::read_file(&path)?)?;
            Ok("已读取文件，请核对路由页中的导入预览")
        });
    }

    pub fn confirm_route_file(&self, id: String) {
        self.route_file_job(move |service| {
            if service.commit_route_import(&id)? {
                Ok("路由已导入，应用配置后生效")
            } else {
                Ok("规则集已存在，配置无变化")
            }
        });
    }

    pub fn export_route_file(&self, scope: ExportScope, path: PathBuf) {
        self.route_file_job(move |service| {
            service.save_route_export(&scope, &path)?;
            Ok("路由文件已导出")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AppPaths;

    #[test]
    fn review_and_cancel_do_not_write_and_confirmation_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let previous = service.model().routing;
        let src = "[SwitchyOmega Conditions]\n*.example.org";
        let (first, _) = service.preview_route_import(src).unwrap();
        service.cancel_route_import(&first);
        assert!(service.commit_route_import(&first).is_err());
        let (id, _) = service.preview_route_import(src).unwrap();
        assert!(service.commit_route_import(&first).is_err());
        assert_eq!(service.model().routing, previous);
        assert!(service.commit_route_import(&id).unwrap());
        assert!(service.commit_route_import(&id).is_err());
        let model = service.model();
        let profile_id = service.inner.active_profile.read().clone().unwrap();
        let stored = service
            .inner
            .store
            .load_profile(&profile_id)
            .unwrap()
            .unwrap();
        assert_eq!(model.routing, stored.model.routing);
        assert_eq!(model.routing.rules, previous.rules);
        assert_eq!(
            model.routing.rule_groups.len(),
            previous.rule_groups.len() + 1
        );
    }
}
