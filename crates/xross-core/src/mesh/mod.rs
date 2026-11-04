//! Mesh networking intent and redacted UI state, independent of proxy profiles.

pub mod coexist;
pub mod controller;
pub mod driver;
pub mod easytier;
mod easytier_gateway;
pub mod ipc;
pub mod platform;
pub mod tailscale;
pub mod worker;

use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;
pub const DNS_ADDRESS: &str = if cfg!(target_os = "macos") {
    "127.0.0.1"
} else {
    "127.0.0.153"
};
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
    pub exit_node: String,
    pub advertise_exit_node: bool,
    pub advertised_subnets: Vec<String>,
    pub port_forwards: Vec<PortForward>,
    /// Zero disables the loopback gateway and its generated outbound node.
    pub outbound_port: u16,
    pub outbound_secret: MeshSecret,
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
            exit_node: String::new(),
            advertise_exit_node: false,
            advertised_subnets: Vec::new(),
            port_forwards: Vec::new(),
            outbound_port: 0,
            outbound_secret: MeshSecret::default(),
        }
    }
}
impl MeshProfile {
    pub fn interface_name(&self) -> String {
        if cfg!(target_os = "macos") {
            let id = uuid::Uuid::parse_str(&self.id)
                .map(|v| v.as_u128())
                .unwrap_or(0);
            format!("utun{}", 100 + id % 10000)
        } else if cfg!(target_os = "linux") {
            format!(
                "xm{}{}",
                if self.provider == MeshProvider::EasyTier {
                    "et"
                } else {
                    "ts"
                },
                &self.id[..8]
            )
        } else {
            format!(
                "xross-mesh-{}-{}",
                if self.provider == MeshProvider::EasyTier {
                    "et"
                } else {
                    "ts"
                },
                &self.id[..8]
            )
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.auto_connect {
            return Err(Error::config(
                "请关闭 auto_connect，并在应用启动设置中选择组网配置",
            ));
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
                return Err(Error::config("EasyTier 远端网段仅支持 IPv4"));
            }
        }
        for net in &self.advertised_subnets {
            let net = coexist::safe_network(net)?;
            if self.provider == MeshProvider::EasyTier && net.addr().is_ipv6() {
                return Err(Error::config("EasyTier 子网发布仅支持 IPv4"));
            }
        }
        if !self.exit_node.is_empty() {
            if self.advertise_exit_node {
                return Err(Error::config("使用远端出口与发布本机出口不能同时启用"));
            }
            if self.provider == MeshProvider::EasyTier {
                let ip = self
                    .exit_node
                    .parse::<std::net::Ipv4Addr>()
                    .map_err(|_| Error::config("EasyTier 出口节点必须是虚拟 IPv4 地址"))?;
                if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast()
                {
                    return Err(Error::config("出口节点地址无效"));
                }
            } else if self.exit_node.len() > 253
                || !self
                    .exit_node
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c))
            {
                return Err(Error::config("Tailscale 出口节点必须是设备名或 IP 地址"));
            }
        }
        if self.outbound_port != 0 && (self.exit_node.is_empty() || self.outbound_port < 1024) {
            return Err(Error::config(
                "出口出站需要指定出口节点和 1024–65535 之间的本地端口",
            ));
        }
        if self.outbound_port != 0
            && (self.outbound_secret.0.is_empty() || self.outbound_secret.0.len() > 255)
        {
            return Err(Error::config("出口出站凭据无效，请重新保存组网配置"));
        }
        if self.advertised_subnets.len() > 128 || self.port_forwards.len() > 64 {
            return Err(Error::config("子网或转发规则数量过多"));
        }
        for (index, rule) in self.port_forwards.iter().enumerate() {
            rule.validate()?;
            if self.provider == MeshProvider::EasyTier && rule.target.ip().is_ipv6() {
                return Err(Error::config("EasyTier 转发目标仅支持 IPv4"));
            }
            if (rule.listen.port() == self.outbound_port
                && (rule.listen.ip().is_loopback() || rule.listen.ip().is_unspecified()))
                || self.port_forwards[..index].iter().any(|other| {
                    other.protocol == rule.protocol
                        && other.listen.port() == rule.listen.port()
                        && (other.listen.ip() == rule.listen.ip()
                            || other.listen.ip().is_unspecified()
                            || rule.listen.ip().is_unspecified())
                })
            {
                return Err(Error::config("端口转发监听地址重复或与出口出站冲突"));
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
            exit_node: self.exit_node.clone(),
            advertise_exit_node: self.advertise_exit_node,
            advertised_subnets: self.advertised_subnets.clone(),
            port_forwards: self.port_forwards.clone(),
            outbound_port: self.outbound_port,
        }
    }
}

/// Local listener forwarding to a literal mesh destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForward {
    pub protocol: ForwardProtocol,
    pub listen: std::net::SocketAddr,
    pub target: std::net::SocketAddr,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardProtocol {
    Tcp,
    Udp,
}
impl ForwardProtocol {
    pub fn key(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}
impl PortForward {
    pub fn validate(&self) -> Result<()> {
        if self.listen.port() == 0
            || self.target.port() == 0
            || self.target.ip().is_unspecified()
            || self.target.ip().is_multicast()
            || self.listen.ip().is_multicast()
        {
            return Err(Error::config("转发端口必须大于 0，目标必须是有效单播 IP"));
        }
        Ok(())
    }
    pub fn parse(value: &str) -> Result<Self> {
        let (scheme, rest) = value
            .trim()
            .split_once("://")
            .ok_or_else(|| Error::config("转发格式：tcp://127.0.0.1:8080/10.126.126.2:80"))?;
        let protocol = match scheme {
            "tcp" => ForwardProtocol::Tcp,
            "udp" => ForwardProtocol::Udp,
            _ => return Err(Error::config("转发协议仅支持 tcp 和 udp")),
        };
        let (listen, target) = rest
            .split_once('/')
            .ok_or_else(|| Error::config("请填写转发监听地址和目标地址"))?;
        let result = Self {
            protocol,
            listen: listen
                .parse()
                .map_err(|_| Error::config("转发监听地址无效"))?,
            target: target
                .parse()
                .map_err(|_| Error::config("转发目标地址无效"))?,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn text(&self) -> String {
        format!("{}://{}/{}", self.protocol.key(), self.listen, self.target)
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
    pub exit_node: String,
    pub advertise_exit_node: bool,
    pub advertised_subnets: Vec<String>,
    pub port_forwards: Vec<PortForward>,
    /// Zero disables the loopback gateway and its generated outbound node.
    pub outbound_port: u16,
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
            outbound_secret: MeshSecret("fake-outbound-secret".into()),
            ..Default::default()
        };
        assert!(!format!("{p:?}").contains("fake-test-secret"));
        assert!(!format!("{p:?}").contains("fake-outbound-secret"));
        assert!(
            !serde_json::to_string(&p.summary())
                .unwrap()
                .contains("fake-outbound-secret")
        );
        assert!(
            !serde_json::to_string(&p.summary())
                .unwrap()
                .contains("fake-test-secret")
        );
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

    #[test]
    fn publication_exit_and_forward_validation() {
        let mut p = MeshProfile {
            provider: MeshProvider::Tailscale,
            exit_node: "exit.example.ts.net".into(),
            outbound_port: 19081,
            outbound_secret: MeshSecret("fixture".into()),
            advertised_subnets: vec!["192.168.50.0/24".into()],
            port_forwards: vec![PortForward::parse("udp://127.0.0.1:5533/100.64.1.2:53").unwrap()],
            ..Default::default()
        };
        assert!(p.validate().is_ok());
        p.advertise_exit_node = true;
        assert!(p.validate().is_err());
        p.advertise_exit_node = false;
        p.advertised_subnets.push("::/0".into());
        assert!(p.validate().is_err());
        p.advertised_subnets.pop();
        p.port_forwards
            .push(PortForward::parse("tcp://0.0.0.0:19081/100.64.1.2:80").unwrap());
        assert!(p.validate().is_err());
        assert!(PortForward::parse("tcp://127.0.0.1:0/192.0.2.1:80").is_err());
        assert!(PortForward::parse("http://127.0.0.1:8888/192.0.2.1:80").is_err());
        let v6 = PortForward::parse("tcp://[::1]:8080/[fd00::2]:80").unwrap();
        assert_eq!(PortForward::parse(&v6.text()).unwrap(), v6);
        let old: MeshProfile = serde_json::from_str(r#"{"provider":"tailscale"}"#).unwrap();
        assert_eq!(old.outbound_port, 0);
        assert!(!old.advertise_exit_node);
        assert!(old.validate().is_ok());
    }
}
