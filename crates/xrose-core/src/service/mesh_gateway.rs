//! Dedicated routing processes for traffic received from mesh peers.
use super::*;
use crate::mesh::{ExitBridge, ExitRoutingMode, MeshProfile};
use std::net::{Ipv4Addr, TcpListener};

pub(super) struct ExitGateway {
    core: Arc<CoreManager>,
    bridge: ExitBridge,
    model: AppModel,
    settings: Settings,
}

fn policy_model(model: &AppModel) -> AppModel {
    let mut policy = model.clone();
    policy.work_mode = WorkMode::Proxy;
    policy.inbounds.clear();
    policy.mesh_mode = Default::default();
    policy
}

fn reserve_ports() -> Result<[u16; 3]> {
    let sockets = (0..3)
        .map(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
        .collect::<std::io::Result<Vec<_>>>()?;
    Ok([
        sockets[0].local_addr()?.port(),
        sockets[1].local_addr()?.port(),
        sockets[2].local_addr()?.port(),
    ])
}

fn references_gateway(model: &AppModel, node_id: &str) -> bool {
    model
        .groups
        .iter()
        .any(|group| group.enabled && group.members.iter().any(|id| id == node_id))
        || model.dns.servers.iter().any(|server| {
            server.detour.as_deref().is_some_and(|detour| {
                detour == node_id
                    || model
                        .node(node_id)
                        .is_some_and(|node| node.outbound_tag() == detour)
            })
        })
}

fn host_captures_system(model: &AppModel, settings: &Settings) -> bool {
    model.work_mode == WorkMode::Mesh
        || settings.effective_tun()
        || model
            .inbounds
            .iter()
            .any(|inbound| inbound.enabled && inbound.kind == InboundKind::Tun)
}

impl AppService {
    pub(super) async fn check_exit_gateway_tun_conflict(
        &self,
        model: &AppModel,
        settings: &Settings,
    ) -> Result<()> {
        if host_captures_system(model, settings)
            && self
                .inner
                .mesh_gateways
                .lock()
                .await
                .values()
                .any(|g| g.settings.core == CoreKind::Xray)
        {
            return Err(Error::config(
                "Xray 出口分流不能与本机 TUN 同时运行，请将内核切换为 sing-box，或先断开提供出口分流的组网",
            ));
        }
        Ok(())
    }

    pub(super) fn refresh_exit_gateway_status(&self) {
        let Ok(gateways) = self.inner.mesh_gateways.try_lock() else {
            return;
        };
        let mut errors = self.inner.mesh_connection_errors.write();
        for (id, gateway) in gateways.iter() {
            if gateway.core.state().phase == ProcessPhase::Failed {
                errors.insert(
                    id.clone(),
                    "出口分流内核已停止，远端流量暂不可用；请检查日志后重新连接组网".into(),
                );
            } else if gateway.core.state().phase == ProcessPhase::Running
                && errors
                    .get(id)
                    .is_some_and(|reason| reason.starts_with("出口分流内核已停止"))
            {
                errors.remove(id);
            }
        }
    }

    pub(super) async fn check_exit_policy_target_conflict(
        &self,
        profile: &MeshProfile,
        address: &str,
    ) -> Result<()> {
        if !profile.exit_node.is_empty() && profile.exit_node != address {
            let gateway_id = format!("mesh-exit-{}", profile.id);
            if self
                .inner
                .mesh_gateways
                .lock()
                .await
                .values()
                .any(|g| references_gateway(&g.model, &gateway_id))
            {
                return Err(Error::config(
                    "正在运行的出口分流服务引用了该网络的另一个出口，请保持相同出口或先停止该出口服务",
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn prepare_exit_gateway(&self, profile: &MeshProfile) -> Result<()> {
        let mut gateways = self.inner.mesh_gateways.lock().await;
        if !profile.advertise_exit_node || profile.exit_routing != ExitRoutingMode::Rules {
            self.inner.mesh.set_exit_bridge(&profile.id, None);
            if let Some(gateway) = gateways.remove(&profile.id) {
                gateway.core.stop().await?;
            }
            return Ok(());
        }
        let saved_model = self.model();
        if saved_model.work_mode == WorkMode::Mesh {
            if let Some(target) = &saved_model.mesh_mode.target {
                let saved_profile = self.inner.mesh.profile(&target.profile_id)?;
                if saved_profile.exit_node != target.address
                    && references_gateway(&saved_model, &target.gateway_id)
                {
                    return Err(Error::config(
                        "出口分流引用的组网出口与本机系统出口不同，请先使用相同出口或切回代理模式",
                    ));
                }
            }
        }
        let model = policy_model(&saved_model);
        let mut settings = self.settings();
        if settings.core == CoreKind::Xray && host_captures_system(&saved_model, &settings) {
            return Err(Error::config(
                "Xray 出口分流不能与本机 TUN 同时使用，请切换为 sing-box，或先关闭 TUN 并退出组网模式",
            ));
        }
        self.ensure_core_binary(&mut settings).await?;
        self.ensure_rule_sets(&model, &settings)
            .await
            .map_err(Error::other)?;
        settings.tun_enabled = false;
        settings.system_proxy = false;
        settings.allow_lan = false;
        settings.separate_lan_port = false;
        settings.pac_enabled = false;
        let previous = gateways.get(&profile.id);
        let new_gateway = previous.is_none();
        let bridge = if let Some(previous) = previous {
            settings.mixed_port = previous.bridge.port;
            settings.api_port = previous.settings.api_port;
            settings.stats_port = previous.settings.stats_port;
            previous.bridge.clone()
        } else {
            let [port, api, stats] = reserve_ports()?;
            settings.mixed_port = port;
            settings.api_port = api;
            settings.stats_port = stats;
            ExitBridge {
                port,
                password: crate::mesh::MeshSecret(uuid::Uuid::new_v4().simple().to_string()),
            }
        };
        if let Some(previous) = previous {
            if previous.model == model
                && !settings.requires_core_restart(&previous.settings, &model)
                && previous.core.state().phase == ProcessPhase::Running
            {
                self.inner.mesh.set_exit_bridge(&profile.id, Some(bridge));
                return Ok(());
            }
        }
        let core = if let Some(previous) = previous {
            previous.core.clone()
        } else {
            let mut paths = self.paths().clone();
            paths.runtime_dir = paths.runtime_dir.join("mesh-exit-policy").join(&profile.id);
            paths.ensure()?;
            Arc::new(CoreManager::new(paths, self.inner.core.logs().clone()))
        };
        core.set_exit_bridge(bridge.clone());
        let outcome = if core.state().phase.is_alive() {
            core.reload(&model, &settings, async {}).await?
        } else {
            core.start(&model, &settings, settings.core).await?
        };
        if !outcome.started || !outcome.report.ok {
            return Err(Error::other(
                "出口分流配置未通过校验，请检查路由、DNS 和可用出站组",
            ));
        }
        self.inner
            .mesh
            .set_exit_bridge(&profile.id, Some(bridge.clone()));
        if new_gateway {
            self.start_exit_node_switch_scheduler(core.clone());
        }
        gateways.insert(
            profile.id.clone(),
            ExitGateway {
                core,
                bridge,
                model,
                settings,
            },
        );
        Ok(())
    }

    pub(super) fn schedule_exit_gateway_refresh(&self) {
        self.refresh_exit_gateway_policies(false);
    }

    pub(super) fn refresh_exit_gateway_policies(&self, force: bool) {
        if !force && !self.settings().hot_reload {
            return;
        }
        self.inner
            .exit_policy_revision
            .fetch_add(1, Ordering::AcqRel);
        if !self.inner.busy.write().insert("exit-policy".into()) {
            return;
        }
        let service = self.clone();
        self.spawn(async move {
            loop {
                let revision = service.inner.exit_policy_revision.load(Ordering::Acquire);
                let _operation = service.inner.mesh.operation.lock().await;
                let snapshot = service.inner.mesh.snapshot();
                let profiles: Vec<_> = service
                    .inner
                    .mesh
                    .profiles
                    .read()
                    .iter()
                    .filter(|p| p.advertise_exit_node && p.exit_routing == ExitRoutingMode::Rules)
                    .filter(|p| {
                        snapshot.networks.iter().any(|n| {
                            n.profile_id == p.id && n.phase == crate::mesh::MeshPhase::Connected
                        })
                    })
                    .cloned()
                    .collect();
                for profile in profiles {
                    if let Err(error) = service.prepare_exit_gateway(&profile).await {
                        service.notify(
                            ToastLevel::Error,
                            format!("出口分流更新失败，保留已应用配置：{error}"),
                        );
                    }
                }
                // Claim release and generation check share the busy gate with producers.
                let mut busy = service.inner.busy.write();
                if revision == service.inner.exit_policy_revision.load(Ordering::Acquire) {
                    busy.remove("exit-policy");
                    break;
                }
            }
        });
    }

    pub(super) async fn stop_exit_gateways(&self) {
        let mut gateways = self.inner.mesh_gateways.lock().await;
        for (id, gateway) in std::mem::take(&mut *gateways) {
            self.inner.mesh.set_exit_bridge(&id, None);
            let _ = gateway.core.stop().await;
        }
    }

    pub(super) async fn stop_exit_gateway(&self, profile_id: &str) {
        let gateway = self.inner.mesh_gateways.lock().await.remove(profile_id);
        self.inner.mesh.set_exit_bridge(profile_id, None);
        if let Some(gateway) = gateway {
            let _ = gateway.core.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tun_conflict_detects_forced_mesh_and_saved_tun_independently() {
        let mut model = AppModel::default();
        let mut settings = Settings::default();
        settings.tun_enabled = false;
        assert!(!host_captures_system(&model, &settings));
        let tun = model
            .inbounds
            .iter_mut()
            .find(|inbound| inbound.kind == InboundKind::Tun)
            .unwrap();
        tun.enabled = true;
        assert!(host_captures_system(&model, &settings));
        model
            .inbounds
            .iter_mut()
            .filter(|inbound| inbound.kind == InboundKind::Tun)
            .for_each(|inbound| inbound.enabled = false);
        model.work_mode = WorkMode::Mesh;
        assert!(host_captures_system(&model, &settings));
        model.work_mode = WorkMode::Proxy;
        settings.tun_enabled = true;
        assert!(host_captures_system(&model, &settings));
    }
    #[test]
    fn dns_only_gateway_references_cannot_be_retargeted_silently() {
        let mut model = AppModel::default();
        let node = Node::new(
            "Fixture exit",
            crate::model::Protocol::Socks,
            "127.0.0.1",
            19081,
        );
        model.nodes.push(node.clone());
        assert!(!references_gateway(&model, &node.id));
        model.dns.servers[0].detour = Some(node.id.clone());
        assert!(references_gateway(&model, &node.id));
        model.dns.servers[0].detour = Some(node.outbound_tag());
        assert!(references_gateway(&model, &node.id));
    }
    #[test]
    fn remote_policy_is_independent_of_host_mode_and_inbounds() {
        let mut saved = AppModel::default();
        saved.work_mode = WorkMode::Mesh;
        saved.inbounds.iter_mut().for_each(|i| i.enabled = true);
        let policy = policy_model(&saved);
        assert_eq!(policy.work_mode, WorkMode::Proxy);
        assert!(policy.inbounds.is_empty());
        assert_eq!(policy.routing, saved.routing);
        assert_eq!(policy.dns, saved.dns);
        assert_eq!(saved.work_mode, WorkMode::Mesh);
    }
}
