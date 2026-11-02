//! Content shared by the macOS status-item menu and the Windows tray menu.
//!
//! Both backends render the same [`Dashboard`] so the two platforms cannot
//! drift: status header, live traffic, the first outbound nodes with latency
//! and a checked marker, then the same actions.

use xross_core::service::LatencyScope;
use xross_core::settings::Language;
use xross_core::util::{format_bytes, format_speed};
use xross_core::AppService;

use crate::i18n::tr;

/// Nodes shown in the tray menu before the list is cut off. Menus longer than
/// this are unusable on a small screen.
pub const MAX_NODES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardNode {
    pub id: String,
    /// Already formatted: `● 东京 01 (23ms)` / `○ 新加坡 02`.
    pub label: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub language: Language,
    pub status_line: String,
    pub traffic_line: Option<String>,
    pub group_id: Option<String>,
    pub group_name: String,
    pub nodes: Vec<DashboardNode>,
    pub running: bool,
    pub system_proxy: bool,
    pub down_speed: u64,
    pub up_speed: u64,
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
    let active_node = group
        .as_ref()
        .and_then(|group| group.active_node().cloned());
    let nodes = model
        .nodes
        .iter()
        .filter(|node| node.enabled)
        .take(MAX_NODES)
        .map(|node| {
            let delay = snapshot
                .latency
                .get(&node.id)
                .and_then(|entry| entry.delay_ms)
                .map(|ms| format!(" ({ms}ms)"))
                .unwrap_or_default();
            DashboardNode {
                id: node.id.clone(),
                label: format!(
                    "{} {}{}",
                    if active_node.as_deref() == Some(node.id.as_str()) {
                        "●"
                    } else {
                        "○"
                    },
                    node.name,
                    delay
                ),
                active: active_node.as_deref() == Some(node.id.as_str()),
            }
        })
        .collect();
    let status_line = format!(
        "Xross v{} · {} ({})",
        snapshot.app_version,
        tr(
            language,
            if running { "运行中" } else { "已停止" },
            if running { "Running" } else { "Stopped" }
        ),
        snapshot.settings.core.display_name()
    );
    let traffic_line = running.then(|| {
        format!(
            "↓ {}  ↑ {}  ·  {} {}",
            format_speed(snapshot.runtime.down_speed),
            format_speed(snapshot.runtime.up_speed),
            tr(language, "累计", "total"),
            format_bytes(snapshot.runtime.down_total + snapshot.runtime.up_total)
        )
    });
    Dashboard {
        language,
        status_line,
        traffic_line,
        group_id: group.as_ref().map(|group| group.id.clone()),
        group_name: group
            .map(|group| group.name)
            .unwrap_or_else(|| "代理".to_string()),
        nodes,
        running,
        down_speed: snapshot.runtime.down_speed,
        up_speed: snapshot.runtime.up_speed,
        system_proxy: snapshot.system_proxy_active,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    ToggleCore,
    TestAll,
    SelectNode { group_id: String, node_id: String },
    ToggleSystemProxy,
    ShowWindow,
    Quit,
}

/// A command that needs the GPUI main loop (it owns the window and the quit
/// path). Everything else is applied directly from the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ShowWindow,
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
        TrayAction::ShowWindow => return Some(TrayCommand::ShowWindow),
        TrayAction::Quit => return Some(TrayCommand::Quit),
    }
    None
}
