//! Content and service actions shared by every platform's GPUI tray flyout.

use xrose_core::AppService;
use xrose_core::model::RoutePreset;
use xrose_core::service::LatencyScope;
use xrose_core::settings::Language;
use xrose_core::util::{format_bytes, format_speed};

use crate::i18n::tr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardNode {
    pub id: String,
    pub label: String,
    pub delay_ms: Option<u32>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardMeshStatus {
    pub provider: xrose_core::mesh::MeshProvider,
    pub phase: xrose_core::mesh::MeshPhase,
    pub profile_name: Option<String>,
    pub ip: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub language: Language,
    pub status_line: String,
    #[allow(dead_code)]
    pub traffic_line: Option<String>,
    pub group_id: Option<String>,
    pub group_name: String,
    pub nodes: Vec<DashboardNode>,
    pub running: bool,
    pub show_speed: bool,
    pub easytier: DashboardMeshStatus,
    pub tailscale: DashboardMeshStatus,
    pub proxy_address: String,
    pub system_proxy: bool,
    pub tun_enabled: bool,
    pub tun_supported: bool,
    pub route_preset: RoutePreset,
    pub down_speed: u64,
    pub up_speed: u64,
    pub total: String,
    pub core_name: String,
    pub busy_core: bool,
    pub busy_latency: bool,
}

/// Builds the dashboard from one snapshot. Called by the tray tick at 5 Hz;
/// keep it allocation-light.
pub fn build(service: &AppService) -> Dashboard {
    let snapshot = service.snapshot();
    let language = snapshot.settings.language;
    let running = snapshot.process.phase.is_alive();
    let model = &snapshot.model;
    let group = model
        .default_group_id()
        .and_then(|id| model.group(&id).cloned())
        .or_else(|| model.groups.first().cloned());
    let active_node = snapshot
        .selected_node(group.as_ref().map(|group| group.id.as_str()))
        .map(|node| node.id.clone());
    let nodes = model
        .nodes
        .iter()
        .filter(|node| {
            node.enabled
                && group
                    .as_ref()
                    .is_some_and(|group| group.members.contains(&node.id))
        })
        .map(|node| {
            let delay = snapshot
                .latency
                .get(&node.id)
                .and_then(|entry| entry.delay_ms);
            DashboardNode {
                id: node.id.clone(),
                label: node.name.clone(),
                delay_ms: delay,
                active: active_node.as_deref() == Some(node.id.as_str()),
            }
        })
        .collect();
    let et_net = snapshot
        .mesh
        .networks
        .iter()
        .find(|n| n.provider == xrose_core::mesh::MeshProvider::EasyTier);
    let et_phase = et_net
        .map(|n| n.phase)
        .unwrap_or(xrose_core::mesh::MeshPhase::Disconnected);
    let et_profile = snapshot
        .mesh
        .profiles
        .iter()
        .find(|p| p.provider == xrose_core::mesh::MeshProvider::EasyTier);
    let et_ip = et_net.and_then(|n| {
        n.addresses
            .first()
            .map(|addr| addr.split('/').next().unwrap_or(addr).to_string())
    });
    let easytier = DashboardMeshStatus {
        provider: xrose_core::mesh::MeshProvider::EasyTier,
        phase: et_phase,
        profile_name: et_profile.map(|p| p.name.clone()),
        ip: et_ip,
    };

    let ts_net = snapshot
        .mesh
        .networks
        .iter()
        .find(|n| n.provider == xrose_core::mesh::MeshProvider::Tailscale);
    let ts_phase = ts_net
        .map(|n| n.phase)
        .unwrap_or(xrose_core::mesh::MeshPhase::Disconnected);
    let ts_profile = snapshot
        .mesh
        .profiles
        .iter()
        .find(|p| p.provider == xrose_core::mesh::MeshProvider::Tailscale);
    let ts_ip = ts_net.and_then(|n| {
        n.addresses
            .first()
            .map(|addr| addr.split('/').next().unwrap_or(addr).to_string())
    });
    let tailscale = DashboardMeshStatus {
        provider: xrose_core::mesh::MeshProvider::Tailscale,
        phase: ts_phase,
        profile_name: ts_profile.map(|p| p.name.clone()),
        ip: ts_ip,
    };

    let mut status_line = format!(
        "Xrose v{} · {} ({})",
        snapshot.app_version,
        tr(
            language,
            if running { "运行中" } else { "已停止" },
            if running { "Running" } else { "Stopped" }
        ),
        snapshot.settings.core.display_name()
    );
    if et_phase != xrose_core::mesh::MeshPhase::Disconnected {
        status_line.push_str(&format!(" · EasyTier: {:?}", et_phase));
    }
    if ts_phase != xrose_core::mesh::MeshPhase::Disconnected {
        status_line.push_str(&format!(" · Tailscale: {:?}", ts_phase));
    }
    let traffic_line = running.then(|| {
        format!(
            "↓ {}  ↑ {}  ·  {} {}",
            format_speed(snapshot.runtime.down_speed),
            format_speed(snapshot.runtime.up_speed),
            tr(language, "累计", "total"),
            format_bytes(
                snapshot
                    .runtime
                    .down_total
                    .saturating_add(snapshot.runtime.up_total)
            )
        )
    });
    Dashboard {
        language,
        status_line,
        traffic_line,
        group_id: group
            .as_ref()
            .filter(|g| g.enabled && g.manually_selectable())
            .map(|g| g.id.clone()),
        group_name: group
            .map(|group| group.name)
            .unwrap_or_else(|| tr(language, "代理", "Proxy").to_string()),
        nodes,
        running,
        show_speed: snapshot.settings.show_tray_speed,
        easytier,
        tailscale,
        proxy_address: format!("127.0.0.1:{}", snapshot.settings.mixed_port),
        down_speed: snapshot.runtime.down_speed,
        up_speed: snapshot.runtime.up_speed,
        system_proxy: snapshot.system_proxy_active,
        tun_enabled: snapshot.settings.effective_tun(),
        tun_supported: snapshot.settings.core.capabilities().native_tun,
        route_preset: snapshot.model.routing.preset,
        total: format_bytes(
            snapshot
                .runtime
                .down_total
                .saturating_add(snapshot.runtime.up_total),
        ),
        core_name: snapshot.settings.core.display_name().to_string(),
        busy_core: snapshot.busy.iter().any(|key| key == "core"),
        busy_latency: snapshot
            .busy
            .iter()
            .any(|key| matches!(key.as_str(), "latency" | "download" | "auto-probe")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    ToggleCore,
    TestAll,
    SelectNode { group_id: String, node_id: String },
    ToggleSystemProxy,
    ToggleTun,
    SetRoutePreset(RoutePreset),
    ShowWindow,
    ShowSettings,
    Quit,
}

/// A command that needs the GPUI main loop (it owns the window and the quit
/// path). Everything else is applied directly from the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    TogglePopup,
    ShowWindow,
    ShowSettings,
    Quit,
}

pub fn dispatch(action: TrayAction, service: &AppService) -> Option<TrayCommand> {
    match action {
        TrayAction::ToggleCore => service.toggle_core(),
        TrayAction::TestAll => service.test_latency(LatencyScope::All),
        TrayAction::SelectNode { group_id, node_id } => {
            service.select_node(&group_id, &node_id);
        }
        TrayAction::ToggleSystemProxy => {
            let enabled = service.settings().system_proxy;
            service.set_system_proxy(!enabled);
        }
        TrayAction::ToggleTun => service.set_tun_enabled(!service.settings().effective_tun()),
        TrayAction::SetRoutePreset(preset) => service.switch_route_preset(preset),
        TrayAction::ShowWindow => return Some(TrayCommand::ShowWindow),
        TrayAction::ShowSettings => return Some(TrayCommand::ShowSettings),
        TrayAction::Quit => return Some(TrayCommand::Quit),
    }
    None
}
