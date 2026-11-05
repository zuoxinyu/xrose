//! EasyTier connection preferences and device-local forwarding policy.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum P2pPolicy {
    #[default]
    Automatic,
    OnDemand,
    Disabled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TcpAcceleration {
    #[default]
    Off,
    Kcp,
    Quic,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EasyTierOptions {
    pub p2p: P2pPolicy,
    pub p2p_only: bool,
    pub request_p2p: bool,
    pub latency_first: bool,
    pub ipv6_transport: bool,
    pub tcp_acceleration: TcpAcceleration,
    pub disable_encryption: bool,
    /// None uses the built-in EasyTier MTU; the backend accounts for encryption overhead.
    pub mtu: Option<u16>,
    pub device: EasyTierDeviceOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EasyTierDeviceOptions {
    pub bind_physical: bool,
    pub multi_thread: bool,
    pub userspace_stack: bool,
    pub accept_kcp: bool,
    pub accept_quic: bool,
    pub tcp_hole_punching: bool,
    pub udp_hole_punching: bool,
    pub symmetric_nat_punching: bool,
    pub port_mapping: bool,
    pub relay_peer_rpc: bool,
    pub private_mode: bool,
    pub system_forwarding: bool,
    pub udp_broadcast_relay: bool,
}

impl Default for EasyTierDeviceOptions {
    fn default() -> Self {
        Self {
            bind_physical: true,
            multi_thread: true,
            userspace_stack: false,
            accept_kcp: true,
            accept_quic: true,
            tcp_hole_punching: true,
            udp_hole_punching: true,
            symmetric_nat_punching: true,
            port_mapping: false,
            relay_peer_rpc: false,
            private_mode: false,
            system_forwarding: false,
            udp_broadcast_relay: false,
        }
    }
}

impl EasyTierOptions {
    pub(super) fn validate(&self) -> crate::Result<()> {
        if self.mtu.is_some_and(|mtu| !(400..=1380).contains(&mtu)) {
            return Err(crate::Error::config(
                "EasyTier MTU 必须为 400–1380，或留空使用默认值",
            ));
        }
        if !cfg!(windows) && self.device.udp_broadcast_relay {
            return Err(crate::Error::config(
                "EasyTier UDP 广播转发仅支持 Windows，请关闭此选项",
            ));
        }
        Ok(())
    }
}
