//! Restore only networks requested in this app session after their worker is lost.
use super::{AppService, ToastLevel};
use crate::{
    Result,
    mesh::{MeshPhase, MeshProfile, MeshProvider, MeshSnapshot},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const FIRST_RETRY: Duration = Duration::from_secs(3);
const MAX_RETRY: Duration = Duration::from_secs(60);

struct Target {
    id: String,
    due: Instant,
    delay: Duration,
}

#[derive(Default)]
pub(super) struct ReconnectState {
    targets: BTreeMap<MeshProvider, Target>,
    closed: bool,
}

#[derive(PartialEq, Eq)]
enum Connection {
    Cancelled,
    Live(MeshPhase),
    Missing,
}

fn connection(mesh: &MeshSnapshot, provider: MeshProvider, id: &str) -> Connection {
    if !mesh
        .profiles
        .iter()
        .any(|p| p.id == id && p.provider == provider && p.auto_reconnect)
    {
        return Connection::Cancelled;
    }
    match mesh.networks.iter().find(|n| n.provider == provider) {
        Some(n) if n.profile_id == id && n.user_stopped => Connection::Cancelled,
        Some(n) if !matches!(n.phase, MeshPhase::Failed | MeshPhase::Disconnected) => {
            if n.profile_id == id {
                Connection::Live(n.phase)
            } else {
                Connection::Cancelled
            }
        }
        _ => Connection::Missing,
    }
}

impl ReconnectState {
    pub(super) fn remember(&mut self, provider: MeshProvider, id: &str, enabled: bool) {
        self.targets.remove(&provider);
        if enabled && !self.closed {
            self.targets.insert(
                provider,
                Target {
                    id: id.to_owned(),
                    due: Instant::now(),
                    delay: FIRST_RETRY,
                },
            );
        }
    }

    pub(super) fn supersede(&mut self, mesh: &MeshSnapshot, provider: MeshProvider, id: &str) {
        if self
            .targets
            .get(&provider)
            .is_some_and(|t| t.id != id && connection(mesh, provider, &t.id) == Connection::Missing)
        {
            self.targets.remove(&provider);
        }
    }

    pub(super) fn forget(&mut self, id: &str) {
        self.targets.retain(|_, target| target.id != id);
    }

    pub(super) fn forget_provider(&mut self, provider: MeshProvider) {
        self.targets.remove(&provider);
    }

    pub(super) fn close(&mut self) {
        self.closed = true;
        self.targets.clear();
    }

    pub(super) fn pending(&self, mesh: &MeshSnapshot) -> BTreeSet<String> {
        self.targets
            .iter()
            .filter(|(provider, t)| connection(mesh, **provider, &t.id) == Connection::Missing)
            .map(|(_, t)| t.id.clone())
            .collect()
    }

    fn next(&mut self, mesh: &MeshSnapshot, now: Instant) -> Option<(MeshProvider, String)> {
        self.targets.retain(
            |provider, target| match connection(mesh, *provider, &target.id) {
                Connection::Cancelled => false,
                Connection::Live(MeshPhase::Connected) if now >= target.due => {
                    target.delay = FIRST_RETRY;
                    true
                }
                _ => true,
            },
        );
        self.targets
            .iter()
            .filter(|(provider, t)| {
                t.due <= now && connection(mesh, **provider, &t.id) == Connection::Missing
            })
            .min_by_key(|(_, t)| t.due)
            .map(|(provider, t)| (*provider, t.id.clone()))
    }

    fn defer(&mut self, provider: MeshProvider, id: &str, now: Instant) {
        if let Some(target) = self.targets.get_mut(&provider).filter(|t| t.id == id) {
            target.due = now + target.delay;
            target.delay = (target.delay * 2).min(MAX_RETRY);
        }
    }
}

impl AppService {
    pub fn cancel_mesh_reconnect(&self, id: &str) {
        self.inner.mesh_reconnect.lock().forget(id);
        self.inner.mesh_connection_errors.write().remove(id);
    }

    /// Called after polling, with the mesh operation lock held. Slow starts run off the monitor.
    pub(super) fn schedule_mesh_reconnect(&self) {
        if self.inner.mesh_reconnect.lock().targets.is_empty() {
            return;
        }
        let mesh = self.inner.mesh.snapshot();
        let Some((provider, id)) = self.inner.mesh_reconnect.lock().next(&mesh, Instant::now())
        else {
            return;
        };
        let helper = self.helper_status();
        if !helper.running || helper.needs_update {
            return;
        }
        {
            let mut busy = self.inner.busy.write();
            if busy.contains("helper") || busy.contains("core") || !busy.insert("mesh".into()) {
                return;
            }
        }
        let service = self.clone();
        self.spawn(async move {
            let _operation = service.inner.mesh.operation.lock().await;
            let result = service.retry_mesh_connection(provider, &id).await;
            service
                .inner
                .mesh_reconnect
                .lock()
                .defer(provider, &id, Instant::now());
            if let Err(error) = result {
                service
                    .inner
                    .mesh_connection_errors
                    .write()
                    .insert(id, error.to_string());
            }
            service.set_busy("mesh", false);
        });
    }

    async fn retry_mesh_connection(&self, provider: MeshProvider, id: &str) -> Result<()> {
        // A missed status response can leave a healthy worker behind. Confirm before starting.
        let Ok(_helper) = self.inner.helper_lock.try_lock() else {
            return Ok(());
        };
        self.inner
            .mesh
            .poll_status()
            .await
            .map_err(|_| crate::Error::other("暂时无法获取组网状态，提权助手就绪后将自动重试"))?;
        let mesh = self.inner.mesh.snapshot();
        if !self.inner.mesh_reconnect.lock().pending(&mesh).contains(id) {
            if mesh
                .networks
                .iter()
                .any(|n| n.profile_id == id && n.phase == MeshPhase::Connected)
            {
                self.inner.mesh_connection_errors.write().remove(id);
            }
            return Ok(());
        }
        self.connect_mesh_worker(self.inner.mesh.profile(id)?)
            .await?;
        if !self
            .inner
            .mesh_reconnect
            .lock()
            .targets
            .get(&provider)
            .is_some_and(|target| target.id == id)
        {
            self.inner.mesh.disconnect(provider).await?;
            return self.reconcile_mesh().await;
        }
        self.inner.mesh_connection_errors.write().remove(id);
        self.toast(
            ToastLevel::Info,
            format!("已重新启动 {} 组网连接", provider.label()),
        );
        Ok(())
    }

    pub(super) async fn connect_mesh_worker(&self, mut profile: MeshProfile) -> Result<()> {
        self.prepare_exit_gateway(&profile).await?;
        if self.model().work_mode == crate::model::WorkMode::Mesh {
            if let Some(target) = self
                .model()
                .mesh_mode
                .target
                .filter(|t| t.profile_id == profile.id)
            {
                profile.exit_node = target.address;
            }
        }
        let profile_id = profile.id.clone();
        let mut preflight = self.inner.core.mesh_plan();
        preflight.active = true;
        if let Err(error) = self.apply_mesh_plan(preflight).await {
            self.stop_exit_gateway(&profile_id).await;
            return Err(error);
        }
        if let Err(error) = self
            .inner
            .mesh
            .start_profile(profile, &self.resource_network(), None)
            .await
        {
            self.stop_exit_gateway(&profile_id).await;
            let _ = self.reconcile_mesh().await;
            return Err(error);
        }
        self.reconcile_mesh().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{MeshProfile, MeshRuntimeView};

    fn network(provider: MeshProvider, auto_reconnect: bool) -> MeshProfile {
        MeshProfile {
            provider,
            auto_reconnect,
            ..Default::default()
        }
    }

    fn worker(profile: &MeshProfile, phase: MeshPhase) -> MeshRuntimeView {
        MeshRuntimeView {
            profile_id: profile.id.clone(),
            provider: profile.provider,
            phase,
            ..Default::default()
        }
    }

    #[test]
    fn recovers_only_requested_profiles_and_rechecks_surviving_workers() {
        let a = network(MeshProvider::EasyTier, true);
        let b = network(MeshProvider::Tailscale, true);
        let mut mesh = MeshSnapshot {
            profiles: vec![a.summary(), b.summary()],
            networks: vec![
                worker(&a, MeshPhase::Connected),
                worker(&b, MeshPhase::NeedsLogin),
            ],
            ..Default::default()
        };
        let mut recovery = ReconnectState::default();
        recovery.remember(a.provider, &a.id, true);
        recovery.remember(b.provider, &b.id, true);
        let now = Instant::now();
        assert!(recovery.next(&mesh, now).is_none());
        mesh.networks.clear();
        let (first, id) = recovery.next(&mesh, now).unwrap();
        recovery.defer(first, &id, now);
        let (second, _) = recovery.next(&mesh, now).unwrap();
        assert_ne!(first, second);
        for phase in [
            MeshPhase::Starting,
            MeshPhase::NeedsApproval,
            MeshPhase::NeedsLogin,
            MeshPhase::Connected,
            MeshPhase::Stopping,
        ] {
            mesh.networks = vec![worker(&a, phase), worker(&b, phase)];
            assert!(recovery.pending(&mesh).is_empty());
        }
        mesh.networks = vec![
            worker(&a, MeshPhase::Failed),
            worker(&b, MeshPhase::Disconnected),
        ];
        assert_eq!(recovery.pending(&mesh).len(), 2);
        mesh.networks[0].user_stopped = true;
        mesh.profiles.retain(|p| p.id != b.id);
        assert!(recovery.next(&mesh, now).is_none());
        mesh.networks.clear();
        assert!(recovery.next(&mesh, now + MAX_RETRY).is_none());
    }

    #[test]
    fn retries_back_off_and_stop_after_cancellation_opt_out_or_shutdown() {
        let p = network(MeshProvider::EasyTier, true);
        let mut mesh = MeshSnapshot {
            profiles: vec![p.summary()],
            ..Default::default()
        };
        let mut recovery = ReconnectState::default();
        recovery.remember(p.provider, &p.id, true);
        let mut now = Instant::now();
        for seconds in [3, 6, 12, 24, 48, 60, 60] {
            assert!(recovery.next(&mesh, now).is_some());
            recovery.defer(p.provider, &p.id, now);
            now += Duration::from_secs(seconds);
            assert!(
                recovery
                    .next(&mesh, now - Duration::from_millis(1))
                    .is_none()
            );
        }
        recovery.forget(&p.id);
        assert!(recovery.next(&mesh, now).is_none());
        recovery.remember(p.provider, &p.id, true);
        mesh.profiles[0].auto_reconnect = false;
        assert!(recovery.next(&mesh, now).is_none());
        mesh.profiles[0].auto_reconnect = true;
        assert!(recovery.next(&mesh, now).is_none());
        recovery.remember(p.provider, &p.id, true);
        recovery.supersede(&mesh, p.provider, "another-profile");
        assert!(recovery.next(&mesh, now).is_none());
        recovery.close();
        recovery.remember(p.provider, &p.id, true);
        assert!(recovery.next(&mesh, now).is_none());
    }
}
