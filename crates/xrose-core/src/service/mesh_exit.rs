//! Selecting an advertised mesh exit from a manual proxy group.
use super::{AppService, ToastLevel};
use crate::{
    Error, Result,
    mesh::MeshSecret,
    model::{AppModel, inbound::effective_inbounds},
    settings::Settings,
};
use std::{
    collections::HashSet,
    net::{Ipv4Addr, TcpListener},
};

impl AppService {
    pub fn select_mesh_exit(&self, profile_id: String, peer_id: String, group_id: String) {
        {
            let mut busy = self.inner.busy.write();
            if busy.contains("core") || !busy.insert("mesh".into()) {
                return;
            }
            busy.insert("mesh-exit".into());
            busy.insert("core".into());
        }
        let active_profile = self.inner.active_profile.read().clone();
        let service = self.clone();
        self.spawn(async move {
            let _guard = service.inner.mesh.operation.lock().await;
            let result = service
                .switch_mesh_exit(&profile_id, &peer_id, &group_id, active_profile.as_deref())
                .await;
            service.set_busy("core", false);
            service.set_busy("mesh-exit", false);
            service.set_busy("mesh", false);
            match result {
                Ok(()) if service.is_running() => service.apply_config(),
                Ok(()) => service.toast(ToastLevel::Success, "已选中组网出口节点"),
                Err(error) => service.toast(ToastLevel::Error, error.to_string()),
            }
        });
    }

    async fn switch_mesh_exit(
        &self,
        profile_id: &str,
        peer_id: &str,
        group_id: &str,
        active: Option<&str>,
    ) -> Result<()> {
        let active = active.ok_or_else(|| Error::config("请先选择代理配置"))?;
        let mesh = &self.inner.mesh;
        mesh.refresh().await?;
        let snapshot = mesh.snapshot();
        let old = mesh.profile(profile_id)?;
        let (_, _, address) = snapshot
            .exits()
            .find(|(p, peer, _)| p.id == profile_id && peer.id == peer_id && peer.online)
            .ok_or_else(|| Error::config("出口设备已离线或停止提供出口，请重新选择"))?;
        let model = self.model();
        let node_id = format!("mesh-exit-{profile_id}");
        if !model.can_select_mesh_exit(group_id, &node_id) {
            return Err(Error::config(
                "请启用出口节点，并选择包含该节点的手动出站组",
            ));
        }

        let mut next = old.clone();
        next.exit_node = address.to_string();
        if next.outbound_port == 0 {
            next.outbound_port = available_port(&model, &self.settings(), &snapshot.profiles)?;
        }
        if next.outbound_secret.0.is_empty() {
            next.outbound_secret = MeshSecret(uuid::Uuid::new_v4().simple().to_string());
        }
        next.validate()?;
        let network = self.resource_network();
        mesh.disconnect(old.provider).await?;
        let result = async {
            mesh.start_profile(next.clone(), &network, Some(peer_id))
                .await?;
            self.reconcile_mesh().await?;
            mesh.commit_exit(next, &mut self.inner.model.write(), group_id, active)
        }
        .await;
        if let Err(error) = result {
            let restore: Result<()> = async {
                mesh.disconnect(old.provider).await?;
                mesh.start_profile(old, &network, None).await?;
                self.reconcile_mesh().await
            }
            .await;
            if let Err(rollback) = restore {
                let _ = self.reconcile_mesh().await;
                return Err(Error::other(format!(
                    "切换出口失败：{error}。恢复原组网失败，请重新连接：{rollback}"
                )));
            }
            return Err(Error::other(format!("切换出口失败，已恢复原组网：{error}")));
        }
        self.inner.latency.write().remove(&node_id);
        self.inner.downloads.write().remove(&node_id);
        self.refresh_doctor();
        self.mark_dirty();
        Ok(())
    }
}

pub(super) fn proxy_ports(model: &AppModel, settings: &Settings) -> HashSet<u16> {
    let mut ports = HashSet::from([
        settings.mixed_port,
        settings.api_port,
        settings.stats_port,
        settings.pac_port,
    ]);
    for inbound in effective_inbounds(model, settings) {
        if let Ok(listeners) = inbound.listen_ports(settings.core) {
            ports.extend(listeners);
        }
    }
    ports
}

fn available_port(
    model: &AppModel,
    settings: &Settings,
    profiles: &[crate::mesh::MeshProfileView],
) -> Result<u16> {
    let mut reserved = proxy_ports(model, settings);
    for profile in profiles {
        reserved.insert(profile.outbound_port);
        reserved.extend(profile.port_forwards.iter().map(|rule| rule.listen.port()));
    }
    for _ in 0..16 {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        if port >= 1024 && !reserved.contains(&port) {
            return Ok(port);
        }
    }
    Err(Error::config(
        "无法分配本地出口端口，请在组网配置中指定端口",
    ))
}
