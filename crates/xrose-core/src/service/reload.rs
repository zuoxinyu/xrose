//! Pending configuration reloads. A failed attempt stays dirty until the next
//! edit or an explicit retry, so a bad configuration cannot cause a restart loop.

use super::*;
use std::time::{Duration, Instant};

const EDIT_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct ReloadState {
    pub(super) dirty: bool,
    pub(super) due: Option<Instant>,
}

impl ReloadState {
    fn schedule(&mut self, enabled: bool, now: Instant) {
        self.due = (enabled && self.dirty).then_some(now + EDIT_DELAY);
    }

    fn ready(&self, now: Instant) -> bool {
        self.dirty && self.due.is_some_and(|due| due <= now)
    }
}

impl AppService {
    /// Save events and the core becoming idle wake this worker. Network probes
    /// and statistics polling can take seconds and must not gate reloads.
    pub(super) fn start_reload_worker(&self) {
        let weak = Arc::downgrade(&self.inner);
        let mut wake = self.inner.reload_wake.subscribe();
        self.spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let service = AppService { inner };
                let deadline = if service.is_running() && !service.is_busy("core") {
                    service.inner.reload.lock().due
                } else {
                    None
                };
                drop(service);

                if let Some(deadline) = deadline {
                    tokio::select! {
                        changed = wake.changed() => if changed.is_err() { break },
                        _ = tokio::time::sleep_until(deadline.into()) => {
                            let Some(inner) = weak.upgrade() else { break };
                            AppService { inner }.apply_config_with_tun_cleanup(false, true);
                        }
                    }
                } else if wake.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    pub(super) fn mark_dirty(&self) {
        if self.is_running() || self.is_busy("core") {
            let settings = self.inner.settings.read();
            let mut reload = self.inner.reload.lock();
            reload.dirty = true;
            reload.schedule(settings.hot_reload, Instant::now());
            self.inner.reload_wake.send_replace(());
        }
    }

    pub(super) fn reload_preference_changed(&self) {
        let settings = self.inner.settings.read();
        let mut reload = self.inner.reload.lock();
        reload.schedule(settings.hot_reload && self.is_running(), Instant::now());
        self.inner.reload_wake.send_replace(());
    }

    pub(super) fn finish_config_apply(&self, model: &AppModel, settings: &Settings) {
        let current_model = self.inner.model.read();
        let current_settings = self.inner.settings.read();
        let mut reload = self.inner.reload.lock();
        reload.dirty = *current_model != *model
            || current_settings.requires_core_restart(settings, &current_model);
        reload.schedule(
            current_settings.hot_reload && self.is_running(),
            Instant::now(),
        );
    }

    /// Claim the operation and consume the queued edit together. A concurrent
    /// edit starts a new debounce period even while the operation is in flight.
    pub(super) fn claim_config_apply(&self, automatic: bool) -> bool {
        let settings = self.inner.settings.read();
        let mut busy = self.inner.busy.write();
        if busy.contains("core") {
            return false;
        }
        let mut reload = self.inner.reload.lock();
        if automatic
            && (!settings.hot_reload || !self.is_running() || !reload.ready(Instant::now()))
        {
            return false;
        }
        busy.insert("core".into());
        reload.due = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_helper_update_requires_opt_in_and_a_known_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let mut status = HelperStatus {
            supported: true,
            installed: true,
            running: true,
            needs_update: true,
            elevated: false,
            detail: String::new(),
            executable: None,
        };
        assert!(!service.claim_helper_update(&status));
        let mut settings = service.settings();
        settings.hot_reload = true;
        service.update_settings(settings);
        status.installed = false;
        assert!(!service.claim_helper_update(&status));
        status.installed = true;
        status.needs_update = false;
        assert!(!service.claim_helper_update(&status));
        status.needs_update = true;
        service.set_busy("helper", true);
        assert!(!service.claim_helper_update(&status));
        service.set_busy("helper", false);
        assert!(service.claim_helper_update(&status));
        service.set_busy("helper", false);
        assert!(!service.claim_helper_update(&status));
        assert!(
            Settings::load(&service.paths().settings_file())
                .unwrap()
                .hot_reload
        );
        assert!(!service.snapshot().config_dirty);
    }

    #[cfg(unix)]
    #[test]
    fn running_core_reloads_saved_edits_and_keeps_working_config_on_failure() {
        use crate::model::Protocol;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        #[track_caller]
        fn wait(until: impl Fn() -> bool) {
            let start = Instant::now();
            while !until() {
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "timed out waiting for core operation"
                );
                std::thread::sleep(Duration::from_millis(40));
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let binary = dir.path().join("test-core");
        fs::write(
            &binary,
            r#"#!/bin/sh
for config do :; done
case "$*" in
  api\ statsquery*) echo '{}'; exit 0 ;;
  version) echo 'Xray 26.6.27'; exit 0 ;;
  check\ *|*-test*)
    grep -q '"port": 19444' "$config" && exit 1
    exit 0 ;;
esac
grep -q '"port": 19446' "$config" && exit 1
exec sleep 90
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        for asset in ["geoip.dat", "geosite.dat"] {
            fs::write(dir.path().join(asset), "fixture").unwrap();
        }
        let mut settings = service.settings();
        settings.core = CoreKind::Xray;
        settings
            .binary_paths
            .insert("xray".into(), binary.display().to_string());
        service.update_settings(settings);
        service
            .commit_model_change(|model| {
                model.add_nodes([
                    Node::new("Example", Protocol::Socks, "example.test", 1080),
                    Node::new("Other", Protocol::Socks, "other.test", 1080),
                ]);
                Ok(true)
            })
            .unwrap();
        service.start_core();
        wait(|| !service.is_busy("core") && service.process_state().pid.is_some());
        // An explicit selection reloads even with automatic reload disabled.
        let direct_pid = service.process_state().pid;
        let node_id = service.model().nodes[0].id.clone();
        service.select_node("proxy", &node_id);
        wait(|| {
            !service.is_busy("core")
                && service.inner.core.applied_work_mode() == Some(WorkMode::Proxy)
        });
        assert_ne!(service.process_state().pid, direct_pid);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Proxy);
        assert_eq!(
            service.snapshot().runtime.running_proxy_node.as_ref(),
            Some(&node_id)
        );
        service.set_node_enabled(&node_id, false);
        assert!(service.model().node(&node_id).unwrap().enabled);
        assert!(!service.snapshot().config_dirty);
        let other_id = service.model().nodes[1].id.clone();
        service.set_nodes_enabled([node_id.as_str(), other_id.as_str()], false);
        assert!(service.model().node(&node_id).unwrap().enabled);
        assert!(!service.model().node(&other_id).unwrap().enabled);
        let measured = service.tray_probe_target().unwrap();
        service.record_proxy_probe(Some(&measured), "another-node", false);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Proxy);
        service.record_proxy_probe(Some(&measured), &node_id, false);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Unavailable);
        service.record_proxy_probe(Some(&measured), &node_id, true);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Proxy);
        service.select_direct_mode().unwrap();
        wait(|| {
            !service.is_busy("core")
                && service.inner.core.applied_work_mode() == Some(WorkMode::Direct)
        });
        assert_eq!(
            service.model().group("proxy").unwrap().selected.as_deref(),
            Some(node_id.as_str())
        );
        service.record_proxy_probe(Some(&measured), &node_id, false);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Direct);
        assert!(service.snapshot().runtime.running_proxy_node.is_none());
        service.set_node_enabled(&node_id, false);
        assert!(!service.model().node(&node_id).unwrap().enabled);
        let original_pid = service.process_state().pid;
        let mut settings = service.settings();
        settings.mixed_port = 17442;
        service.update_settings(settings.clone());
        assert!(service.snapshot().config_dirty && !service.snapshot().auto_reload_pending);
        assert_eq!(service.process_state().pid, original_pid);

        // Enabling the switch consumes an existing pending edit. Newer edits during
        // the debounce period must be included in the same running configuration.
        settings.hot_reload = true;
        service.update_settings(settings.clone());
        settings.mixed_port = 17443;
        service.update_settings(settings.clone());
        wait(|| !service.is_busy("core") && !service.snapshot().config_dirty);
        let good_pid = service.process_state().pid.unwrap();
        assert_ne!(Some(good_pid), original_pid);
        let config = service.paths().generated_config("xray");
        let good = fs::read_to_string(&config).unwrap();
        assert!(good.contains("\"port\": 17443"));

        // Save a second change while the replacement is starting. Its completion
        // must not clear the newer edit or require a manual apply.
        settings.mixed_port = 17450;
        service.update_settings(settings.clone());
        wait(|| {
            service.is_busy("core")
                && service
                    .process_state()
                    .pid
                    .is_some_and(|pid| pid != good_pid)
        });
        service.set_dns_enabled(false);
        wait(|| !service.is_busy("core") && !service.snapshot().config_dirty);
        let (applied, _, _) = service.inner.core.statistics_context().unwrap();
        assert!(!applied.dns.enabled);
        let good_pid = service.process_state().pid.unwrap();
        let good = fs::read_to_string(&config).unwrap();

        settings.mixed_port = 19444;
        service.update_settings(settings.clone());
        wait(|| !service.is_busy("core") && service.snapshot().validation.is_some_and(|r| !r.ok));
        assert_eq!(service.process_state().pid, Some(good_pid));
        assert_eq!(fs::read_to_string(&config).unwrap(), good);
        assert!(service.snapshot().config_dirty && !service.snapshot().auto_reload_pending);

        // Validation succeeds but the new process exits. Roll back the executable
        // configuration and leave the edited intent available for correction.
        settings.mixed_port = 19446;
        service.update_settings(settings.clone());
        wait(|| {
            !service.is_busy("core")
                && service
                    .active_toasts()
                    .iter()
                    .any(|t| t.message.contains("已恢复原代理配置"))
        });
        assert!(service.is_running());
        assert_eq!(fs::read_to_string(&config).unwrap(), good);
        assert_eq!(service.settings().mixed_port, 19446);

        settings.hot_reload = false;
        settings.mixed_port = 17445;
        service.update_settings(settings);
        service.apply_config();
        wait(|| !service.is_busy("core"));
        assert!(!service.snapshot().config_dirty);
        assert!(
            fs::read_to_string(config)
                .unwrap()
                .contains("\"port\": 17445")
        );
        // Changing the selected core must validate the new format and publish
        // its own runtime file, even though the old process used a different one.
        let sets = service.paths().rule_sets_dir();
        fs::create_dir_all(&sets).unwrap();
        for set in crate::adapters::required_rule_sets(&service.model()) {
            fs::write(sets.join(format!("{}.srs", set.tag)), b"fixture").unwrap();
        }
        let api = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut settings = service.settings();
        settings.api_port = api.local_addr().unwrap().port();
        settings.core = CoreKind::SingBox;
        settings
            .binary_paths
            .insert(settings.core.as_str().into(), binary.display().to_string());
        settings.hot_reload = true;
        service.update_settings(settings);
        wait(|| {
            service.process_state().core == Some(CoreKind::SingBox)
                && !service.is_busy("core")
                && !service.snapshot().config_dirty
        });
        assert_eq!(
            service.inner.core.last_config_path(),
            Some(service.paths().generated_config("sing-box"))
        );
        drop(api);
        service.stop_core();
        wait(|| !service.is_running() && !service.is_busy("core"));
    }
}
