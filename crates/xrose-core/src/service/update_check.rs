//! Release checks for the actual configured executable, outside the render path.

use super::{
    APP_VERSION, AppService, CoreKind, Error, Result, Settings, ToastLevel, UpdateCandidate,
    now_unix, updates,
};

impl AppService {
    /// GitHub owner/repository configured when building a public release.
    pub fn project_repository(&self) -> Option<&'static str> {
        option_env!("XROSE_GITHUB_REPOSITORY")
            .map(str::trim)
            .filter(|repo| !repo.is_empty())
    }

    pub(super) fn same_binary(core: CoreKind, a: &Settings, b: &Settings) -> bool {
        a.binary_override(core) == b.binary_override(core)
            && a.active_version(core) == b.active_version(core)
    }

    async fn installed_core_version(
        &self,
        core: CoreKind,
        settings: &Settings,
    ) -> Result<Option<String>> {
        if settings.binary_override(core).is_none()
            && settings.active_version(core).is_none()
            && crate::core::binary::path_binary(core).is_none()
        {
            return Ok(None);
        }
        let binary = self.inner.core.resolve_binary(core, settings)?;
        crate::core::binary::probe_version(&binary)
            .await
            .map(Some)
            .ok_or_else(|| {
                Error::binary(format!(
                    "无法读取 {} 版本，请检查设置中的内核路径",
                    core.display_name()
                ))
            })
    }

    /// The download action can install a managed copy even when PATH already has this release.
    pub async fn core_download_candidate(
        &self,
        core: CoreKind,
        settings: &Settings,
        network: &updates::UpdateNetwork,
    ) -> Result<Option<UpdateCandidate>> {
        let managed =
            settings.binary_override(core).is_none() && settings.active_version(core).is_some();
        let current = if managed {
            self.installed_core_version(core, settings)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        updates::check_core_update(core, current.as_deref(), network).await
    }

    pub(super) fn record_core_check(
        &self,
        core: CoreKind,
        checked: &Settings,
        result: Result<Option<UpdateCandidate>>,
    ) -> bool {
        let current = self.inner.settings.read();
        if !Self::same_binary(core, checked, &current) {
            return false;
        }
        let mut updates = self.inner.updates.lock();
        let status = updates.cores.entry(core.as_str().into()).or_default();
        status.checked_at = Some(now_unix());
        match result {
            Ok(candidate) => {
                status.candidate = candidate;
                status.error = None;
            }
            Err(error) => status.error = Some(error.to_string()),
        }
        true
    }

    pub fn check_updates(&self) {
        self.check_core_release(self.settings().core, true);
        self.check_app_release(true);
    }

    pub(super) fn check_core_release(&self, core: CoreKind, manual: bool) {
        let key = format!("updates-{}", core.as_str());
        if !self.inner.busy.write().insert(key.clone()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let settings = service.settings();
            // A failed reconciliation may be retried by the monitor. Limit
            // background release requests; a manual check always retries.
            let recent = service
                .inner
                .updates
                .lock()
                .cores
                .get(core.as_str())
                .and_then(|s| s.checked_at)
                .is_some_and(|t| now_unix().saturating_sub(t) < 60);
            if manual || !recent {
                let result = match service.installed_core_version(core, &settings).await {
                    Ok(Some(version)) => {
                        updates::check_core_update(
                            core,
                            Some(&version),
                            &service.resource_network(),
                        )
                        .await
                    }
                    // An available download is not an update to an installed core.
                    Ok(None) => {
                        service.set_busy(&key, false);
                        return;
                    }
                    Err(error) => Err(error),
                };
                let message = if manual {
                    Some(match &result {
                        Ok(Some(candidate)) => (
                            ToastLevel::Info,
                            format!("{} 可更新至 {}", core.display_name(), candidate.version),
                        ),
                        Ok(None) => (
                            ToastLevel::Info,
                            format!("{} 已是最新版本", core.display_name()),
                        ),
                        Err(error) => (
                            ToastLevel::Warning,
                            format!("{} 更新检查失败：{error}", core.display_name()),
                        ),
                    })
                } else {
                    None
                };
                if service.record_core_check(core, &settings, result)
                    && let Some((level, text)) = message
                {
                    service.toast(level, text);
                }
            }
            service.set_busy(&key, false);
        });
    }

    pub(super) fn check_app_release(&self, manual: bool) {
        let Some(repository) = self.project_repository() else {
            return;
        };
        if !self.inner.busy.write().insert("updates-app".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            let result =
                updates::check_app_update(repository, APP_VERSION, &service.resource_network())
                    .await;
            let message = {
                let mut status = service.inner.updates.lock();
                status.checked_at = Some(now_unix());
                match result {
                    Ok(candidate) => {
                        let message = candidate
                            .as_ref()
                            .map(|a| format!("Xrose 可更新至 {}", a.version))
                            .unwrap_or_else(|| "Xrose 已是最新版本".into());
                        status.app = candidate;
                        status.error = None;
                        (ToastLevel::Info, message)
                    }
                    Err(error) => {
                        let message = format!("Xrose 更新检查失败：{error}");
                        status.error = Some(message.clone());
                        (ToastLevel::Warning, message)
                    }
                }
            };
            if manual {
                service.toast(message.0, message.1);
            }
            service.set_busy("updates-app", false);
        });
    }
}
