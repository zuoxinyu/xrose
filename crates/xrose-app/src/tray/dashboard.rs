//! Content and service actions shared by every platform's GPUI tray flyout.

use xrose_core::AppService;
use xrose_core::model::{DIRECT_VIRTUAL_NODE_ID, RoutePreset, WorkMode};
use xrose_core::service::{APP_VERSION, HttpTestTarget, TrayIndicator, TrayStatus};
use xrose_core::settings::Language;
use xrose_core::util::{format_bytes, format_speed};

use crate::i18n::tr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardNode {
    pub id: String,
    pub label: String,
    pub delay_ms: Option<u32>,
    pub active: bool,
    pub action: Option<TrayAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardMeshStatus {
    pub provider: xrose_core::mesh::MeshProvider,
    pub phase: xrose_core::mesh::MeshPhase,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub ip: Option<String>,
    pub retrying: bool,
}

impl DashboardMeshStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            xrose_core::mesh::MeshPhase::Starting
                | xrose_core::mesh::MeshPhase::NeedsLogin
                | xrose_core::mesh::MeshPhase::NeedsApproval
                | xrose_core::mesh::MeshPhase::Connected
                | xrose_core::mesh::MeshPhase::Stopping
        )
    }

    pub fn is_stoppable(&self) -> bool {
        matches!(
            self.phase,
            xrose_core::mesh::MeshPhase::Starting
                | xrose_core::mesh::MeshPhase::NeedsLogin
                | xrose_core::mesh::MeshPhase::NeedsApproval
                | xrose_core::mesh::MeshPhase::Connected
        ) || self.retrying
    }
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub language: Language,
    pub direct: bool,
    pub mesh: bool,
    pub mesh_supported: bool,
    pub automatic_group: bool,
    pub group_name: String,
    pub nodes: Vec<DashboardNode>,
    pub running: bool,
    pub easytier: DashboardMeshStatus,
    pub tailscale: DashboardMeshStatus,
    pub proxy_address: String,
    pub system_proxy: bool,
    pub system_proxy_status: Option<xrose_core::system::proxy::SystemProxyStatus>,
    pub tun_enabled: bool,
    pub tun_supported: bool,
    pub route_preset: RoutePreset,
    pub down_speed: u64,
    pub up_speed: u64,
    pub total: String,
    pub core_name: String,
    pub busy_core: bool,
    pub busy_mesh: bool,
    pub busy_power: bool,
    pub busy_proxy: bool,
    pub busy_latency: bool,
}

impl Dashboard {
    pub fn any_service_active(&self) -> bool {
        self.running
            || self.easytier.is_active()
            || self.easytier.retrying
            || self.tailscale.is_active()
            || self.tailscale.retrying
    }
}

pub(super) fn mesh_phase_label(
    phase: xrose_core::mesh::MeshPhase,
    lang: Language,
) -> gpui_kit::SharedString {
    use xrose_core::mesh::MeshPhase::*;
    match phase {
        Connected => tr(lang, "已连接", "Connected"),
        Starting => tr(lang, "连接中", "Connecting"),
        NeedsLogin => tr(lang, "待登录", "Sign-in required"),
        NeedsApproval => tr(lang, "待批准", "Approval required"),
        Stopping => tr(lang, "断开中", "Disconnecting"),
        Failed => tr(lang, "异常", "Failed"),
        Disconnected => tr(lang, "未连接", "Disconnected"),
    }
}

pub(super) fn status_lines(status: &TrayStatus) -> (String, Option<String>) {
    let language = status.language;
    use xrose_core::core::ProcessPhase;
    let (zh, en) = match status.indicator {
        TrayIndicator::Unavailable => ("代理不可用", "Proxy unavailable"),
        TrayIndicator::MeshOnly => ("仅组网运行", "Mesh only"),
        TrayIndicator::MeshProxy => ("代理与组网运行中", "Proxy and mesh active"),
        TrayIndicator::Direct => ("直连监控", "Direct monitoring"),
        TrayIndicator::Proxy => ("代理运行中", "Proxy running"),
        TrayIndicator::Mesh => ("组网出口接管中", "Mesh exit active"),
        TrayIndicator::Idle => match status.phase {
            ProcessPhase::Starting => ("启动中", "Starting"),
            ProcessPhase::Stopping => ("停止中", "Stopping"),
            _ => ("代理未运行", "Proxy stopped"),
        },
    };
    let mut status_line = format!(
        "Xrose v{} · {} ({})",
        APP_VERSION,
        tr(language, zh, en),
        status.core.display_name()
    );
    if status.easytier != xrose_core::mesh::MeshPhase::Disconnected {
        status_line.push_str(&format!(
            " · EasyTier: {}",
            mesh_phase_label(status.easytier, language)
        ));
    }
    if status.tailscale != xrose_core::mesh::MeshPhase::Disconnected {
        status_line.push_str(&format!(
            " · Tailscale: {}",
            mesh_phase_label(status.tailscale, language)
        ));
    }
    let traffic_line = status.phase.is_alive().then(|| {
        format!(
            "↓ {}  ↑ {}  ·  {} {}",
            format_speed(status.down_speed),
            format_speed(status.up_speed),
            tr(language, "累计", "total"),
            format_bytes(status.total_bytes)
        )
    });
    (status_line, traffic_line)
}

#[cfg(any(target_os = "macos", windows))]
pub(super) fn tooltip(status: &TrayStatus) -> String {
    let (line, traffic) = status_lines(status);
    match traffic {
        Some(traffic) => format!("{line}\n{traffic}"),
        None => line,
    }
}

/// Builds the flyout from one UI snapshot.
pub fn build(service: &AppService) -> Dashboard {
    let snapshot = service.snapshot();
    let language = snapshot.settings.language;
    let hide_ips = snapshot.settings.privacy_mode;
    let suffixes = snapshot.mesh.magic_dns_suffixes();
    let running = snapshot.process.phase.is_alive();
    let model = &snapshot.model;
    let group = model
        .default_group_id()
        .and_then(|id| model.group(&id).cloned())
        .or_else(|| model.groups.first().cloned());
    let active_node = snapshot
        .selected_node(group.as_ref().map(|group| group.id.as_str()))
        .map(|node| node.id.clone());
    let direct = model.work_mode == WorkMode::Direct;
    let represented_gateways: std::collections::HashSet<_> = snapshot
        .mesh
        .exits()
        .map(|(profile, _, _)| format!("mesh-exit-{}", profile.id))
        .chain(
            model
                .mesh_mode
                .target
                .iter()
                .map(|target| target.gateway_id.clone()),
        )
        .collect();
    let mut nodes = vec![DashboardNode {
        id: DIRECT_VIRTUAL_NODE_ID.into(),
        label: tr(language, "本机直连", "Direct").into(),
        delay_ms: None,
        active: direct,
        action: Some(TrayAction::SelectMode(WorkMode::Direct)),
    }];
    if let Some(group) = group.as_ref().filter(|group| {
        group
            .selected
            .as_ref()
            .and_then(|id| model.node(id))
            .is_some_and(|node| node.is_mesh_gateway())
    }) {
        nodes.push(DashboardNode {
            id: "__xrose_proxy_mode__".into(),
            label: format!(
                "{} · {}",
                tr(language, "代理", "Proxy"),
                crate::i18n::outbound_group_label(language, group)
            ),
            delay_ms: None,
            active: model.work_mode == WorkMode::Proxy,
            action: model
                .has_usable_default_group(snapshot.settings.core)
                .then_some(TrayAction::SelectMode(WorkMode::Proxy)),
        });
    }
    let members: std::collections::HashSet<_> = group.iter().flat_map(|g| &g.members).collect();
    nodes.extend(
        model
            .nodes
            .iter()
            .filter(|node| {
                model.node_usable_with(node, snapshot.settings.core)
                    && (!node.is_mesh_gateway() || !represented_gateways.contains(&node.id))
                    && (node.is_mesh_gateway() || members.contains(&node.id))
            })
            .map(|node| {
                let delay = snapshot
                    .latency
                    .get(&node.id)
                    .filter(|entry| entry.is_http())
                    .and_then(|entry| entry.delay_ms);
                DashboardNode {
                    id: node.id.clone(),
                    label: if node.is_mesh_gateway() {
                        format!("{} · {}", tr(language, "组网", "Mesh"), node.name)
                    } else if hide_ips {
                        xrose_core::privacy::mask_server_in_label(
                            &node.name,
                            &node.server,
                            "•••",
                            &suffixes,
                        )
                    } else {
                        node.name.clone()
                    },
                    delay_ms: delay,
                    active: active_node.as_deref() == Some(node.id.as_str())
                        && (!node.is_mesh_gateway() || model.work_mode == WorkMode::Mesh),
                    action: if node.is_mesh_gateway() {
                        Some(TrayAction::SelectNode {
                            group_id: String::new(),
                            node_id: node.id.clone(),
                        })
                    } else {
                        group
                            .as_ref()
                            .filter(|g| g.enabled && g.manually_selectable())
                            .map(|g| TrayAction::SelectNode {
                                group_id: g.id.clone(),
                                node_id: node.id.clone(),
                            })
                    },
                }
            }),
    );
    let mut target_listed = false;
    for (profile, peer, _) in snapshot.mesh.exits() {
        let current = model
            .mesh_mode
            .target
            .as_ref()
            .is_some_and(|target| target.profile_id == profile.id && target.peer_id == peer.id);
        target_listed |= current;
        let name = if peer.name.is_empty() {
            profile.name.clone()
        } else if hide_ips {
            xrose_core::privacy::mask_magic_dns_name(&peer.name, &suffixes)
        } else {
            peer.name.clone()
        };
        nodes.push(DashboardNode {
            id: format!("mesh:{}:{}", profile.id, peer.id),
            label: format!("{} · {name}", tr(language, "组网", "Mesh")),
            delay_ms: snapshot
                .latency
                .get(
                    &HttpTestTarget::MeshExit {
                        profile: profile.id.clone(),
                        peer: peer.id.clone(),
                    }
                    .key(),
                )
                .filter(|entry| entry.is_http())
                .and_then(|entry| entry.delay_ms),
            active: model.work_mode == WorkMode::Mesh
                && model.mesh_mode.target.as_ref().is_some_and(|target| {
                    target.profile_id == profile.id && target.peer_id == peer.id
                }),
            action: (peer.online || current).then(|| TrayAction::SelectMeshExit {
                profile_id: profile.id.clone(),
                peer_id: peer.id.clone(),
            }),
        });
    }
    if !target_listed && let Some(target) = &model.mesh_mode.target {
        let name = if hide_ips {
            xrose_core::privacy::mask_server_in_label(
                &target.name,
                &target.address,
                "•••",
                &suffixes,
            )
        } else {
            target.name.clone()
        };
        nodes.push(DashboardNode {
            id: format!("mesh:{}:{}", target.profile_id, target.peer_id),
            label: format!("{} · {name}", tr(language, "组网", "Mesh")),
            delay_ms: None,
            active: model.work_mode == WorkMode::Mesh,
            action: Some(TrayAction::SelectMode(WorkMode::Mesh)),
        });
    }
    let easytier = mesh_status(&snapshot, xrose_core::mesh::MeshProvider::EasyTier);
    let tailscale = mesh_status(&snapshot, xrose_core::mesh::MeshProvider::Tailscale);
    let busy_core = snapshot
        .busy
        .iter()
        .any(|key| key == "core" || key == "mesh-exit");
    let busy_mesh = snapshot
        .busy
        .iter()
        .any(|key| key == "mesh" || key == "mesh-exit");
    let busy_power = busy_core || busy_mesh || snapshot.busy.iter().any(|key| key == "power");

    Dashboard {
        language,
        direct,
        mesh: model.work_mode == WorkMode::Mesh,
        mesh_supported: snapshot.mesh.supported,
        automatic_group: group.as_ref().is_some_and(|g| !g.manually_selectable())
            && model.has_usable_default_group(snapshot.settings.core),
        group_name: group
            .map(|group| crate::i18n::outbound_group_label(language, &group).to_string())
            .unwrap_or_else(|| tr(language, "代理", "Proxy").to_string()),
        nodes,
        running,
        easytier,
        tailscale,
        proxy_address: if hide_ips {
            format!("127.*.*.*:{}", snapshot.settings.mixed_port)
        } else {
            format!("127.0.0.1:{}", snapshot.settings.mixed_port)
        },
        down_speed: snapshot.runtime.down_speed,
        up_speed: snapshot.runtime.up_speed,
        system_proxy: snapshot.system_proxy_active,
        system_proxy_status: snapshot.system_proxy_status,
        tun_enabled: model.work_mode == WorkMode::Mesh || snapshot.settings.effective_tun(),
        tun_supported: snapshot.settings.core.capabilities().native_tun,
        route_preset: snapshot.model.routing.preset,
        total: format_bytes(
            snapshot
                .runtime
                .down_total
                .saturating_add(snapshot.runtime.up_total),
        ),
        core_name: snapshot.settings.core.display_name().to_string(),
        busy_proxy: snapshot.busy.iter().any(|key| key == "proxy"),
        busy_core,
        busy_mesh,
        busy_power,
        busy_latency: snapshot
            .busy
            .iter()
            .any(|key| matches!(key.as_str(), "latency" | "download" | "auto-probe")),
    }
}

fn mesh_status(
    snapshot: &xrose_core::service::UiSnapshot,
    provider: xrose_core::mesh::MeshProvider,
) -> DashboardMeshStatus {
    use xrose_core::mesh::MeshPhase;
    let net = xrose_core::mesh::MeshRuntimeView::for_provider(&snapshot.mesh.networks, provider);
    let retrying_id = snapshot
        .mesh
        .profiles
        .iter()
        .find(|p| p.provider == provider && snapshot.mesh_reconnecting.contains(&p.id))
        .map(|p| p.id.clone());
    let profile_id = net
        .map(|n| n.profile_id.clone())
        .or(retrying_id)
        .or_else(|| {
            xrose_core::service::preferred_mesh_profile_id(
                &snapshot.mesh.profiles,
                &snapshot.settings,
                provider,
            )
        });
    let profile = profile_id
        .as_deref()
        .and_then(|id| snapshot.mesh.profiles.iter().find(|p| p.id == id));
    let raw_phase = net.map(|n| n.phase).unwrap_or(MeshPhase::Disconnected);
    let active = matches!(
        raw_phase,
        MeshPhase::Starting
            | MeshPhase::NeedsLogin
            | MeshPhase::NeedsApproval
            | MeshPhase::Connected
            | MeshPhase::Stopping
    );
    let retrying = profile_id
        .as_deref()
        .is_some_and(|id| snapshot.mesh_reconnecting.contains(id));
    let has_error = profile_id
        .as_deref()
        .is_some_and(|id| snapshot.mesh_connection_errors.contains_key(id))
        || net.and_then(|n| n.error.as_deref()).is_some();
    let phase = if !active && !retrying && has_error {
        MeshPhase::Failed
    } else {
        raw_phase
    };
    let ip = net.and_then(|n| {
        n.addresses.first().map(|addr| {
            let raw = addr.split('/').next().unwrap_or(addr);
            if snapshot.settings.privacy_mode {
                xrose_core::privacy::mask_ip_or_cidr(raw)
            } else {
                raw.to_string()
            }
        })
    });
    DashboardMeshStatus {
        provider,
        phase,
        profile_id,
        profile_name: profile.map(|p| p.name.clone()),
        ip,
        retrying,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    ToggleCore,
    ToggleAll,
    ConnectMesh { profile_id: String },
    DisconnectMesh(xrose_core::mesh::MeshProvider),
    CancelMeshReconnect { profile_id: String },
    TestAll,
    SelectMode(WorkMode),
    SelectNode { group_id: String, node_id: String },
    SelectMeshExit { profile_id: String, peer_id: String },
    SetSystemProxy(bool),
    ToggleTun,
    SetRoutePreset(RoutePreset),
    ShowWindow,
    Quit,
}

/// A command that needs the GPUI main loop (it owns the window and the quit
/// path). Everything else is applied directly from the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    #[cfg(target_os = "linux")]
    CloseMainWindow,
    #[cfg(target_os = "linux")]
    TrayUnavailable,
    TogglePopup,
    ShowWindow,
    #[cfg(target_os = "macos")]
    ShowAbout,
    #[cfg(target_os = "macos")]
    ShowUpdates,
    Quit,
}

#[cfg(target_os = "linux")]
impl TrayCommand {
    pub(super) fn shows_main_window(self) -> bool {
        matches!(self, Self::ShowWindow)
    }
}

pub fn dispatch(action: TrayAction, service: &AppService) -> Option<TrayCommand> {
    match action {
        TrayAction::ToggleCore => service.toggle_core(),
        TrayAction::ToggleAll => service.toggle_proxy_and_mesh(),
        TrayAction::ConnectMesh { profile_id } => service.mesh_connect(&profile_id),
        TrayAction::DisconnectMesh(provider) => service.mesh_disconnect(provider),
        TrayAction::CancelMeshReconnect { profile_id } => {
            service.cancel_mesh_reconnect(&profile_id)
        }
        TrayAction::SelectMode(mode) => {
            let result = match mode {
                WorkMode::Direct => service.select_direct_mode(),
                WorkMode::Proxy => service.select_proxy_mode(),
                WorkMode::Mesh => service.select_mesh_mode(),
            };
            if let Err(error) = result {
                service.notify(xrose_core::service::ToastLevel::Error, error.to_string());
            }
        }
        TrayAction::TestAll => service.test_all_http_latency(),
        TrayAction::SelectNode { group_id, node_id } => {
            service.select_node(&group_id, &node_id);
        }
        TrayAction::SelectMeshExit {
            profile_id,
            peer_id,
        } => service.select_mesh_exit_mode(profile_id, peer_id),
        TrayAction::SetSystemProxy(enabled) => service.set_system_proxy(enabled),
        TrayAction::ToggleTun => service.set_tun_enabled(!service.settings().effective_tun()),
        TrayAction::SetRoutePreset(preset) => service.switch_route_preset(preset),
        TrayAction::ShowWindow => return Some(TrayCommand::ShowWindow),
        TrayAction::Quit => return Some(TrayCommand::Quit),
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Dashboard, DashboardMeshStatus};
    use xrose_core::mesh::{MeshPhase, MeshProvider};
    use xrose_core::model::RoutePreset;
    use xrose_core::settings::Language;

    fn mesh_stub(provider: MeshProvider) -> DashboardMeshStatus {
        DashboardMeshStatus {
            provider,
            phase: MeshPhase::Disconnected,
            profile_id: None,
            profile_name: None,
            ip: None,
            retrying: false,
        }
    }

    fn dashboard_stub() -> Dashboard {
        Dashboard {
            language: Language::Zh,
            direct: false,
            mesh: false,
            mesh_supported: true,
            automatic_group: false,
            group_name: "代理".into(),
            nodes: Vec::new(),
            running: false,
            easytier: mesh_stub(MeshProvider::EasyTier),
            tailscale: mesh_stub(MeshProvider::Tailscale),
            proxy_address: "127.0.0.1:7890".into(),
            system_proxy: false,
            system_proxy_status: None,
            tun_enabled: false,
            tun_supported: true,
            route_preset: RoutePreset::BypassMainland,
            down_speed: 0,
            up_speed: 0,
            total: "0 B".into(),
            core_name: "sing-box".into(),
            busy_core: false,
            busy_mesh: false,
            busy_power: false,
            busy_proxy: false,
            busy_latency: false,
        }
    }

    #[test]
    fn any_service_active_reflects_core_mesh_and_reconnect_state() {
        let mut d = dashboard_stub();
        assert!(!d.any_service_active());

        d.running = true;
        assert!(d.any_service_active());

        d.running = false;
        d.easytier.phase = MeshPhase::Connected;
        assert!(d.any_service_active());
        assert!(d.easytier.is_stoppable());

        d.easytier.phase = MeshPhase::Stopping;
        assert!(d.any_service_active());
        assert!(!d.easytier.is_stoppable());

        d.easytier.phase = MeshPhase::Disconnected;
        d.tailscale.retrying = true;
        assert!(d.any_service_active());
        assert!(d.tailscale.is_stoppable());
    }
}
