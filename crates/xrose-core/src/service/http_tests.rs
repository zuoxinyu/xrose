//! HTTP measurements for direct, proxy and connected mesh exits.

use super::{AppService, ToastLevel};
use crate::{
    Error, Result,
    mesh::MeshPhase,
    model::{DIRECT_VIRTUAL_NODE_ID, Node},
    storage::LatencyEntry,
    util::now_unix,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpTestTarget {
    Direct,
    Node(String),
    MeshExit { profile: String, peer: String },
}

impl HttpTestTarget {
    pub fn key(&self) -> String {
        match self {
            Self::Direct => DIRECT_VIRTUAL_NODE_ID.into(),
            Self::Node(id) => id.clone(),
            Self::MeshExit { profile, peer } => format!("mesh-http:{profile}:{peer}"),
        }
    }
}

impl AppService {
    pub fn test_http_target(&self, target: HttpTestTarget) {
        self.test_http_targets(vec![target]);
    }

    /// Discovery rows are distinct from saved gateway nodes. Never time a loopback handshake.
    pub fn test_all_http_latency(&self) {
        let model = self.model();
        let mesh = self.inner.mesh.snapshot();
        let mut targets = vec![HttpTestTarget::Direct];
        targets.extend(
            model
                .nodes
                .iter()
                .filter(|node| node.enabled && !node.is_mesh_gateway())
                .map(|node| HttpTestTarget::Node(node.id.clone())),
        );
        targets.extend(
            mesh.exits()
                .filter(|(_, peer, _)| peer.online)
                .map(|(profile, peer, _)| HttpTestTarget::MeshExit {
                    profile: profile.id.clone(),
                    peer: peer.id.clone(),
                })
                .filter(|target| self.http_probe_node(target).is_ok()),
        );
        self.test_http_targets(targets);
    }

    pub fn test_http_targets(&self, targets: Vec<HttpTestTarget>) {
        if self.is_busy("core")
            || self.is_busy("mesh")
            || self.is_busy("latency")
            || self.is_busy("download")
            || self.is_busy("auto-probe")
        {
            return;
        }
        let mut seen = std::collections::BTreeSet::new();
        let targets: Vec<_> = targets
            .into_iter()
            .filter(|target| seen.insert(target.key()))
            .filter_map(|target| match self.http_probe_node(&target) {
                Ok(node) => Some((target, node)),
                Err(error) => {
                    self.toast(ToastLevel::Warning, test_error_message(error));
                    None
                }
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let settings = self.settings();
        let bypass_tun = self.inner.core.native_tun_active();
        let core = self
            .inner
            .core
            .state()
            .core
            .filter(|_| bypass_tun)
            .unwrap_or(settings.core);
        let binary = match self.inner.core.resolve_binary(core, &settings) {
            Ok(binary) => binary,
            Err(error) => {
                self.toast(ToastLevel::Error, test_error_message(error));
                return;
            }
        };
        if !self.try_begin_probe("latency") {
            return;
        }
        for (target, _) in &targets {
            self.set_busy(&format!("latency:{}", target.key()), true);
        }
        let active = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            use futures_util::{StreamExt, stream};
            let results: Vec<_> = stream::iter(targets)
                .map(|(target, node)| {
                    let service = service.clone();
                    let binary = binary.clone();
                    let url = settings.urltest_url.clone();
                    async move {
                        // Keep the mesh operation lock while measuring its gateway so a concurrent
                        // exit switch cannot silently send this request through another device.
                        let _mesh_guard = if matches!(target, HttpTestTarget::MeshExit { .. })
                            || node.as_ref().is_some_and(Node::is_mesh_gateway)
                        {
                            Some(service.inner.mesh.operation.lock().await)
                        } else {
                            None
                        };
                        let result = match service.http_probe_node(&target) {
                            Ok(current) if current == node => {
                                crate::runtime::probe::http_target_latency(
                                    &binary,
                                    core,
                                    node.as_ref(),
                                    &url,
                                    bypass_tun,
                                )
                                .await
                            }
                            _ => Err(Error::other("出口配置已更改，请重新进行 HTTP 测速")),
                        };
                        (target, result)
                    }
                })
                .buffer_unordered(4)
                .collect()
                .await;
            let mut successful = 0;
            for (target, result) in results {
                if result.is_ok() {
                    successful += 1;
                }
                if let HttpTestTarget::Node(id) = &target {
                    service.record_proxy_probe(active.as_ref(), id, result.is_ok());
                }
                let key = target.key();
                let entry = LatencyEntry {
                    key: key.clone(),
                    delay_ms: result.as_ref().ok().copied(),
                    checked_at: now_unix(),
                    detail: Some(match result {
                        Ok(_) => "HTTP 延迟".into(),
                        Err(error) => format!("HTTP 测速失败：{}", test_error_message(error)),
                    }),
                };
                let _ = service.inner.store.set_latency(&entry);
                service.inner.latency.write().insert(key.clone(), entry);
                service.set_busy(&format!("latency:{key}"), false);
            }
            service.set_busy("latency", false);
            service.toast(
                ToastLevel::Info,
                format!("HTTP 测速完成：{successful} 个出口可用"),
            );
        });
    }

    fn http_probe_node(&self, target: &HttpTestTarget) -> Result<Option<Node>> {
        match target {
            HttpTestTarget::Direct => Ok(None),
            HttpTestTarget::Node(id) => {
                let node = self
                    .model()
                    .node(id)
                    .filter(|node| node.enabled)
                    .cloned()
                    .ok_or_else(|| Error::config("节点不存在或已禁用，请先启用节点"))?;
                if node.is_mesh_gateway() {
                    let profile = id.strip_prefix("mesh-exit-").unwrap();
                    let snapshot = self.inner.mesh.snapshot();
                    let peer = snapshot
                        .exits()
                        .find(|(p, peer, address)| {
                            p.id == profile
                                && peer.online
                                && self
                                    .inner
                                    .mesh
                                    .profile(profile)
                                    .is_ok_and(|p| p.exit_node == address.to_string())
                        })
                        .map(|(_, peer, _)| peer.id.clone())
                        .ok_or_else(|| Error::config("请先选择并连接组网出口，再进行 HTTP 测速"))?;
                    return self.http_probe_node(&HttpTestTarget::MeshExit {
                        profile: profile.into(),
                        peer,
                    });
                }
                Ok(Some(node))
            }
            HttpTestTarget::MeshExit { profile, peer } => {
                let snapshot = self.inner.mesh.snapshot();
                let saved = self.inner.mesh.profile(profile)?;
                connected_mesh_node(
                    &saved,
                    &snapshot,
                    &self.model(),
                    self.inner
                        .core
                        .mesh_exit_runtime()
                        .map(|runtime| runtime.node),
                    profile,
                    peer,
                )
                .map(Some)
            }
        }
    }
}

fn test_error_message(error: Error) -> String {
    match error {
        Error::Config(message)
        | Error::Binary(message)
        | Error::Process(message)
        | Error::Other(message) => message,
        _ => "HTTP 测速失败，请检查出口连接、内核与测试地址".into(),
    }
}

fn connected_mesh_node(
    saved: &crate::mesh::MeshProfile,
    snapshot: &crate::mesh::MeshSnapshot,
    model: &crate::model::AppModel,
    runtime: Option<Node>,
    profile: &str,
    peer: &str,
) -> Result<Node> {
    let (_, _, address) = snapshot
        .exits()
        .find(|(p, device, _)| p.id == profile && device.id == peer && device.online)
        .ok_or_else(|| Error::config("出口设备未连接，请先连接所属组网"))?;
    if !snapshot
        .networks
        .iter()
        .any(|network| network.profile_id == profile && network.phase == MeshPhase::Connected)
    {
        return Err(Error::config("组网尚未连接，请连接后再进行 HTTP 测速"));
    }
    if model.work_mode == crate::model::WorkMode::Mesh {
        if let Some(target) = model
            .mesh_mode
            .target
            .as_ref()
            .filter(|target| target.profile_id == profile)
        {
            if target.peer_id != peer || target.address != address.to_string() {
                return Err(Error::config(
                    "此组网当前使用其他出口，请先选择并连接此出口，再进行 HTTP 测速",
                ));
            }
            return runtime.ok_or_else(|| {
                Error::config("出口通道尚未就绪，请等待连接完成后再进行 HTTP 测速")
            });
        }
    }
    if saved.outbound_port == 0 || saved.exit_node != address.to_string() {
        return Err(Error::config("请先选择并连接此组网出口，再进行 HTTP 测速"));
    }
    Ok(crate::mesh::controller::outbound_node(saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::Protocol, paths::AppPaths};

    #[test]
    fn mesh_http_measurement_requires_the_exact_live_exit() {
        use crate::mesh::{MeshPeer, MeshProfile, MeshRuntimeView, MeshSecret, MeshSnapshot};
        let profile = MeshProfile {
            exit_node: "192.0.2.10".into(),
            outbound_port: 19081,
            outbound_secret: MeshSecret("fixture-only".into()),
            ..Default::default()
        };
        let mut snapshot = MeshSnapshot {
            profiles: vec![profile.summary()],
            networks: vec![MeshRuntimeView {
                profile_id: profile.id.clone(),
                phase: MeshPhase::Connected,
                peers: vec![
                    MeshPeer {
                        id: "a".into(),
                        online: true,
                        offers_exit: true,
                        addresses: vec!["192.0.2.10".into()],
                        ..Default::default()
                    },
                    MeshPeer {
                        id: "b".into(),
                        online: true,
                        offers_exit: true,
                        addresses: vec!["192.0.2.11".into()],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let model = crate::model::AppModel::default();
        assert_eq!(
            connected_mesh_node(&profile, &snapshot, &model, None, &profile.id, "a").unwrap(),
            crate::mesh::controller::outbound_node(&profile)
        );
        assert!(connected_mesh_node(&profile, &snapshot, &model, None, &profile.id, "b").is_err());
        let mut mesh_model = model.clone();
        mesh_model.work_mode = crate::model::WorkMode::Mesh;
        mesh_model.mesh_mode.target = Some(crate::model::MeshExitTarget {
            profile_id: profile.id.clone(),
            peer_id: "b".into(),
            provider: profile.provider,
            name: "Fixture current exit".into(),
            address: "192.0.2.11".into(),
            gateway_id: format!("mesh-exit-{}", profile.id),
        });
        let mut current = crate::mesh::controller::outbound_node(&profile);
        current.port = 19082;
        assert!(
            connected_mesh_node(
                &profile,
                &snapshot,
                &mesh_model,
                Some(current.clone()),
                &profile.id,
                "a"
            )
            .is_err()
        );
        assert_eq!(
            connected_mesh_node(
                &profile,
                &snapshot,
                &mesh_model,
                Some(current.clone()),
                &profile.id,
                "b"
            )
            .unwrap(),
            current
        );
        assert!(
            connected_mesh_node(&profile, &snapshot, &mesh_model, None, &profile.id, "b").is_err()
        );
        snapshot.networks[0].phase = MeshPhase::Disconnected;
        assert!(connected_mesh_node(&profile, &snapshot, &model, None, &profile.id, "a").is_err());
        snapshot.networks[0].phase = MeshPhase::Connected;
        snapshot.networks[0].peers[0].online = false;
        assert!(connected_mesh_node(&profile, &snapshot, &model, None, &profile.id, "a").is_err());
    }

    #[test]
    fn http_targets_have_distinct_keys_and_never_change_selection() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let mut node = Node::new("Fixture proxy", Protocol::Socks, "192.0.2.10", 1080);
        service.inner.model.write().add_nodes([node.clone()]);
        let before = service.model();
        assert!(
            service
                .http_probe_node(&HttpTestTarget::Direct)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            service
                .http_probe_node(&HttpTestTarget::Node(node.id.clone()))
                .unwrap(),
            Some(node.clone())
        );
        let first = HttpTestTarget::MeshExit {
            profile: "fixture".into(),
            peer: "a".into(),
        };
        let second = HttpTestTarget::MeshExit {
            profile: "fixture".into(),
            peer: "b".into(),
        };
        assert_ne!(first.key(), second.key());
        assert!(service.http_probe_node(&first).is_err());
        assert_eq!(service.model(), before);
        node.enabled = false;
        service.inner.model.write().nodes[0] = node.clone();
        assert!(
            service
                .http_probe_node(&HttpTestTarget::Node(node.id))
                .is_err()
        );
    }
}
