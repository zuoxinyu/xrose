//! Serialize desktop proxy changes on a blocking worker. Snapshots only read cached UI state.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Enable,
    Refresh,
    Restore,
}

#[derive(Default)]
pub(super) struct Requests {
    next: Option<Change>,
    target: Option<bool>,
    closing: bool,
    stopping_core: bool,
    managed: bool,
}

impl AppService {
    pub fn open_system_proxy_settings(&self) {
        let service = self.clone();
        self.spawn(async move {
            if !service.inner.proxy_backend.open_settings().await {
                service.toast(ToastLevel::Error, match service.settings().language {
                    crate::settings::Language::Zh => "无法打开系统代理页面，请从系统网络设置中查看",
                    crate::settings::Language::En => "Could not open proxy settings. Open the network settings for this device.",
                });
            }
        });
    }

    pub fn proxy_env_command(&self) -> crate::Result<system::proxy::shell::ProxyEnvCommand> {
        system::proxy::shell::for_default_shell(&self.inner.settings.read())
    }

    pub fn set_system_proxy(&self, enabled: bool) {
        let mut requests = self.inner.proxy_requests.lock();
        self.queue_proxy(
            &mut requests,
            if enabled {
                Change::Enable
            } else {
                Change::Restore
            },
        );
    }

    fn queue_proxy(&self, requests: &mut Requests, change: Change) {
        let enabled = change != Change::Restore;
        if requests.closing || (enabled && requests.stopping_core) {
            return;
        }
        if !(change == Change::Refresh && requests.next == Some(Change::Enable)) {
            requests.next = Some(change);
        }
        let running = requests.target.replace(enabled).is_some();
        requests.managed = true;
        if running {
            return;
        }
        self.set_busy("proxy", true);
        let service = self.clone();
        self.inner
            .rt
            .spawn_blocking(move || service.run_proxy_changes());
    }

    pub fn reapply_system_proxy(&self) {
        let mut requests = self.inner.proxy_requests.lock();
        let enabled = requests
            .target
            .unwrap_or_else(|| self.settings().system_proxy);
        if enabled
            && (requests.target.is_some()
                || !matches!(
                    self.system_proxy_status(),
                    Some(
                        system::proxy::SystemProxyStatus::Off
                            | system::proxy::SystemProxyStatus::External
                    )
                ))
        {
            self.queue_proxy(&mut requests, Change::Refresh);
        }
    }

    pub(super) fn stop_managed_proxy(&self) {
        let mut requests = self.inner.proxy_requests.lock();
        requests.stopping_core = true;
        if requests.target.is_some() || self.settings().system_proxy {
            self.queue_proxy(&mut requests, Change::Restore);
        }
    }

    pub(super) fn finish_core_stop(&self) {
        self.inner.proxy_requests.lock().stopping_core = false;
    }

    fn run_proxy_changes(&self) {
        let manager = self.inner.proxy.lock();
        loop {
            let next = {
                let mut requests = self.inner.proxy_requests.lock();
                match requests.next.take() {
                    Some(enabled) if !requests.closing => enabled,
                    _ => {
                        requests.target = None;
                        self.set_busy("proxy", false);
                        return;
                    }
                }
            };
            let enabled = next != Change::Restore;
            let result = if enabled {
                self.apply_desktop_proxy(&manager, next == Change::Refresh)
            } else {
                manager.restore()
            };
            match result {
                Ok(()) => {
                    if !enabled {
                        self.inner.pac.lock().take();
                    }
                    #[cfg(target_os = "macos")]
                    if !enabled {
                        self.inner.proxy_observation.write().clear_expected();
                    }
                    self.record_proxy_state(enabled);
                    self.toast(
                        ToastLevel::Success,
                        if enabled {
                            "系统代理已开启"
                        } else {
                            "系统代理已还原"
                        },
                    );
                }
                Err(error) => {
                    if enabled && manager.has_snapshot() {
                        if let Err(restore) = manager.restore() {
                            self.record_proxy_state(true);
                            self.toast(ToastLevel::Error, format!("设置系统代理失败：{error}。还原失败：{restore}，请重试关闭系统代理"));
                            continue;
                        }
                    }
                    if enabled {
                        self.inner.pac.lock().take();
                    }
                    #[cfg(target_os = "macos")]
                    if !manager.has_snapshot() {
                        self.inner.proxy_observation.write().clear_expected();
                    }
                    self.record_proxy_state(manager.has_snapshot());
                    self.toast(
                        ToastLevel::Error,
                        format!(
                            "{}失败：{error}",
                            if enabled {
                                "启用系统代理"
                            } else {
                                "还原系统代理"
                            }
                        ),
                    );
                }
            }
        }
    }

    fn record_proxy_state(&self, enabled: bool) {
        let result = {
            let mut settings = self.inner.settings.write();
            settings.system_proxy = enabled;
            settings.save(&self.inner.paths.settings_file())
        };
        if let Err(err) = result {
            self.toast(ToastLevel::Error, format!("保存系统代理状态失败：{err}"));
        }
    }

    fn apply_desktop_proxy(&self, manager: &ProxyManager, refresh: bool) -> crate::Result<()> {
        let http = self
            .inner
            .core
            .active_proxy("http")
            .ok_or_else(|| Error::other("请先启动带 HTTP 或混合入站的内核，再开启系统代理"))?;
        let socks = self.inner.core.active_proxy("socks");
        let settings = self.settings();
        let model = self
            .inner
            .core
            .applied_model()
            .unwrap_or_else(|| self.model());
        let pac_url = if system::pac::use_pac(
            settings.pac_enabled,
            model.work_mode,
            self.inner.core.native_tun_active(),
        ) {
            let mut direct_domains = Vec::new();
            let mut direct_cidrs = vec![
                "10.0.0.0/8".to_string(),
                "172.16.0.0/12".to_string(),
                "192.168.0.0/16".to_string(),
            ];
            for rule in model.routing.effective_rules() {
                if rule.action.kind == crate::model::RouteActionKind::Direct {
                    direct_domains.extend(rule.matchers.domains.iter().cloned());
                    direct_domains.extend(rule.matchers.domain_suffixes.iter().cloned());
                    direct_cidrs.extend(rule.matchers.ip_cidrs.iter().cloned());
                }
            }
            let pac = system::build_pac(&http.host, http.port, &direct_domains, &direct_cidrs);

            let plan = self.inner.core.mesh_plan();
            let mut running = self.inner.pac.lock();
            let url = if let Some(server) = running
                .as_mut()
                .filter(|s| s.info.port == settings.pac_port)
            {
                server.update_script(pac, &plan);
                server.info.url.clone()
            } else {
                let server = PacServer::start(pac, settings.pac_port)?;
                server.update_mesh(&plan);
                let url = server.info.url.clone();
                *running = Some(server);
                url
            };
            Some(url)
        } else {
            None
        };
        let socks = socks.as_ref().map(|p| (p.host.as_str(), p.port));
        manager.apply_endpoints(&http.host, http.port, socks, pac_url.as_deref(), refresh)?;
        #[cfg(target_os = "macos")]
        {
            self.inner.proxy_observation.write().expect(
                &http.host,
                http.port,
                socks,
                pac_url.as_deref(),
            );
        }
        if pac_url.is_none() {
            self.inner.pac.lock().take();
        }
        Ok(())
    }

    pub(super) fn system_proxy_status(&self) -> Option<system::proxy::SystemProxyStatus> {
        #[cfg(target_os = "macos")]
        {
            self.inner.proxy_observation.read().status()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    #[cfg(target_os = "macos")]
    pub fn observe_system_proxy(&self) -> Option<impl Drop> {
        let observation = self.inner.proxy_observation.clone();
        system::proxy::observer::subscribe(observation)
    }

    pub(super) fn close_system_proxy(&self) {
        let managed = {
            let mut requests = self.inner.proxy_requests.lock();
            requests.closing = true;
            requests.next = None;
            requests.managed
        };
        if managed {
            let manager = self.inner.proxy.lock();
            if let Err(err) = manager.restore() {
                self.toast(ToastLevel::Error, format!("退出时还原系统代理失败：{err}"));
            } else {
                self.record_proxy_state(false);
            }
        }
        self.inner.pac.lock().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_os_change_keeps_snapshot_responsive_and_honors_last_request() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let backend = service.inner.proxy.lock();
        service.set_system_proxy(true);
        service.set_system_proxy(false);
        service.reapply_system_proxy();
        let view = service.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let snapshot = view.snapshot();
            tx.send((
                snapshot.busy.iter().any(|key| key == "proxy"),
                snapshot.system_proxy_active,
            ))
            .unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(3));
        let next = service.inner.proxy_requests.lock().next;
        drop(backend);
        task.join().unwrap();
        assert_eq!(result.unwrap(), (true, false));
        assert!(next == Some(Change::Restore));
        service.close_system_proxy();
        assert!(!service.inner.paths.proxy_snapshot().exists());
    }
}
