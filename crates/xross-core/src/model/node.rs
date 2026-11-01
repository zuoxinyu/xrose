use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Vmess,
    Vless,
    Trojan,
    Shadowsocks,
    Hysteria2,
    Tuic,
    Socks,
    Http,
    Wireguard,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Vmess => "vmess",
            Protocol::Vless => "vless",
            Protocol::Trojan => "trojan",
            Protocol::Shadowsocks => "ss",
            Protocol::Hysteria2 => "hysteria2",
            Protocol::Tuic => "tuic",
            Protocol::Socks => "socks",
            Protocol::Http => "http",
            Protocol::Wireguard => "wireguard",
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            Protocol::Vmess => "VMess",
            Protocol::Vless => "VLESS",
            Protocol::Trojan => "Trojan",
            Protocol::Shadowsocks => "Shadowsocks",
            Protocol::Hysteria2 => "Hysteria2",
            Protocol::Tuic => "TUIC",
            Protocol::Socks => "SOCKS",
            Protocol::Http => "HTTP",
            Protocol::Wireguard => "WireGuard",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vmess" => Some(Protocol::Vmess),
            "vless" => Some(Protocol::Vless),
            "trojan" => Some(Protocol::Trojan),
            "ss" | "shadowsocks" => Some(Protocol::Shadowsocks),
            "hysteria2" | "hy2" => Some(Protocol::Hysteria2),
            "tuic" => Some(Protocol::Tuic),
            "socks" | "socks5" => Some(Protocol::Socks),
            "http" | "https" => Some(Protocol::Http),
            "wireguard" | "wg" => Some(Protocol::Wireguard),
            _ => None,
        }
    }
    pub fn supported_by(&self, core: crate::model::CoreKind) -> bool {
        use crate::model::CoreKind;
        match core {
            CoreKind::Xray => !matches!(self, Protocol::Hysteria2 | Protocol::Tuic),
            CoreKind::SingBox => true,
        }
    }
    pub fn default_port(&self) -> u16 {
        match self {
            Protocol::Http => 80,
            Protocol::Socks => 1080,
            Protocol::Wireguard => 51820,
            _ => 443,
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Tcp,
    Udp,
    TcpUdp,
}

impl Network {
    pub fn as_str(&self) -> &'static str {
        match self {
            Network::Tcp => "tcp",
            Network::Udp => "udp",
            Network::TcpUdp => "tcp,udp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealityOptions {
    pub public_key: String,
    #[serde(default)]
    pub short_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct TlsOptions {
    pub enabled: bool,
    pub server_name: Option<String>,
    pub insecure: bool,
    pub alpn: Vec<String>,
    /// uTLS fingerprint, e.g. `chrome`. Xray only.
    pub fingerprint: Option<String>,
    pub reality: Option<RealityOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Transport {
    Tcp,
    WebSocket {
        #[serde(default = "default_ws_path")]
        path: String,
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    Grpc {
        #[serde(default)]
        service_name: String,
    },
    Http2 {
        #[serde(default = "default_ws_path")]
        path: String,
        #[serde(default)]
        host: Option<String>,
    },
    HttpUpgrade {
        #[serde(default = "default_ws_path")]
        path: String,
        #[serde(default)]
        host: Option<String>,
    },
    Quic,
    Kcp {
        #[serde(default)]
        seed: Option<String>,
    },
}

fn default_ws_path() -> String {
    "/".into()
}

impl Default for Transport {
    fn default() -> Self {
        Transport::Tcp
    }
}

impl Transport {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::WebSocket { .. } => "ws",
            Transport::Grpc { .. } => "grpc",
            Transport::Http2 { .. } => "h2",
            Transport::HttpUpgrade { .. } => "httpupgrade",
            Transport::Quic => "quic",
            Transport::Kcp { .. } => "kcp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Credentials {
    pub uuid: Option<String>,
    pub password: Option<String>,
    pub username: Option<String>,
    pub method: Option<String>,
    pub flow: Option<String>,
    pub alter_id: u32,
    pub plugin: Option<String>,
    pub plugin_opts: Option<String>,
    pub obfs: Option<String>,
    pub obfs_password: Option<String>,
    pub congestion_control: Option<String>,
    pub udp_relay_mode: Option<String>,
}

impl Credentials {
    /// Hash of the secret material, used for de-duplication without leaking it.
    pub fn secret_fingerprint(&self) -> String {
        let mut secret = String::new();
        for v in [&self.uuid, &self.password, &self.username, &self.method] {
            if let Some(v) = v {
                secret.push_str(v);
            }
        }
        crate::util::sha256_bytes(secret.as_bytes())[..16].to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeSource {
    #[default]
    Manual,
    Subscription,
    Import,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub server: String,
    pub port: u16,
    #[serde(default)]
    pub credentials: Credentials,
    #[serde(default)]
    pub tls: TlsOptions,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub udp: bool,
    #[serde(default)]
    pub subscription_id: Option<String>,
    #[serde(default)]
    pub source: NodeSource,
    #[serde(default)]
    pub sort: i64,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Node {
    pub fn new(
        name: impl Into<String>,
        protocol: Protocol,
        server: impl Into<String>,
        port: u16,
    ) -> Self {
        Self {
            id: new_id("node"),
            name: name.into(),
            protocol,
            server: server.into(),
            port,
            credentials: Credentials::default(),
            tls: TlsOptions::default(),
            transport: Transport::default(),
            enabled: true,
            udp: true,
            subscription_id: None,
            source: NodeSource::Manual,
            sort: 0,
            tags: Vec::new(),
        }
    }

    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.protocol.as_str(),
            self.server.to_ascii_lowercase(),
            self.port,
            self.credentials.username.as_deref().unwrap_or(""),
            self.credentials.method.as_deref().unwrap_or(""),
            self.credentials.secret_fingerprint()
        )
    }

    pub fn endpoint(&self) -> String {
        crate::util::host_port(&self.server, self.port)
    }

    pub fn outbound_tag(&self) -> String {
        format!("node-{}", self.id)
    }

    pub fn usable_with(&self, core: crate::model::CoreKind) -> bool {
        self.enabled && self.protocol.supported_by(core) && !self.server.is_empty() && self.port > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    #[default]
    Selector,
    UrlTest,
    Fallback,
    LoadBalance,
}

impl GroupKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GroupKind::Selector => "selector",
            GroupKind::UrlTest => "urltest",
            GroupKind::Fallback => "fallback",
            GroupKind::LoadBalance => "loadbalance",
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            GroupKind::Selector => "手动选择",
            GroupKind::UrlTest => "自动测速",
            GroupKind::Fallback => "故障转移",
            GroupKind::LoadBalance => "负载均衡",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "select" | "selector" => Some(GroupKind::Selector),
            "url-test" | "urltest" => Some(GroupKind::UrlTest),
            "fallback" => Some(GroupKind::Fallback),
            "load-balance" | "loadbalance" => Some(GroupKind::LoadBalance),
            _ => None,
        }
    }
}

fn default_interval() -> u64 {
    300
}
fn default_tolerance() -> u32 {
    50
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboundGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: GroupKind,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_tolerance")]
    pub tolerance_ms: u32,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl OutboundGroup {
    pub fn new(name: impl Into<String>, kind: GroupKind) -> Self {
        Self {
            id: new_id("group"),
            name: name.into(),
            kind,
            members: Vec::new(),
            selected: None,
            interval_secs: default_interval(),
            tolerance_ms: default_tolerance(),
            url: None,
            enabled: true,
        }
    }
    pub fn tag(&self) -> String {
        format!("group-{}", self.id)
    }
    pub fn active_node(&self) -> Option<&String> {
        self.selected.as_ref().or_else(|| self.members.first())
    }
    /// Repairs a stale selection. Returns true when it changed.
    pub fn repair(&mut self) -> bool {
        let before = self.selected.clone();
        if self.members.is_empty() {
            self.selected = None;
        } else if self
            .selected
            .as_ref()
            .map(|s| !self.members.contains(s))
            .unwrap_or(true)
        {
            self.selected = self.members.first().cloned();
        }
        before != self.selected
    }
}
