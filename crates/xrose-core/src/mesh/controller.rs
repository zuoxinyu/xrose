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
    pub(crate) profiles: RwLock<Vec<MeshProfile>>,
    pub(crate) sync_password: MeshSecret,
    pub(crate) sync_port: std::sync::atomic::AtomicU16,
    networks: RwLock<Vec<MeshRuntimeView>>,
    installed: RwLock<bool>,
    pub operation: tokio::sync::Mutex<()>,
}
impl MeshController {
    pub fn new(paths: AppPaths, store: Arc<Store>) -> Result<Self> {
        let profiles = store.mesh_profiles()?;
        let installed = super::tailscale::archive_path(&paths).exists();
        Ok(Self {
            sync_password: MeshSecret(uuid::Uuid::new_v4().simple().to_string()),
            sync_port: Default::default(),
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
        Some(outbound_node(profile))
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
            if let Some(node) = self.outbound_node(&id) {
                upsert_outbound(model, node);
            }
        }
    }
    pub(crate) fn commit_exit(
        &self,
        profile: MeshProfile,
        model: &mut crate::model::AppModel,
        group_id: &str,
        active_profile_id: &str,
    ) -> Result<()> {
        let mut profiles = self.profiles.write();
        let current = profiles
            .iter_mut()
            .find(|p| p.id == profile.id)
            .ok_or_else(|| Error::config("组网配置不存在"))?;
        let mut next = model.clone();
        let node = outbound_node(&profile);
        let node_id = node.id.clone();
        upsert_outbound(&mut next, node);
        if next.node(&node_id).is_none() || !next.can_select_mesh_exit(group_id, &node_id) {
            return Err(Error::config("请选择包含此出口节点的已启用手动出站组"));
        }
        next.select_node(group_id, &node_id);
        next.work_mode = crate::model::WorkMode::Proxy;
        next.updated_at = crate::util::now_unix();
        self.store
            .save_mesh_exit_selection(&profile, &next, active_profile_id)?;
        *model = next;
        *current = profile;
        Ok(())
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
        let mut profiles = self.profiles.write();
        self.store.delete_mesh_profile(id)?;
        profiles.retain(|p| p.id != id);
        Ok(())
    }
    pub(crate) fn forward_edit(
        &self,
        id: &str,
        old: Option<&super::ForwardRule>,
        new: Option<super::ForwardRule>,
    ) -> Result<(super::ForwardRules, super::ForwardRules)> {
        use super::{ForwardRule, ForwardRules};
        let mut profile = self.profile(id)?;
        let original = ForwardRules::from_profile(&profile);
        let mut next = original.clone();
        if let Some(old) = old {
            fn remove<T: PartialEq>(rules: &mut Vec<T>, old: &T) -> Result<()> {
                let index = rules
                    .iter()
                    .position(|r| r == old)
                    .ok_or_else(|| Error::other("这条转发已被更改，请重新打开"))?;
                rules.remove(index);
                Ok(())
            }
            match old {
                ForwardRule::ToMesh(r) => remove(&mut next.local, r)?,
                ForwardRule::ToHost(r) => remove(&mut next.services, r)?,
            }
        }
        match new {
            Some(ForwardRule::ToMesh(r)) => next.local.push(r),
            Some(ForwardRule::ToHost(r)) => next.services.push(r),
            None => {}
        }
        next.assign(&mut profile);
        profile.validate()?;
        Ok((original, next))
    }

    /// The caller holds the mesh operation lock throughout live acknowledgement and persistence.
    pub(crate) async fn apply_forward_edit(
        &self,
        id: &str,
        old: &super::ForwardRules,
        next: &super::ForwardRules,
    ) -> Result<()> {
        if super::ForwardRules::from_profile(&self.profile(id)?) != *old {
            return Err(Error::other("转发列表已变更，请重新选择"));
        }
        let live = self.networks.read().iter().any(|n| {
            n.profile_id == id
                && !matches!(
                    n.phase,
                    super::MeshPhase::Disconnected | super::MeshPhase::Failed
                )
        });
        if live {
            if let Err(e) = self.live_forwards(id, next).await {
                // Also reconcile an unacknowledged request (e.g. a lost reply).
                return self.restore_forwards(id, old, e).await;
            }
        }
        let saved = (|| -> Result<()> {
            let mut profiles = self.profiles.write();
            let profile = profiles
                .iter_mut()
                .find(|p| p.id == id && super::ForwardRules::from_profile(p) == *old)
                .ok_or_else(|| Error::other("组网配置已变更，请重新选择"))?;
            let mut updated = profile.clone();
            next.assign(&mut updated);
            self.store.save_mesh_profile(&updated)?;
            *profile = updated;
            Ok(())
        })();
        if live && let Err(e) = saved {
            return self.restore_forwards(id, old, e).await;
        }
        saved
    }
    async fn restore_forwards(
        &self,
        id: &str,
        rules: &super::ForwardRules,
        cause: Error,
    ) -> Result<()> {
        if self.live_forwards(id, rules).await.is_err() {
            return Err(Error::other(format!(
                "{cause}。未能确认当前监听状态，请重新连接组网"
            )));
        }
        Err(cause)
    }
    async fn live_forwards(&self, profile_id: &str, rules: &super::ForwardRules) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send(MeshCommand::SetForwards {
            profile_id: profile_id.into(),
            id: id.clone(),
            rules: rules.clone(),
        })
        .await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            {
                let networks = self.networks.read();
                let state = networks
                    .iter()
                    .find(|n| n.profile_id == profile_id)
                    .filter(|n| {
                        !matches!(
                            n.phase,
                            super::MeshPhase::Failed | super::MeshPhase::Disconnected
                        )
                    })
                    .ok_or_else(|| Error::other("组网已断开，请重新保存端口转发"))?;
                if let Some(ack) = state.forward_update.as_ref().filter(|r| r.id == id) {
                    return match &ack.error {
                        Some(e) => Err(Error::other(e.clone())),
                        None => Ok(()),
                    };
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::other("等待端口转发更新超时"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            self.send(MeshCommand::Status).await?;
        }
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
            return Err(Error::other("组网助手版本不兼容，请在设置中更新提权助手"));
        }
        *self.networks.write() = reply.networks;
        Ok(())
    }
    pub(crate) fn profile(&self, id: &str) -> Result<MeshProfile> {
        self.profiles
            .read()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| Error::config("组网配置不存在"))
    }
    /// Test legacy helpers before changing any proxy or network state.
    pub(crate) async fn verify_protocol(&self) -> Result<()> {
        self.send(MeshCommand::Status).await.map_err(|error| {
            Error::other(format!(
                "无法确认提权助手支持当前组网协议，请在设置中更新助手后重试：{error}"
            ))
        })
    }

    pub async fn connect(&self, id: &str, network: &crate::updates::UpdateNetwork) -> Result<()> {
        self.start_profile(self.profile(id)?, network, None).await
    }
    pub(crate) async fn start_profile(
        &self,
        profile: MeshProfile,
        _network: &crate::updates::UpdateNetwork,
        expected_exit: Option<&str>,
    ) -> Result<()> {
        let id = profile.id.clone();
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
            sync_password: self.sync_password.clone(),
            sync_port: self.sync_port.load(std::sync::atomic::Ordering::Acquire),
            driver_archive,
        })
        .await?;
        // A spawned worker is not a started backend. Wait for the startup
        // result so a failed child cannot be followed by a success toast.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            self.send(MeshCommand::Status).await?;
            if startup_ready(&self.networks.read(), &id, expected_exit)? {
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
                state.finish(Some("与组网助手失去联系，请重启助手后重新连接".into()));
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

fn outbound_node(profile: &MeshProfile) -> crate::model::Node {
    let mut node = crate::model::Node::new(
        format!("{} · 出口", profile.name),
        crate::model::Protocol::Socks,
        "127.0.0.1",
        profile.outbound_port,
    );
    node.id = format!("mesh-exit-{}", profile.id);
    node.credentials.username = Some("xrose".into());
    node.credentials.password = Some(profile.outbound_secret.0.clone());
    node.tags = vec!["mesh-exit".into()];
    node
}

/// Install or update the gateway without resetting its group membership.
fn upsert_outbound(model: &mut crate::model::AppModel, mut node: crate::model::Node) {
    if let Some(old) = model.node_mut(&node.id) {
        node.enabled = old.enabled;
        node.sort = old.sort;
        *old = node;
    } else {
        model.add_nodes([node]);
    }
}

fn startup_ready(
    states: &[MeshRuntimeView],
    id: &str,
    expected_exit: Option<&str>,
) -> Result<bool> {
    use super::MeshPhase;
    match states.iter().find(|state| state.profile_id == id) {
        Some(state) => match state.phase {
            MeshPhase::Connected => Ok(expected_exit.is_none_or(|id| {
                state
                    .peers
                    .iter()
                    .any(|p| p.id == id && p.online && p.exit_selected && p.offers_exit)
            })),
            MeshPhase::NeedsLogin | MeshPhase::NeedsApproval if expected_exit.is_some() => {
                Err(Error::other("请先完成组网设备授权再切换出口"))
            }
            MeshPhase::NeedsLogin | MeshPhase::NeedsApproval => Ok(true),
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
    fn snapshot_lists_only_saved_profiles_and_stays_empty_after_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        let open = || MeshController::new(paths.clone(), Store::open(&paths).unwrap()).unwrap();
        let controller = open();
        let initial = controller.snapshot();
        assert!(initial.profiles.is_empty());
        assert!(initial.networks.is_empty());

        let id = controller
            .save(MeshProfile {
                provider: MeshProvider::Tailscale,
                ..Default::default()
            })
            .unwrap();
        drop(controller);
        let controller = open();
        let saved = controller.snapshot();
        assert_eq!(saved.profiles.len(), 1);
        assert_eq!(saved.profiles[0].id, id);
        assert!(saved.networks.is_empty());
        controller.delete(&id).unwrap();
        drop(controller);
        let reopened = open().snapshot();
        assert!(reopened.profiles.is_empty());
        assert!(reopened.networks.is_empty());
    }

    #[test]
    fn connection_waits_for_backend_and_propagates_startup_failure() {
        use super::super::MeshPhase;
        let mut state = MeshRuntimeView {
            profile_id: "requested".into(),
            phase: MeshPhase::Starting,
            ..Default::default()
        };
        assert!(!startup_ready(&[state.clone()], "requested", None).unwrap());
        state.phase = MeshPhase::NeedsLogin;
        assert!(startup_ready(&[state.clone()], "requested", None).unwrap());
        state.phase = MeshPhase::Connected;
        assert!(startup_ready(&[state.clone()], "requested", None).unwrap());
        state.phase = MeshPhase::Failed;
        state.error = Some("模拟组件提前退出".into());
        assert!(
            startup_ready(&[state], "requested", None)
                .unwrap_err()
                .to_string()
                .contains("模拟组件提前退出")
        );
        assert!(startup_ready(&[], "requested", None).is_err());
    }

    #[test]
    fn switch_waits_for_the_chosen_peer_and_saves_both_profiles_together() {
        use crate::mesh::{MeshPeer, MeshPhase};
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        let store = Store::open(&paths).unwrap();
        let controller = MeshController::new(paths, store.clone()).unwrap();
        let id = controller
            .save(MeshProfile {
                provider: MeshProvider::Tailscale,
                ..Default::default()
            })
            .unwrap();
        let old = controller.profile(&id).unwrap();
        let mut state = MeshRuntimeView {
            profile_id: id.clone(),
            phase: MeshPhase::Connected,
            peers: vec![MeshPeer {
                id: "chosen".into(),
                online: true,
                offers_exit: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!startup_ready(std::slice::from_ref(&state), &id, Some("chosen")).unwrap());
        state.peers[0].exit_selected = true;
        assert!(!startup_ready(std::slice::from_ref(&state), &id, Some("different")).unwrap());
        assert!(startup_ready(std::slice::from_ref(&state), &id, Some("chosen")).unwrap());
        state.peers[0].online = false;
        assert!(!startup_ready(&[state], &id, Some("chosen")).unwrap());

        let mut model = crate::model::AppModel::default();
        let ordinary = crate::model::Node::new(
            "ordinary",
            crate::model::Protocol::Socks,
            "example.test",
            1080,
        );
        model.add_nodes([ordinary.clone()]);
        let mut group =
            crate::model::OutboundGroup::new("测速组", crate::model::GroupKind::UrlTest);
        group.id = "auto".into();
        group.members = vec![ordinary.id.clone()];
        model.groups.push(group);
        let active = store.save_active_profile(&model).unwrap();
        let next = MeshProfile {
            exit_node: "100.64.0.2".into(),
            outbound_port: 19081,
            ..old.clone()
        };
        for (group, profile) in [("auto", active.as_str()), ("proxy", "stale-profile")] {
            assert!(
                controller
                    .commit_exit(next.clone(), &mut model, group, profile)
                    .is_err()
            );
            assert_eq!(model.nodes.len(), 1);
            assert_eq!(store.mesh_profiles().unwrap()[0], old);
        }
        let gateway = format!("mesh-exit-{id}");
        controller
            .commit_exit(next.clone(), &mut model, "proxy", &active)
            .unwrap();
        let next = MeshProfile {
            exit_node: "100.64.0.3".into(),
            ..next
        };
        controller
            .commit_exit(next.clone(), &mut model, "proxy", &active)
            .unwrap();
        assert_eq!(model.nodes.len(), 2);
        assert_eq!(store.mesh_profiles().unwrap()[0], next);
        let saved = store.load_profile(&active).unwrap().unwrap().model;
        assert_eq!(saved.work_mode, crate::model::WorkMode::Proxy);
        assert_eq!(saved.group("proxy").unwrap().active_node(), Some(&gateway));
        assert_eq!(saved.node(&gateway).unwrap().port, 19081);
        assert!(model.select_node("proxy", &ordinary.id));
    }

    #[tokio::test]
    async fn disconnected_forward_edits_preserve_profile_and_reject_stale_rules() {
        use super::super::PortForward;
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        let store = Store::open(&paths).unwrap();
        let controller = MeshController::new(paths, store.clone()).unwrap();
        let id = controller
            .save(MeshProfile {
                provider: MeshProvider::Tailscale,
                hostname: "fixture".into(),
                ..Default::default()
            })
            .unwrap();
        let original = controller.profile(&id).unwrap();
        let mut published = original.clone();
        published
            .service_forwards
            .push(super::super::ServiceForward {
                protocol: super::super::ForwardProtocol::Tcp,
                port: 18080,
                target: "127.0.0.1:8080".parse().unwrap(),
            });
        controller.networks.write().push(MeshRuntimeView {
            profile_id: id.clone(),
            phase: super::super::MeshPhase::Connected,
            ..Default::default()
        });
        assert!(controller.save(published).is_err());
        assert_eq!(controller.profile(&id).unwrap(), original);
        controller.networks.write().clear();
        let tcp = super::super::ForwardRule::ToMesh(
            PortForward::parse("tcp://127.0.0.1:15000/192.0.2.2:80").unwrap(),
        );
        let udp = super::super::ForwardRule::ToMesh(
            PortForward::parse("udp://127.0.0.1:15000/192.0.2.2:53").unwrap(),
        );
        let (old, next) = controller
            .forward_edit(&id, None, Some(tcp.clone()))
            .unwrap();
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        let (_, both) = controller
            .forward_edit(&id, None, Some(udp.clone()))
            .unwrap();
        controller
            .apply_forward_edit(&id, &next, &both)
            .await
            .unwrap();
        assert!(
            controller
                .apply_forward_edit(&id, &next, &super::super::ForwardRules::default())
                .await
                .is_err()
        );
        assert!(
            controller
                .forward_edit(&id, Some(&udp), Some(tcp.clone()))
                .is_err()
        );
        assert_eq!(store.mesh_profiles().unwrap()[0].port_forwards, both.local);
        let (old, next) = controller.forward_edit(&id, Some(&tcp), None).unwrap();
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        assert!(controller.forward_edit(&id, Some(&tcp), None).is_err());
        let (old, next) = controller.forward_edit(&id, Some(&udp), None).unwrap();
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        // A single edit can change direction and persist both lists together.
        let service = super::super::ForwardRule::ToHost(super::super::ServiceForward {
            protocol: super::super::ForwardProtocol::Tcp,
            port: 18080,
            target: "127.0.0.1:8080".parse().unwrap(),
        });
        let (old, next) = controller
            .forward_edit(&id, None, Some(service.clone()))
            .unwrap();
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        let (old, next) = controller
            .forward_edit(&id, Some(&service), Some(tcp.clone()))
            .unwrap();
        assert!(next.services.is_empty());
        assert_eq!(next.local.len(), 1);
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        assert!(controller.forward_edit(&id, Some(&service), None).is_err());
        let (old, next) = controller.forward_edit(&id, Some(&tcp), None).unwrap();
        controller
            .apply_forward_edit(&id, &old, &next)
            .await
            .unwrap();
        assert_eq!(store.mesh_profiles().unwrap(), vec![original]);
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
            routes: vec!["192.0.2.0/24".into()],
            peers: vec![super::super::MeshPeer {
                online: true,
                ..Default::default()
            }],
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
        assert!(state.routes.is_empty() && state.peers.is_empty());
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
