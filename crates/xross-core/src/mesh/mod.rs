//! Mesh networking intent and redacted UI state, independent of proxy profiles.

pub mod coexist;
pub mod controller;
pub mod driver;
#[cfg(windows)]
pub mod easytier;
pub mod ipc;
pub mod platform;
pub mod tailscale;
pub mod worker;

use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const EASYTIER_VERSION: &str = "2.6.4";
pub const TAILSCALE_CORE_VERSION: &str = "1.14.4";
pub const TAILSCALE_TUN_ADDRESS: &str = "10.255.253.1/30";
pub const TAILSCALE_TUN_NETWORK: &str = "10.255.253.0/30";
pub const TAILSCALE_TUN_IPV6_ADDRESS: &str = "fdfe:dcba:9877::1/126";
pub const TAILSCALE_TUN_IPV6_NETWORK: &str = "fdfe:dcba:9877::/126";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshProvider {
    #[default]
    EasyTier,
    Tailscale,
}
impl MeshProvider {
    pub fn key(self) -> &'static str {
        match self {
            Self::EasyTier => "easytier",
            Self::Tailscale => "tailscale",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::EasyTier => "EasyTier",
            Self::Tailscale => "Tailscale",
        }
    }
}

/// Deliberately has no Display implementation. Serialization is only for the
/// encrypted store and authenticated IPC; never include this in a snapshot.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MeshSecret(pub String);
impl std::fmt::Debug for MeshSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MeshProfile {
    pub id: String,
    pub name: String,
    pub provider: MeshProvider,
    pub hostname: String,
    pub auto_connect: bool,
    pub network_name: String,
    pub network_secret: MeshSecret,
    pub peers: Vec<String>,
    /// Empty selects automatic allocation. Otherwise an IPv4 host/prefix.
    pub ipv4: String,
    /// Explicit remote subnet allowlist. Never accepts a default route.
    pub remote_subnets: Vec<String>,
    pub control_url: String,
}
impl Default for MeshProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "异地组网".into(),
            provider: MeshProvider::EasyTier,
            hostname: "xross".into(),
            auto_connect: false,
            network_name: String::new(),
            network_secret: MeshSecret::default(),
            peers: Vec::new(),
            ipv4: String::new(),
            remote_subnets: Vec::new(),
            control_url: String::new(),
        }
    }
}
impl MeshProfile {
    pub fn validate(&self) -> Result<()> {
        if self.auto_connect {
            return Err(Error::config("首期仅支持手动连接，请关闭 auto_connect"));
        }
        uuid::Uuid::parse_str(&self.id).map_err(|_| Error::config("组网配置 ID 无效"))?;
        if self.name.trim().is_empty() || self.name.len() > 256 {
            return Err(Error::config("请填写有效的组网名称（最多 256 字节）"));
        }
        if self.hostname.is_empty()
            || self.hostname.len() > 63
            || self.hostname.starts_with('-')
            || self.hostname.ends_with('-')
            || !self
                .hostname
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(Error::config(
                "设备名称只能包含字母、数字和连字符，长度为 1–63",
            ));
        }
        if self.provider == MeshProvider::EasyTier {
            if self.network_name.trim().is_empty() || self.network_secret.0.is_empty() {
                return Err(Error::config("请填写 EasyTier 网络名称和密钥"));
            }
            if self.peers.is_empty() {
                return Err(Error::config("请填写至少一个 EasyTier 入口节点"));
            }
            for peer in &self.peers {
                let u =
                    url::Url::parse(peer).map_err(|_| Error::config("EasyTier 入口地址无效"))?;
                if !["tcp", "udp", "quic", "ws", "wss", "wg"].contains(&u.scheme())
                    || u.host_str().is_none()
                    || !u.username().is_empty()
                    || u.password().is_some()
                    || u.query().is_some()
                    || u.fragment().is_some()
                {
                    return Err(Error::config(
                        "EasyTier 入口必须是 tcp/udp/quic/ws/wss/wg 地址，且不能包含用户凭据",
                    ));
                }
            }
        }
        if !self.ipv4.is_empty() {
            let ip = self
                .ipv4
                .parse::<ipnet::Ipv4Net>()
                .map_err(|_| Error::config("虚拟 IPv4 格式应为地址/前缀"))?;
            if ip.prefix_len() < 8
                || ip.addr().is_loopback()
                || ip.addr().is_unspecified()
                || ip.addr().is_multicast()
                || ip.addr().is_broadcast()
            {
                return Err(Error::config("虚拟 IPv4 必须是有效单播地址"));
            }
        }
        for net in &self.remote_subnets {
            let n = coexist::safe_network(net)?;
            if self.provider == MeshProvider::EasyTier && n.addr().is_ipv6() {
                return Err(Error::config("EasyTier 首期仅支持 IPv4 远端网段"));
            }
        }
        if self.peers.len() > 32
            || self.remote_subnets.len() > 128
            || self.network_name.len() > 256
            || self.network_secret.0.len() > 4096
        {
            return Err(Error::config("组网配置过大，请减少入口或网段数量"));
        }
        if !self.control_url.is_empty() {
            let u = url::Url::parse(&self.control_url)
                .map_err(|_| Error::config("控制服务器地址无效"))?;
            if u.scheme() != "https"
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err(Error::config("控制服务器必须使用不含凭据的 HTTPS 地址"));
            }
        }
        Ok(())
    }
    pub fn summary(&self) -> MeshProfileView {
        MeshProfileView {
            id: self.id.clone(),
            name: self.name.clone(),
            provider: self.provider,
            hostname: self.hostname.clone(),
            auto_connect: self.auto_connect,
            network_name: self.network_name.clone(),
            has_secret: !self.network_secret.0.is_empty(),
            peers: self.peers.clone(),
            ipv4: self.ipv4.clone(),
            remote_subnets: self.remote_subnets.clone(),
            control_url: self.control_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshProfileView {
    pub id: String,
    pub name: String,
    pub provider: MeshProvider,
    pub hostname: String,
    pub auto_connect: bool,
    pub network_name: String,
    pub has_secret: bool,
    pub peers: Vec<String>,
    pub ipv4: String,
    pub remote_subnets: Vec<String>,
    pub control_url: String,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshPhase {
    #[default]
    Disconnected,
    Starting,
    NeedsLogin,
    Connected,
    Stopping,
    Failed,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeshPeer {
    pub id: String,
    pub name: String,
    pub addresses: Vec<String>,
    pub online: bool,
    pub latency_ms: Option<f64>,
    pub connection: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeshRuntimeView {
    #[serde(default)]
    pub probe: Option<MeshProbe>,
    pub profile_id: String,
    pub provider: MeshProvider,
    pub phase: MeshPhase,
    pub interface: String,
    pub addresses: Vec<String>,
    pub routes: Vec<String>,
    pub peers: Vec<MeshPeer>,
    pub dns_suffix: Option<String>,
    pub dns_address: Option<String>,
    /// Ephemeral UI-only login action, never written to the database or log.
    pub login_url: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshProbe {
    pub ip: String,
    pub success: bool,
    pub latency_ms: Option<f64>,
    pub detail: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeshSnapshot {
    pub profiles: Vec<MeshProfileView>,
    pub networks: Vec<MeshRuntimeView>,
    pub tailscale_installed: bool,
    pub supported: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_never_enter_debug_or_view() {
        let p = MeshProfile {
            network_secret: MeshSecret("fake-test-secret".into()),
            ..Default::default()
        };
        assert!(!format!("{p:?}").contains("fake-test-secret"));
        assert!(!serde_json::to_string(&p.summary())
            .unwrap()
            .contains("fake-test-secret"));
    }
    #[test]
    fn rejects_default_routes_and_url_credentials() {
        let mut p = MeshProfile {
            provider: MeshProvider::Tailscale,
            remote_subnets: vec!["0.0.0.0/0".into()],
            ..Default::default()
        };
        assert!(p.validate().is_err());
        p.remote_subnets.clear();
        p.control_url = "https://user:secret@example.com".into();
        assert!(p.validate().is_err());
    }
}
