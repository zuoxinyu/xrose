//! Unprivileged mesh controller. Only cached, redacted values reach the UI.
use super::{
    MeshProfile, MeshProvider, MeshRuntimeView, MeshSecret, MeshSnapshot, PROTOCOL_VERSION,
    ipc::{self, MeshCommand, MeshRequest},
};
use crate::{Error, Result, paths::AppPaths, storage::Store};
use parking_lot::RwLock;
use std::sync::Arc;
pub struct MeshController {
    paths: AppPaths,
    store: Arc<Store>,
    session: MeshSecret,
    profiles: RwLock<Vec<MeshProfile>>,
    networks: RwLock<Vec<MeshRuntimeView>>,
    installed: RwLock<bool>,
    pub operation: tokio::sync::Mutex<()>,
}
impl MeshController {
    pub fn new(paths: AppPaths, store: Arc<Store>) -> Result<Self> {
        let profiles = store.mesh_profiles()?;
        let installed = super::tailscale::archive_path(&paths).exists();
        Ok(Self {
            paths,
            store,
            session: MeshSecret(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )),
            profiles: RwLock::new(profiles),
            networks: RwLock::new(Vec::new()),
            installed: RwLock::new(installed),
            operation: tokio::sync::Mutex::new(()),
        })
    }
    pub fn snapshot(&self) -> MeshSnapshot {
        MeshSnapshot {
            profiles: self
                .profiles
                .read()
                .iter()
                .map(MeshProfile::summary)
                .collect(),
            networks: self.networks.read().clone(),
            tailscale_installed: *self.installed.read(),
            supported: cfg!(any(windows, target_os = "macos", target_os = "linux")),
        }
    }
    pub fn save(&self, mut profile: MeshProfile) -> Result<String> {
        if self.networks.read().iter().any(|n| {
            n.profile_id == profile.id
                && !matches!(
                    n.phase,
                    super::MeshPhase::Disconnected | super::MeshPhase::Failed
                )
        }) {
            return Err(Error::other("请先断开组网，再编辑连接配置"));
        }
        let mut profiles = self.profiles.write();
        if profile.network_secret.0.is_empty() {
            if let Some(old) = profiles.iter().find(|p| p.id == profile.id) {
                profile.network_secret = old.network_secret.clone();
            }
        }
        if profile.outbound_secret.0.is_empty() {
            profile.outbound_secret = profiles
                .iter()
                .find(|p| p.id == profile.id)
                .map(|p| p.outbound_secret.clone())
                .filter(|s| !s.0.is_empty())
                .unwrap_or_else(|| MeshSecret(uuid::Uuid::new_v4().simple().to_string()));
        }
        if profile.outbound_port != 0
            && profiles
                .iter()
                .any(|p| p.id != profile.id && p.outbound_port == profile.outbound_port)
        {
            return Err(Error::config("该出口出站端口已被其他组网配置使用"));
        }
        self.store.save_mesh_profile(&profile)?;
        let id = profile.id.clone();
        profiles.retain(|p| p.id != id);
        profiles.push(profile);
        Ok(id)
    }
    pub fn outbound_node(&self, id: &str) -> Option<crate::model::Node> {
        let profiles = self.profiles.read();
        let profile = profiles
            .iter()
            .find(|p| p.id == id && p.outbound_port != 0)?;
        let mut node = crate::model::Node::new(
            format!("{} · 出口", profile.name),
            crate::model::Protocol::Socks,
            "127.0.0.1",
            profile.outbound_port,
        );
        node.id = format!("mesh-exit-{}", profile.id);
        node.credentials.username = Some("xross".into());
        node.credentials.password = Some(profile.outbound_secret.0.clone());
        node.tags = vec!["mesh-exit".into()];
        Some(node)
    }

    /// Stable IDs keep group membership while profile ports/credentials change.
    pub fn sync_outbounds(&self, model: &mut crate::model::AppModel) {
        let ids: Vec<_> = self
            .profiles
            .read()
            .iter()
            .filter(|p| p.outbound_port != 0)
            .map(|p| p.id.clone())
            .collect();
        let stale: Vec<_> = model
            .nodes
            .iter()
            .filter(|n| {
                n.id.starts_with("mesh-exit-")
                    && n.tags.iter().any(|t| t == "mesh-exit")
                    && !ids.iter().any(|id| n.id == format!("mesh-exit-{id}"))
            })
            .map(|n| n.id.clone())
            .collect();
        for id in stale {
            model.remove_node(&id);
        }
        for id in ids {
            if let Some(mut node) = self.outbound_node(&id) {
                if let Some(old) = model.nodes.iter_mut().find(|n| n.id == node.id) {
                    node.enabled = old.enabled;
                    node.sort = old.sort;
                    *old = node;
                } else {
                    model.add_nodes([node]);
                }
            }
        }
    }
    pub fn delete(&self, id: &str) -> Result<()> {
        if self.networks.read().iter().any(|n| {
            n.profile_id == id
                && !matches!(
                    n.phase,
                    super::MeshPhase::Disconnected | super::MeshPhase::Failed
                )
        }) {
            return Err(Error::other("请先断开组网，再删除配置"));
        }
        self.store.delete_mesh_profile(id)?;
        self.profiles.write().retain(|p| p.id != id);
        Ok(())
    }
    fn request(&self, command: MeshCommand) -> MeshRequest {
        MeshRequest {
            version: PROTOCOL_VERSION,
            owner_pid: std::process::id(),
            session: self.session.clone(),
            command,
        }
    }
    async fn send(&self, command: MeshCommand) -> Result<()> {
        let request = self.request(command);
        let reply = tokio::task::spawn_blocking(move || ipc::call(request))
            .await
            .map_err(|_| Error::other("组网助手任务异常"))??;
        if reply.version != PROTOCOL_VERSION {
            return Err(Error::other("组网助手版本不兼容，请更新并重启助手"));
        }
        *self.networks.write() = reply.networks;
        Ok(())
    }
    pub async fn connect(&self, id: &str, _network: &crate::updates::UpdateNetwork) -> Result<()> {
        let profile = self
            .profiles
            .read()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| Error::config("组网配置不存在"))?;
        if profile.provider == MeshProvider::Tailscale {
            super::tailscale::verify_binary(&super::tailscale::archive_path(&self.paths))?;
        }
        let binary = (profile.provider == MeshProvider::Tailscale)
            .then(|| super::tailscale::archive_path(&self.paths));
        #[cfg(windows)]
        let driver_archive = super::driver::install(&self.paths, _network).await?;
        #[cfg(not(windows))]
        let driver_archive = std::path::PathBuf::new();
        let provider = profile.provider;
        self.send(MeshCommand::Start {
            profile,
            tailscale_binary: binary,
            driver_archive,
        })
        .await?;
        // A spawned worker is not a started backend. Wait for the startup
        // result so a failed child cannot be followed by a success toast.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            self.send(MeshCommand::Status).await?;
            if startup_ready(&self.networks.read(), id)? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                self.send(MeshCommand::Stop { provider }).await?;
                return Err(Error::other("组网启动超时，已停止本次连接，请重试"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    pub async fn refresh(&self) -> Result<()> {
        self.refresh_with(self.send(MeshCommand::Status)).await
    }
    async fn refresh_with(
        &self,
        poll: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        if self.networks.read().iter().all(|n| {
            matches!(
                n.phase,
                super::MeshPhase::Disconnected | super::MeshPhase::Failed
            )
        }) {
            return Ok(());
        }
        if let Err(error) = poll.await {
            for state in self.networks.write().iter_mut() {
                state.phase = super::MeshPhase::Failed;
                state.error = Some("与组网助手失去联系，请重启助手后重新连接".into());
                state.login_url = None;
            }
            return Err(error);
        }
        Ok(())
    }
    pub async fn disconnect(&self, provider: MeshProvider) -> Result<()> {
        self.send(MeshCommand::Stop { provider }).await
    }
    pub async fn logout(&self, provider: MeshProvider) -> Result<()> {
        self.send(MeshCommand::Logout { provider }).await
    }
    pub async fn probe(&self, provider: MeshProvider, ip: String) -> Result<()> {
        self.send(MeshCommand::Probe { provider, ip }).await
    }
    pub async fn install(&self, network: &crate::updates::UpdateNetwork) -> Result<()> {
        super::tailscale::install(&self.paths, network).await?;
        *self.installed.write() = true;
        Ok(())
    }
    pub fn shutdown(&self) {
        if !self.networks.read().is_empty() {
            let _ = ipc::call(self.request(MeshCommand::StopAll));
        }
        self.networks.write().clear();
    }
}

fn startup_ready(states: &[MeshRuntimeView], id: &str) -> Result<bool> {
    use super::MeshPhase;
    match states.iter().find(|state| state.profile_id == id) {
        Some(state) => match state.phase {
            MeshPhase::Connected | MeshPhase::NeedsLogin => Ok(true),
            MeshPhase::Failed => Err(Error::other(
                state.error.clone().unwrap_or_else(|| "组网启动失败".into()),
            )),
            MeshPhase::Disconnected | MeshPhase::Stopping => Err(Error::other("组网启动已中止")),
            MeshPhase::Starting => Ok(false),
        },
        None => Err(Error::other("组网助手未返回本次连接的状态")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_waits_for_backend_and_propagates_startup_failure() {
        use super::super::MeshPhase;
        let mut state = MeshRuntimeView {
            profile_id: "requested".into(),
            phase: MeshPhase::Starting,
            ..Default::default()
        };
        assert!(!startup_ready(&[state.clone()], "requested").unwrap());
        state.phase = MeshPhase::NeedsLogin;
        assert!(startup_ready(&[state.clone()], "requested").unwrap());
        state.phase = MeshPhase::Connected;
        assert!(startup_ready(&[state.clone()], "requested").unwrap());
        state.phase = MeshPhase::Failed;
        state.error = Some("模拟组件提前退出".into());
        assert!(
            startup_ready(&[state], "requested")
                .unwrap_err()
                .to_string()
                .contains("模拟组件提前退出")
        );
        assert!(startup_ready(&[], "requested").is_err());
    }

    #[tokio::test]
    async fn helper_loss_fails_connections_clears_login_and_stops_polling() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        let controller = MeshController::new(paths.clone(), Store::open(&paths).unwrap()).unwrap();
        *controller.networks.write() = vec![MeshRuntimeView {
            phase: super::super::MeshPhase::NeedsLogin,
            login_url: Some("https://example.test/private-login".into()),
            ..Default::default()
        }];
        assert!(
            controller
                .refresh_with(async { Err(Error::other("模拟助手失联")) })
                .await
                .is_err()
        );
        let state = &controller.snapshot().networks[0];
        assert_eq!(state.phase, super::super::MeshPhase::Failed);
        assert!(state.login_url.is_none());
        assert!(state.error.as_ref().unwrap().contains("失去联系"));
        let polled = std::cell::Cell::new(false);
        controller
            .refresh_with(async {
                polled.set(true);
                Err(Error::other("不应再次轮询"))
            })
            .await
            .unwrap();
        assert!(!polled.get());
    }
}
