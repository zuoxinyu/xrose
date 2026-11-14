//! Lightweight runtime state for the native status item.
use super::*;
use crate::core::ProcessPhase;
use crate::mesh::{MeshPhase, MeshProvider};
use crate::model::WorkMode;
use crate::traffic_statistics::CoreEpoch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayIndicator {
    Idle,
    Direct,
    Proxy,
    Mesh,
    MeshOnly,
    MeshProxy,
    Unavailable,
}

impl TrayIndicator {
    fn resolve(
        phase: ProcessPhase,
        mode: Option<WorkMode>,
        failed_lifecycle: bool,
        failed_probe: bool,
        mesh: bool,
    ) -> Self {
        match phase {
            ProcessPhase::Failed => Self::Unavailable,
            ProcessPhase::Stopped | ProcessPhase::Starting if failed_lifecycle => Self::Unavailable,
            ProcessPhase::Running if mode == Some(WorkMode::Proxy) => {
                if failed_probe {
                    Self::Unavailable
                } else if mesh {
                    Self::MeshProxy
                } else {
                    Self::Proxy
                }
            }
            ProcessPhase::Running if mode == Some(WorkMode::Mesh) => {
                if mesh && !failed_probe {
                    Self::Mesh
                } else {
                    Self::Unavailable
                }
            }
            _ if mesh => Self::MeshOnly,
            ProcessPhase::Running if mode == Some(WorkMode::Direct) => Self::Direct,
            _ => Self::Idle,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TrayStatus {
    pub language: crate::settings::Language,
    pub core: CoreKind,
    pub phase: ProcessPhase,
    pub indicator: TrayIndicator,
    pub easytier: MeshPhase,
    pub tailscale: MeshPhase,
    pub show_speed: bool,
    pub show_indicator: bool,
    pub up_speed: u64,
    pub down_speed: u64,
    pub total_bytes: u64,
}

/// Probe results belong to the node and run that were active when the measurement began.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProbeTarget {
    epoch: CoreEpoch,
    node: String,
}

impl AppService {
    pub(super) fn tray_probe_target(&self) -> Option<ProbeTarget> {
        let process = self.process_state();
        if process.phase != ProcessPhase::Running {
            return None;
        }
        let epoch = CoreEpoch::from_state(&process)?;
        let node = self.running_proxy_node(process.phase)?;
        Some(ProbeTarget { epoch, node })
    }

    pub(super) fn record_proxy_probe(&self, target: Option<&ProbeTarget>, id: &str, ok: bool) {
        if let Some(target) = target
            && target.node == id
            && self.tray_probe_target().as_ref() == Some(target)
        {
            *self.inner.failed_proxy_probe.lock() = (!ok).then(|| target.clone());
        }
    }

    pub fn tray_status(&self) -> TrayStatus {
        let process = self.process_state();
        let (language, core, show_speed) = {
            let s = self.inner.settings.read();
            (
                s.language,
                process.core.unwrap_or(s.core),
                s.show_tray_speed,
            )
        };
        let easytier = self.inner.mesh.phase(MeshProvider::EasyTier);
        let tailscale = self.inner.mesh.phase(MeshProvider::Tailscale);
        let failed = self.inner.failed_proxy_probe.lock().clone();
        let failed_probe = failed.is_some() && failed == self.tray_probe_target();
        let mode = self.inner.core.applied_work_mode();
        let mesh_connected = if mode == Some(WorkMode::Mesh) {
            self.model().mesh_mode.target.is_some_and(|target| {
                self.inner.mesh.snapshot().networks.iter().any(|network| {
                    network.profile_id == target.profile_id
                        && network.phase == MeshPhase::Connected
                        && network.peers.iter().any(|peer| {
                            peer.id == target.peer_id && peer.online && peer.exit_selected
                        })
                })
            })
        } else {
            easytier == MeshPhase::Connected || tailscale == MeshPhase::Connected
        };
        let indicator = TrayIndicator::resolve(
            process.phase,
            mode,
            self.inner.last_start_failed.load(Ordering::Relaxed)
                || (process.phase == ProcessPhase::Starting && process.restarts > 0),
            failed_probe,
            mesh_connected,
        );
        let (up, down, up_speed, down_speed) = self.inner.traffic.lock().snapshot();
        TrayStatus {
            language,
            core,
            phase: process.phase,
            indicator,
            easytier,
            tailscale,
            show_speed,
            // Keep the icon monochrome while the status indicator artwork is being revised.
            show_indicator: false,
            up_speed,
            down_speed,
            total_bytes: up.saturating_add(down),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_start_reports_unavailable_even_with_the_icon_indicator_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let service = AppService::with_paths(AppPaths::from_root(dir.path())).unwrap();
        let mut settings = service.settings();
        settings.show_tray_indicator = true;
        settings.binary_paths.insert(
            settings.core.as_str().into(),
            dir.path()
                .join("missing-core")
                .to_string_lossy()
                .into_owned(),
        );
        service.update_settings(settings);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Idle);
        service.start_core();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.is_busy("core") {
            assert!(
                std::time::Instant::now() < deadline,
                "core preparation did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(service.tray_status().phase, ProcessPhase::Stopped);
        assert_eq!(service.tray_status().indicator, TrayIndicator::Unavailable);
        assert!(!service.tray_status().show_indicator);
    }

    #[test]
    fn indicator_uses_the_running_mode_and_gives_proxy_failure_priority() {
        use ProcessPhase::*;
        use TrayIndicator::*;
        let proxy = Some(WorkMode::Proxy);
        for (phase, mode, start_failed, probe_failed, mesh, expected) in [
            (Stopped, None, false, false, false, Idle),
            (Stopped, None, false, false, true, MeshOnly),
            (Stopped, None, true, false, false, Unavailable),
            (Starting, proxy, false, false, true, MeshOnly),
            (Starting, proxy, true, false, true, Unavailable),
            (Stopping, proxy, false, false, true, MeshOnly),
            (
                Running,
                Some(WorkMode::Direct),
                false,
                true,
                false,
                TrayIndicator::Direct,
            ),
            (Running, Some(WorkMode::Direct), false, true, true, MeshOnly),
            (Running, proxy, false, false, false, Proxy),
            (Running, proxy, false, false, true, MeshProxy),
            (Running, proxy, false, true, true, Unavailable),
            (Failed, None, false, false, true, Unavailable),
        ] {
            assert_eq!(
                TrayIndicator::resolve(phase, mode, start_failed, probe_failed, mesh),
                expected
            );
        }
    }
}
