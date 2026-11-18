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

    pub fn test_download_target(&self, target: HttpTestTarget) {
        if let HttpTestTarget::Node(id) = &target
            && self
                .model()
                .node(id)
                .is_some_and(|node| !node.is_mesh_gateway())
        {
            self.test_download(id);
            return;
        }
        let Some((targets, core, binary, bypass_tun)) =
            self.prepare_probe_targets(vec![target], "download")
        else {
            return;
        };
        let active = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            for (target, node) in targets {
                let key = target.key();
                service.inner.downloads.write().remove(&key);
                let _mesh_guard = if matches!(target, HttpTestTarget::MeshExit { .. })
                    || node.as_ref().is_some_and(Node::is_mesh_gateway)
                {
                    Some(service.inner.mesh.operation.lock().await)
                } else {
                    None
                };
                let result = match service.http_probe_node(&target) {
                    Ok(current) if current == node => {
                        crate::runtime::probe::download_target(
                            &binary,
                            core,
                            node.as_ref(),
                            bypass_tun,
                        )
                        .await
                    }
                    _ => Err(Error::other("出口配置已更改，请重新进行吞吐测速")),
                };
                if let HttpTestTarget::Node(id) = &target {
                    service.record_proxy_probe(active.as_ref(), id, result.is_ok());
                }
                let label = match (&target, &node) {
                    (HttpTestTarget::Direct, _) => "本机直连".to_owned(),
                    (_, Some(n)) => format!("“{}”", n.name),
                    _ => key.clone(),
                };
                let (entry, err_stage) = match result {
                    Ok(entry) => {
                        service.toast(
                            ToastLevel::Success,
                            format!(
                                "{label}下载速度 {:.2} MiB/s",
                                entry.bytes_per_second as f64 / 1048576.0
                            ),
                        );
                        (entry, crate::runtime::health::ErrStage::None)
                    }
                    Err(error) => {
                        let msg = test_error_message(error);
                        service.toast(ToastLevel::Warning, msg.clone());
                        let stage = crate::runtime::health::classify_error(&msg);
                        (
                            crate::runtime::probe::DownloadResult {
                                bytes: 0,
                                elapsed_ms: 0,
                                bytes_per_second: 0,
                                checked_at: now_unix(),
                                error: Some(msg),
                            },
                            stage,
                        )
                    }
                };
                service.record_health_sample(crate::runtime::health::ProbeSampleRecord {
                    target_key: key.clone(),
                    kind: crate::runtime::health::ProbeKind::Download,
                    ts: entry.checked_at,
                    ok: entry.error.is_none() && entry.bytes_per_second > 0,
                    rtt_ms: None,
                    bps: (entry.bytes_per_second > 0).then_some(entry.bytes_per_second),
                    bytes_used: entry.bytes,
                    err_stage,
                });
                service.inner.downloads.write().insert(key.clone(), entry);
                service.set_busy(&format!("download:{key}"), false);
            }
            service.set_busy("download", false);
        });
    }

    fn all_exit_targets(&self) -> Vec<HttpTestTarget> {
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
        targets
    }

    /// Discovery rows are distinct from saved gateway nodes. Never time a loopback handshake.
    pub fn test_all_http_latency(&self) {
        self.test_http_targets(self.all_exit_targets());
    }

    pub fn test_all_health(&self, force_refresh: bool) {
        self.test_reachability_targets(self.all_exit_targets(), force_refresh);
    }

    pub fn test_reachability_target(&self, target: HttpTestTarget, force_refresh: bool) {
        self.test_reachability_targets(vec![target], force_refresh);
    }

    pub fn test_reachability_nodes(&self, ids: Vec<String>, force_refresh: bool) {
        let targets = ids.into_iter().map(HttpTestTarget::Node).collect();
        self.test_reachability_targets(targets, force_refresh);
    }

    pub fn test_default_group_reachability(&self, force_refresh: bool) {
        let model = self.model();
        let group_ids: Vec<String> = model
            .default_group_id()
            .and_then(|id| model.group(&id))
            .map(|group| {
                group
                    .members
                    .iter()
                    .filter(|id| model.node(id).is_some_and(|n| n.enabled))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let targets: Vec<HttpTestTarget> = if !group_ids.is_empty() {
            group_ids.into_iter().map(HttpTestTarget::Node).collect()
        } else {
            model
                .nodes
                .iter()
                .filter(|n| n.enabled && !n.is_mesh_gateway())
                .map(|n| HttpTestTarget::Node(n.id.clone()))
                .collect()
        };
        self.test_reachability_targets(targets, force_refresh);
    }

    fn prepare_probe_targets(
        &self,
        targets: Vec<HttpTestTarget>,
        busy_key: &str,
    ) -> Option<(
        Vec<(HttpTestTarget, Option<Node>)>,
        crate::model::CoreKind,
        std::path::PathBuf,
        bool,
    )> {
        if self.is_busy("core")
            || self.is_busy("mesh")
            || self.is_busy("latency")
            || self.is_busy("download")
            || self.is_busy("reachability")
            || self.is_busy("auto-probe")
        {
            return None;
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
            return None;
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
                return None;
            }
        };
        if !self.try_begin_probe(busy_key) {
            return None;
        }
        for (target, _) in &targets {
            self.set_busy(&format!("{busy_key}:{}", target.key()), true);
        }
        Some((targets, core, binary, bypass_tun))
    }

    pub fn test_http_targets(&self, targets: Vec<HttpTestTarget>) {
        let Some((targets, core, binary, bypass_tun)) =
            self.prepare_probe_targets(targets, "latency")
        else {
            return;
        };
        let settings = self.settings();
        let overseas_url = settings.urltest_url;
        let domestic_url = settings.domestic_urltest_url;
        let active = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            use futures_util::{StreamExt, stream};
            let mut pending = stream::iter(targets)
                .map(|(target, node)| {
                    let service = service.clone();
                    let binary = binary.clone();
                    let url = if matches!(target, HttpTestTarget::Direct) {
                        domestic_url.clone()
                    } else {
                        overseas_url.clone()
                    };
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
                .buffer_unordered(4);
            let mut successful = 0;
            while let Some((target, result)) = pending.next().await {
                let ok = result.is_ok();
                if ok {
                    successful += 1;
                }
                if let HttpTestTarget::Node(id) = &target {
                    service.record_proxy_probe(active.as_ref(), id, ok);
                }
                let key = target.key();
                let delay_ms = result.as_ref().ok().copied();
                let checked_at = now_unix();
                let detail = match result {
                    Ok(_) => "HTTP 延迟".into(),
                    Err(error) => format!("HTTP 测速失败：{}", test_error_message(error)),
                };
                let err_stage = if ok {
                    crate::runtime::health::ErrStage::None
                } else {
                    crate::runtime::health::classify_error(&detail)
                };
                service.record_health_sample(crate::runtime::health::ProbeSampleRecord {
                    target_key: key.clone(),
                    kind: crate::runtime::health::ProbeKind::Http,
                    ts: checked_at,
                    ok,
                    rtt_ms: delay_ms,
                    bps: None,
                    bytes_used: if ok { 512 } else { 0 },
                    err_stage,
                });
                let entry = LatencyEntry {
                    key: key.clone(),
                    delay_ms,
                    checked_at,
                    detail: Some(detail),
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

    pub fn test_reachability_targets(&self, targets: Vec<HttpTestTarget>, force_refresh: bool) {
        let Some((targets, core, binary, bypass_tun)) =
            self.prepare_probe_targets(targets, "reachability")
        else {
            return;
        };
        let count = targets.len();
        let settings = self.settings();
        let overseas_url = settings.urltest_url;
        let domestic_url = settings.domestic_urltest_url;
        let active = self.tray_probe_target();
        let service = self.clone();
        self.spawn(async move {
            use futures_util::{StreamExt, stream};
            let mut pending = stream::iter(targets)
                .map(|(target, node)| {
                    let service = service.clone();
                    let binary = binary.clone();
                    let url = if matches!(target, HttpTestTarget::Direct) {
                        domestic_url.clone()
                    } else {
                        overseas_url.clone()
                    };
                    async move {
                        let _mesh_guard = if matches!(target, HttpTestTarget::MeshExit { .. })
                            || node.as_ref().is_some_and(Node::is_mesh_gateway)
                        {
                            Some(service.inner.mesh.operation.lock().await)
                        } else {
                            None
                        };
                        let result = match service.http_probe_node(&target) {
                            Ok(current) if current == node => {
                                let udp_enabled = node.as_ref().is_none_or(|n| n.udp);
                                let store = service.inner.store.clone();
                                crate::runtime::reachability::probe_reachability(
                                    &binary,
                                    core,
                                    node.as_ref(),
                                    bypass_tun,
                                    udp_enabled,
                                    force_refresh,
                                    Some(&url),
                                    move |ip| {
                                        store
                                            .get_cached_reachability(ip, 6 * 3600, now_unix())
                                            .ok()
                                            .flatten()
                                    },
                                )
                                .await
                            }
                            _ => Err(Error::other("出口配置已更改，请重新进行可达性检测")),
                        };
                        (target, result)
                    }
                })
                .buffer_unordered(3);
            let mut successful = 0usize;
            while let Some((target, result)) = pending.next().await {
                if let HttpTestTarget::Node(id) = &target {
                    service.record_proxy_probe(active.as_ref(), id, result.is_ok());
                }
                let key = target.key();
                match result {
                    Ok(outcome) => {
                        successful += 1;
                        let from_cache = outcome.from_cache;
                        service.record_reachability_outcome(&key, &outcome);
                        if count == 1 {
                            service.toast(
                                ToastLevel::Success,
                                if from_cache {
                                    "可达性检测完成（复用同出口缓存）"
                                } else {
                                    "可达性检测完成"
                                },
                            );
                        }
                    }
                    Err(error) => {
                        let msg = test_error_message(error);
                        service.record_health_sample(crate::runtime::health::ProbeSampleRecord {
                            target_key: key.clone(),
                            kind: crate::runtime::health::ProbeKind::Reachability,
                            ts: now_unix(),
                            ok: false,
                            rtt_ms: None,
                            bps: None,
                            bytes_used: 0,
                            err_stage: crate::runtime::health::classify_error(&msg),
                        });
                        if count == 1 {
                            service.toast(ToastLevel::Warning, format!("可达性检测失败：{msg}"));
                        }
                    }
                }
                service.set_busy(&format!("reachability:{key}"), false);
            }
            service.set_busy("reachability", false);
            if count > 1 {
                service.toast(
                    ToastLevel::Info,
                    format!("批量健康检查完成：{successful}/{count} 个出口已评估"),
                );
            }
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
