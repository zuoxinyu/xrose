//! Host mesh mode selection and isolated runtime policy.
use super::{AppService, ToastLevel};
use crate::{
    Error, Result,
    adapters::{CompileContext, MeshExitRuntime},
    mesh::{MeshPhase, MeshProfile, MeshProvider, MeshSecret},
    model::{AppModel, MeshDnsMode, MeshExitTarget, WorkMode},
};

impl AppService {
    pub fn select_mesh_mode(&self) -> Result<()> {
        let target = self
            .model()
            .mesh_mode
            .target
            .ok_or_else(|| Error::config("请先在节点页选择一个组网出口"))?;
        self.select_mesh_exit_mode(target.profile_id, target.peer_id);
        Ok(())
    }

    pub fn set_mesh_dns_mode(&self, mode: MeshDnsMode) -> Result<()> {
        let changed = self.commit_model_change(|model| {
            if model.mesh_mode.dns == mode {
                return Ok(false);
            }
            model.mesh_mode.dns = mode;
            Ok(true)
        })?;
        if changed && self.model().work_mode == WorkMode::Mesh && self.is_running() {
            self.apply_config();
        }
        Ok(())
    }

    pub fn set_mesh_allow_lan(&self, enabled: bool) -> Result<()> {
        let changed = self.commit_model_change(|model| {
            if model.mesh_mode.allow_lan == enabled {
                return Ok(false);
            }
            model.mesh_mode.allow_lan = enabled;
            Ok(true)
        })?;
        if changed && self.model().work_mode == WorkMode::Mesh && self.is_running() {
            self.apply_config();
        }
        Ok(())
    }

    pub fn select_mesh_exit_mode(&self, profile_id: String, peer_id: String) {
        {
            let mut busy = self.inner.busy.write();
            if busy.contains("core") || busy.contains("mesh") {
                return;
            }
            busy.extend(["core".into(), "mesh".into(), "mesh-exit".into()]);
        }
        let service = self.clone();
        self.spawn(async move {
            let result = service.switch_system_exit(&profile_id, &peer_id).await;
            match result {
                Ok(()) => service.notify_work_mode(WorkMode::Mesh),
                Err(error) => service.toast(ToastLevel::Error, error.to_string()),
            }
            service.set_busy("mesh-exit", false);
            service.set_busy("mesh", false);
            service.set_busy("core", false);
        });
    }

    pub(super) fn select_saved_mesh_gateway(&self, node_id: &str) -> bool {
        let Some(profile_id) = node_id.strip_prefix("mesh-exit-") else {
            return false;
        };
        let Ok(profile) = self.inner.mesh.profile(profile_id) else {
            return false;
        };
        let snapshot = self.inner.mesh.snapshot();
        let peer = snapshot
            .exits()
            .find(|(p, _, address)| p.id == profile_id && address.to_string() == profile.exit_node)
            .map(|(_, peer, _)| peer.id.clone());
        if let Some(peer) = peer {
            self.select_mesh_exit_mode(profile_id.into(), peer);
        } else if let Some(target) = self
            .model()
            .mesh_mode
            .target
            .filter(|t| t.profile_id == profile_id && t.address == profile.exit_node)
        {
            self.select_mesh_exit_mode(target.profile_id, target.peer_id);
        } else {
            self.toast(ToastLevel::Error, "请先连接组网，等待出口设备上线后再选择");
        }
        true
    }

    async fn switch_system_exit(&self, profile_id: &str, peer_id: &str) -> Result<()> {
        let _operation = self.inner.mesh.operation.lock().await;
        self.ensure_helper().await.map_err(Error::other)?;
        self.inner.mesh.refresh().await?;
        let old_model = self.model();
        let old_runtime = self.inner.core.mesh_exit_runtime();
        let previous_client = self.inner.mesh_client_profile.lock().clone();
        let active = self
            .inner
            .active_profile
            .read()
            .clone()
            .ok_or_else(|| Error::config("请先选择配置"))?;
        let old_profile = self.inner.mesh.profile(profile_id)?;
        if old_profile.advertise_exit_node {
            return Err(Error::config("同一组网不能同时发布本机出口并使用远端出口"));
        }
        let snapshot = self.inner.mesh.snapshot();
        let previous_network = snapshot
            .networks
            .iter()
            .find(|n| {
                n.provider == old_profile.provider
                    && !matches!(n.phase, MeshPhase::Disconnected | MeshPhase::Failed)
            })
            .and_then(|n| self.inner.mesh.profile(&n.profile_id).ok())
            .map(|profile| {
                previous_client
                    .as_ref()
                    .filter(|p| p.id == profile.id)
                    .cloned()
                    .unwrap_or(profile)
            });
        let (peer, address) = snapshot
            .exits()
            .find(|(p, peer, _)| p.id == profile_id && peer.id == peer_id && peer.online)
            .map(|(_, peer, address)| (peer.clone(), address.to_string()))
            .or_else(|| {
                old_model
                    .mesh_mode
                    .target
                    .as_ref()
                    .filter(|t| t.profile_id == profile_id && t.peer_id == peer_id)
                    .map(|t| {
                        (
                            crate::mesh::MeshPeer {
                                id: t.peer_id.clone(),
                                name: t.name.clone(),
                                ..Default::default()
                            },
                            t.address.clone(),
                        )
                    })
            })
            .ok_or_else(|| Error::config("出口设备已离线或停止提供出口，请重新选择"))?;
        let mut backing = old_profile.clone();
        if backing.outbound_port == 0 {
            backing.outbound_port =
                super::mesh_exit::available_port(&old_model, &self.settings(), &snapshot.profiles)?;
        }
        if backing.outbound_secret.0.is_empty() {
            backing.outbound_secret = MeshSecret(uuid::Uuid::new_v4().simple().to_string());
        }
        // Existing split-routing targets remain persistent and are restored on mode exit.
        if backing.exit_node.is_empty() {
            backing.exit_node = address.clone();
        }
        let target = MeshExitTarget {
            profile_id: profile_id.into(),
            peer_id: peer_id.into(),
            provider: backing.provider,
            name: peer.name,
            address: address.clone(),
            gateway_id: format!("mesh-exit-{profile_id}"),
        };
        self.check_exit_policy_target_conflict(&backing, &address)
            .await?;
        let mut runtime_profile = backing.clone();
        runtime_profile.exit_node = address;
        runtime_profile.validate()?;
        let was_running = self.is_running();
        let mut restored_previous_provider = false;
        let result = async {
            self.inner.mesh.disconnect(backing.provider).await?;
            self.inner
                .mesh
                .start_profile(
                    runtime_profile.clone(),
                    &self.resource_network(),
                    Some(peer_id),
                )
                .await?;
            let mut candidate = old_model.clone();
            candidate.work_mode = WorkMode::Mesh;
            candidate.mesh_mode.target = Some(target.clone());
            let runtime = self.mesh_exit_runtime(&candidate, &runtime_profile)?;
            // The gateway identity is also exposed for outgroup editing without changing selections.
            if candidate.node(&runtime.node.id).is_none() {
                candidate.nodes.push(runtime.node.clone());
            }
            self.reconcile_mesh().await?;
            self.inner.core.set_mesh_exit(Some(runtime));
            let applied_settings = self.activate_mesh_runtime(&candidate, was_running).await?;
            self.suspend_proxy_for_mesh().await?;
            // Another provider can remain connected, but must recover its persistent split target.
            if let Some(previous) = previous_client
                .as_ref()
                .filter(|p| p.provider != backing.provider)
            {
                let saved = self.inner.mesh.profile(&previous.id)?;
                if saved.exit_node != previous.exit_node {
                    restored_previous_provider = true;
                    self.inner.mesh.disconnect(saved.provider).await?;
                    self.inner
                        .mesh
                        .start_profile(saved, &self.resource_network(), None)
                        .await?;
                    self.reconcile_mesh().await?;
                }
            }
            if self.inner.active_profile.read().as_deref() != Some(active.as_str()) {
                return Err(Error::other(
                    "活动配置已变更，已取消本次出口切换，请重新选择",
                ));
            }
            let (edited_during_switch, committed) = {
                let mut model = self.inner.model.write();
                let edited = *model != old_model;
                self.inner
                    .mesh
                    .commit_system_exit(backing.clone(), &mut model, target, &active)?;
                (edited, model.clone())
            };
            self.inner.mesh_reconnect.lock().remember(
                backing.provider,
                profile_id,
                backing.auto_reconnect,
            );
            *self.inner.mesh_client_profile.lock() = Some(runtime_profile.clone());
            self.inner
                .mesh_takeover_requested
                .store(true, std::sync::atomic::Ordering::Release);
            self.finish_config_apply(&committed, &applied_settings);
            if edited_during_switch {
                self.mark_dirty();
            }
            Ok::<(), Error>(())
        }
        .await;
        if let Err(error) = result {
            let _ = self.inner.mesh.disconnect(backing.provider).await;
            let mut restored = true;
            if let Some(profile) = previous_network {
                restored &= self
                    .inner
                    .mesh
                    .start_profile(profile, &self.resource_network(), None)
                    .await
                    .is_ok();
            }
            if restored_previous_provider {
                if let Some(previous) = previous_client {
                    let _ = self.inner.mesh.disconnect(previous.provider).await;
                    restored &= self
                        .inner
                        .mesh
                        .start_profile(previous, &self.resource_network(), None)
                        .await
                        .is_ok();
                }
            }
            self.inner.core.set_mesh_exit(old_runtime);
            if was_running {
                restored &= self
                    .inner
                    .core
                    .reload(&old_model, &self.settings(), async {})
                    .await
                    .is_ok_and(|outcome| outcome.started && outcome.report.ok);
            } else {
                restored &= self.inner.core.stop().await.is_ok();
            }
            restored &= self.reconcile_mesh().await.is_ok();
            if old_model.work_mode != WorkMode::Mesh {
                self.inner
                    .mesh_proxy_suspended
                    .store(false, std::sync::atomic::Ordering::Release);
                if self.settings().system_proxy {
                    self.set_system_proxy(true);
                }
            }
            return Err(Error::other(if restored {
                format!("系统出口切换失败，已恢复原连接：{error}")
            } else {
                format!("系统出口切换失败：{error}；原运行状态未能完全恢复，请检查组网和内核状态")
            }));
        }
        self.refresh_doctor();
        Ok(())
    }

    async fn activate_mesh_runtime(
        &self,
        model: &AppModel,
        running: bool,
    ) -> Result<crate::settings::Settings> {
        let mut settings = self.settings();
        self.check_exit_gateway_tun_conflict(model, &settings)
            .await?;
        let mut ctx = CompileContext::new(
            &settings,
            self.paths().data_dir.clone(),
            self.paths().runtime_dir.clone(),
        );
        ctx.mesh_exit = self.inner.core.mesh_exit_runtime();
        ctx.mesh = self.inner.core.mesh_plan();
        crate::adapters::adapter_for(settings.core).compile(model, &settings, &ctx)?;
        self.ensure_core_binary(&mut settings).await?;
        self.ensure_rule_sets(model, &settings)
            .await
            .map_err(Error::other)?;
        let outcome = if running {
            self.inner.core.reload(model, &settings, async {}).await?
        } else {
            self.inner
                .core
                .start(model, &settings, settings.core)
                .await?
        };
        if !self.record_outcome(&outcome) || !outcome.started {
            return Err(Error::other("组网接管配置未通过校验，请查看内核诊断"));
        }
        self.inner.traffic.lock().reset();
        self.inner.connections.write().clear();
        *self.inner.proxies.write() = Default::default();
        Ok(settings)
    }

    pub(super) fn mesh_exit_runtime(
        &self,
        model: &AppModel,
        profile: &MeshProfile,
    ) -> Result<MeshExitRuntime> {
        let target = model
            .mesh_mode
            .target
            .as_ref()
            .ok_or_else(|| Error::config("尚未选择组网出口"))?;
        if profile.provider == MeshProvider::EasyTier
            && self.inner.mesh.helper_executable().is_none()
        {
            return Err(Error::config(
                "无法确认组网工作进程，请更新提权助手并重新连接组网",
            ));
        }
        let mut node = crate::mesh::controller::outbound_node(profile);
        node.id = target.gateway_id.clone();
        node.name = target.name.clone();
        node.enabled = true;
        Ok(MeshExitRuntime {
            node,
            transport_process: self.inner.mesh.helper_executable(),
            dns_address: if profile.provider == MeshProvider::Tailscale {
                Some(
                    format!("{}:53", crate::mesh::DNS_ADDRESS)
                        .parse()
                        .map_err(|_| Error::config("组网 DNS 地址无效"))?,
                )
            } else {
                None
            },
            ipv6: profile.provider == MeshProvider::Tailscale,
        })
    }

    /// Prepare connected mesh transport before a host TUN starts or is reloaded.
    pub(super) async fn prepare_mesh_mode(&self, model: &AppModel) -> Result<()> {
        self.check_exit_gateway_tun_conflict(model, &self.settings())
            .await?;
        if model.work_mode != WorkMode::Mesh {
            return Ok(());
        }
        let target = model
            .mesh_mode
            .target
            .as_ref()
            .ok_or_else(|| Error::config("请先选择组网出口"))?;
        let mut profile = self.inner.mesh.profile(&target.profile_id)?;
        profile.exit_node = target.address.clone();
        let connected = self.inner.mesh.snapshot().networks.iter().any(|n| {
            n.profile_id == profile.id
                && n.phase == MeshPhase::Connected
                && n.peers
                    .iter()
                    .any(|p| p.id == target.peer_id && p.exit_selected && p.online)
        });
        if !connected {
            self.ensure_helper().await.map_err(Error::other)?;
            let _operation = self.inner.mesh.operation.lock().await;
            self.inner.mesh.disconnect(profile.provider).await?;
            self.inner
                .mesh
                .start_profile(
                    profile.clone(),
                    &self.resource_network(),
                    Some(&target.peer_id),
                )
                .await?;
            self.reconcile_mesh().await?;
        }
        self.inner
            .core
            .set_mesh_exit(Some(self.mesh_exit_runtime(model, &profile)?));
        self.inner.mesh_reconnect.lock().remember(
            profile.provider,
            &profile.id,
            profile.auto_reconnect,
        );
        *self.inner.mesh_client_profile.lock() = Some(profile);
        Ok(())
    }
    pub(super) fn finish_idle_mode_switch(&self) {
        if self.inner.mesh_client_profile.lock().is_none()
            || !self.inner.busy.write().insert("core".into())
        {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            if let Err(error) = service.restore_mesh_split_target().await {
                service.toast(ToastLevel::Error, format!("恢复组网分流出口失败：{error}"));
            }
            // The main core is stopped: keep desktop proxy disabled until it starts again.
            service.set_busy("core", false);
        });
    }
    pub(super) async fn restore_mesh_split_target(&self) -> Result<()> {
        let previous = self.inner.mesh_client_profile.lock().clone();
        if let Some(previous) = previous {
            let _operation = self.inner.mesh.operation.lock().await;
            let saved = self.inner.mesh.profile(&previous.id)?;
            let still_connected = self.inner.mesh.snapshot().networks.iter().any(|n| {
                n.profile_id == previous.id
                    && !matches!(n.phase, MeshPhase::Disconnected | MeshPhase::Failed)
                    && !n.user_stopped
            });
            if still_connected && saved.exit_node != previous.exit_node {
                self.inner.mesh.disconnect(saved.provider).await?;
                self.connect_mesh_worker(saved).await?;
            }
            self.inner.mesh_client_profile.lock().take();
        }
        self.inner.core.set_mesh_exit(None);
        Ok(())
    }
}
