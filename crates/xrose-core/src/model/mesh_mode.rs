//! Host traffic intent, independent from proxy-group selections and mesh membership.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshDnsMode {
    #[default]
    Exit,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshExitTarget {
    pub profile_id: String,
    pub peer_id: String,
    pub provider: crate::mesh::MeshProvider,
    pub name: String,
    pub address: String,
    pub gateway_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshModeConfig {
    pub target: Option<MeshExitTarget>,
    pub dns: MeshDnsMode,
    pub allow_lan: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_profiles_do_not_enable_system_takeover() {
        let model: super::super::AppModel = serde_json::from_value(serde_json::json!({
            "nodes": [], "groups": [], "routing": {}, "dns": {}
        }))
        .unwrap();
        assert_eq!(model.work_mode, super::super::WorkMode::Direct);
        assert_eq!(model.mesh_mode, MeshModeConfig::default());
    }
}
