use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{OpenConnectOptions, OpenVpnOptions};
use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Vmess,
    #[default]
    Vless,
    Trojan,
    Anytls,
    Shadowsocks,
    Hysteria2,
    Tuic,
    Socks,
    Http,
    Wireguard,
    Openvpn,
    Openconnect,
}

impl Protocol {
    pub fn all() -> [Protocol; 12] {
        [
            Protocol::Vmess,
            Protocol::Vless,
            Protocol::Trojan,
            Protocol::Anytls,
            Protocol::Shadowsocks,
            Protocol::Hysteria2,
            Protocol::Tuic,
            Protocol::Socks,
            Protocol::Http,
            Protocol::Wireguard,
            Protocol::Openvpn,
            Protocol::Openconnect,
        ]
    }

    pub fn manual() -> [Protocol; 12] {
        Self::all()
    }

    /// Whether the protocol carries credentials derived from a UUID.
    pub fn needs_uuid(&self) -> bool {
        matches!(self, Protocol::Vmess | Protocol::Vless | Protocol::Tuic)
    }

    /// Whether the protocol is a plain proxy (no TLS / v2ray transport).
    pub fn is_plain_proxy(&self) -> bool {
        matches!(
            self,
            Protocol::Socks | Protocol::Http | Protocol::Shadowsocks
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::Vmess => "vmess",
            Protocol::Vless => "vless",
            Protocol::Trojan => "trojan",
            Protocol::Anytls => "anytls",
            Protocol::Shadowsocks => "ss",
            Protocol::Hysteria2 => "hysteria2",
            Protocol::Tuic => "tuic",
            Protocol::Socks => "socks",
            Protocol::Http => "http",
            Protocol::Wireguard => "wireguard",
            Protocol::Openvpn => "openvpn",
            Protocol::Openconnect => "openconnect",
        }
    }
    pub fn display_name(&self) -> &'static str {
        match self {
            Protocol::Vmess => "VMess",
            Protocol::Vless => "VLESS",
            Protocol::Trojan => "Trojan",
            Protocol::Anytls => "AnyTLS",
            Protocol::Shadowsocks => "Shadowsocks",
            Protocol::Hysteria2 => "Hysteria2",
            Protocol::Tuic => "TUIC",
            Protocol::Socks => "SOCKS",
            Protocol::Http => "HTTP",
            Protocol::Wireguard => "WireGuard",
            Protocol::Openvpn => "OpenVPN",
            Protocol::Openconnect => "OpenConnect",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vmess" => Some(Protocol::Vmess),
            "vless" => Some(Protocol::Vless),
            "trojan" => Some(Protocol::Trojan),
            "anytls" => Some(Protocol::Anytls),
            "ss" | "shadowsocks" => Some(Protocol::Shadowsocks),
            "hysteria2" | "hy2" => Some(Protocol::Hysteria2),
            "tuic" => Some(Protocol::Tuic),
            "socks" | "socks5" => Some(Protocol::Socks),
            "http" | "https" => Some(Protocol::Http),
            "wireguard" | "wg" => Some(Protocol::Wireguard),
            "openvpn" | "openvpn-client" => Some(Protocol::Openvpn),
            "openconnect" => Some(Protocol::Openconnect),
            _ => None,
        }
    }
    pub fn supported_by(&self, core: crate::model::CoreKind) -> bool {
        use crate::model::CoreKind;
        if *self == Protocol::Openvpn {
            return core.capabilities().openvpn;
        }
        if *self == Protocol::Openconnect {
            return core.capabilities().openconnect;
        }
        if *self == Protocol::Wireguard {
            return core.capabilities().wireguard;
        }
        if *self == Protocol::Anytls {
            return core.capabilities().anytls;
        }
        match core {
            CoreKind::Xray => !matches!(self, Protocol::Hysteria2 | Protocol::Tuic),
            CoreKind::SingBox => true,
        }
    }
    pub fn is_vpn(&self) -> bool {
        matches!(self, Self::Openvpn | Self::Openconnect)
    }

    pub fn default_port(&self) -> u16 {
        match self {
            Protocol::Http => 80,
            Protocol::Socks => 1080,
            Protocol::Wireguard => 51820,
            Protocol::Openvpn => 1194,
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
    pub wireguard: Option<WireguardOptions>,
    pub anytls: Option<AnytlsOptions>,
    pub openvpn: Option<OpenVpnOptions>,
    pub openconnect: Option<OpenConnectOptions>,
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
        if let Some(wg) = &self.wireguard {
            secret.push_str(&serde_json::to_string(wg).unwrap_or_default());
        }
        if let Some(options) = &self.openvpn {
            secret.push_str(&serde_json::to_string(options).unwrap_or_default());
        }
        if let Some(options) = &self.openconnect {
            secret.push_str(&serde_json::to_string(options).unwrap_or_default());
        }
        crate::util::sha256_bytes(secret.as_bytes())[..16].to_string()
    }
}

/// One remote peer per proxy node. Keys remain in the encrypted credential payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WireguardOptions {
    pub private_key: String,
    pub public_key: String,
    pub pre_shared_key: String,
    pub addresses: Vec<String>,
    pub allowed_ips: Vec<String>,
    pub mtu: u16,
    pub reserved: [u8; 3],
}
impl Default for WireguardOptions {
    fn default() -> Self {
        Self {
            private_key: String::new(),
            public_key: String::new(),
            pre_shared_key: String::new(),
            addresses: Vec::new(),
            allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            mtu: 1408,
            reserved: [0; 3],
        }
    }
}
impl WireguardOptions {
    pub fn validate(&self) -> crate::Result<()> {
        for (label, key, required) in [
            ("私钥", &self.private_key, true),
            ("公钥", &self.public_key, true),
            ("预共享密钥", &self.pre_shared_key, false),
        ] {
            if key.is_empty() && !required {
                continue;
            }
            if crate::util::decode_base64_any(key).is_none_or(|bytes| bytes.len() != 32) {
                return Err(crate::Error::config(format!(
                    "WireGuard {label}必须是 32 字节的 Base64 密钥"
                )));
            }
        }
        if self.addresses.is_empty() || self.allowed_ips.is_empty() {
            return Err(crate::Error::config("请填写 WireGuard 本地地址和允许网段"));
        }
        for address in self.addresses.iter().chain(&self.allowed_ips) {
            if address.parse::<ipnet::IpNet>().is_err() {
                return Err(crate::Error::config(
                    "WireGuard 地址和网段必须使用 IP/前缀格式",
                ));
            }
        }
        if self.mtu < 1280 {
            return Err(crate::Error::config("WireGuard MTU 必须在 1280–65535 之间"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct AnytlsOptions {
    pub idle_session_check_interval: Option<String>,
    pub idle_session_timeout: Option<String>,
    pub min_idle_session: Option<u32>,
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

    pub(crate) fn needs_front(&self, front: &Node) -> bool {
        let local_mesh_exit = self.id.starts_with("mesh-exit-")
            && self.tags.iter().any(|t| t == "mesh-exit")
            && self
                .server
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        self.id != front.id && !local_mesh_exit
    }

    pub fn outbound_tag(&self) -> String {
        format!("node-{}", self.id)
    }

    pub fn usable_with(&self, core: crate::model::CoreKind) -> bool {
        self.enabled && self.protocol.supported_by(core) && !self.server.is_empty() && self.port > 0
    }
}

/// Transport selector used by the manual "add node" form. It mirrors
/// [`Transport`] without its payload, so the UI can show a stable list before
/// any path/host/service name has been typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    #[default]
    Tcp,
    WebSocket,
    Grpc,
    Http2,
    HttpUpgrade,
    Quic,
    Kcp,
}

impl TransportKind {
    pub fn all() -> [TransportKind; 5] {
        [
            TransportKind::Tcp,
            TransportKind::WebSocket,
            TransportKind::Grpc,
            TransportKind::Http2,
            TransportKind::HttpUpgrade,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TransportKind::Tcp => "tcp",
            TransportKind::WebSocket => "ws",
            TransportKind::Grpc => "grpc",
            TransportKind::Http2 => "h2",
            TransportKind::HttpUpgrade => "httpupgrade",
            TransportKind::Quic => "quic",
            TransportKind::Kcp => "kcp",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            TransportKind::Tcp => "TCP",
            TransportKind::WebSocket => "WebSocket",
            TransportKind::Grpc => "gRPC",
            TransportKind::Http2 => "HTTP/2",
            TransportKind::HttpUpgrade => "HTTPUpgrade",
            TransportKind::Quic => "QUIC",
            TransportKind::Kcp => "mKCP",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "tcp" | "" => Some(TransportKind::Tcp),
            "ws" | "websocket" => Some(TransportKind::WebSocket),
            "grpc" => Some(TransportKind::Grpc),
            "h2" | "http" | "http2" => Some(TransportKind::Http2),
            "httpupgrade" | "http-upgrade" => Some(TransportKind::HttpUpgrade),
            "quic" => Some(TransportKind::Quic),
            "kcp" | "mkcp" => Some(TransportKind::Kcp),
            _ => None,
        }
    }

    pub fn from_transport(transport: &Transport) -> Self {
        match transport {
            Transport::Tcp => TransportKind::Tcp,
            Transport::WebSocket { .. } => TransportKind::WebSocket,
            Transport::Grpc { .. } => TransportKind::Grpc,
            Transport::Http2 { .. } => TransportKind::Http2,
            Transport::HttpUpgrade { .. } => TransportKind::HttpUpgrade,
            Transport::Quic => TransportKind::Quic,
            Transport::Kcp { .. } => TransportKind::Kcp,
        }
    }
}

/// Flat, serde-friendly description of a node entered by hand.
///
/// The form, the CLI and the tests all speak this type; [`NodeDraft::into_node`]
/// is the single place that validates the combination and turns it into a
/// core-agnostic [`Node`]. Validation messages are Chinese and actionable
/// because the UI renders them verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NodeDraft {
    pub name: String,
    pub protocol: Protocol,
    pub server: String,
    pub port: u16,
    pub uuid: String,
    pub password: String,
    pub username: String,
    /// Shadowsocks cipher, e.g. `aes-256-gcm`.
    pub method: String,
    /// VLESS flow, e.g. `xtls-rprx-vision`.
    pub flow: String,
    pub tls_enabled: bool,
    pub sni: String,
    /// Comma or whitespace separated ALPN list.
    pub alpn: String,
    pub fingerprint: String,
    pub insecure: bool,
    pub reality_public_key: String,
    pub reality_short_id: String,
    pub transport: TransportKind,
    pub transport_path: String,
    pub transport_host: String,
    pub transport_service_name: String,
    pub udp: bool,
    /// Comma separated tags.
    pub tags: String,
    pub wireguard_private_key: String,
    pub wireguard_public_key: String,
    pub wireguard_pre_shared_key: String,
    pub wireguard_addresses: String,
    pub wireguard_allowed_ips: String,
    pub wireguard_mtu: String,
    pub wireguard_reserved: String,
    pub openvpn: OpenVpnOptions,
    pub openconnect: OpenConnectOptions,
}

impl Default for NodeDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            protocol: Protocol::Vless,
            server: String::new(),
            port: 443,
            uuid: String::new(),
            password: String::new(),
            username: String::new(),
            method: String::new(),
            flow: String::new(),
            tls_enabled: true,
            sni: String::new(),
            alpn: String::new(),
            fingerprint: String::new(),
            insecure: false,
            reality_public_key: String::new(),
            reality_short_id: String::new(),
            transport: TransportKind::Tcp,
            transport_path: "/".into(),
            transport_host: String::new(),
            transport_service_name: String::new(),
            udp: true,
            tags: String::new(),
            wireguard_private_key: String::new(),
            wireguard_public_key: String::new(),
            wireguard_pre_shared_key: String::new(),
            wireguard_addresses: String::new(),
            wireguard_allowed_ips: "0.0.0.0/0, ::/0".into(),
            wireguard_mtu: "1408".into(),
            wireguard_reserved: "0,0,0".into(),
            openvpn: OpenVpnOptions::default(),
            openconnect: OpenConnectOptions::default(),
        }
    }
}

impl From<&Node> for NodeDraft {
    fn from(node: &Node) -> Self {
        let (path, host, service_name) = match &node.transport {
            Transport::WebSocket { path, host, .. }
            | Transport::Http2 { path, host }
            | Transport::HttpUpgrade { path, host } => (
                path.clone(),
                host.clone().unwrap_or_default(),
                String::new(),
            ),
            Transport::Grpc { service_name } => ("/".into(), String::new(), service_name.clone()),
            _ => ("/".into(), String::new(), String::new()),
        };
        let wg = node.credentials.wireguard.clone().unwrap_or_default();
        Self {
            openvpn: node.credentials.openvpn.clone().unwrap_or_default(),
            openconnect: node.credentials.openconnect.clone().unwrap_or_default(),
            wireguard_private_key: wg.private_key,
            wireguard_public_key: wg.public_key,
            wireguard_pre_shared_key: wg.pre_shared_key,
            wireguard_addresses: wg.addresses.join(", "),
            wireguard_allowed_ips: wg.allowed_ips.join(", "),
            wireguard_mtu: wg.mtu.to_string(),
            wireguard_reserved: wg
                .reserved
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(","),
            name: node.name.clone(),
            protocol: node.protocol,
            server: node.server.clone(),
            port: node.port,
            uuid: node.credentials.uuid.clone().unwrap_or_default(),
            password: node.credentials.password.clone().unwrap_or_default(),
            username: node.credentials.username.clone().unwrap_or_default(),
            method: node.credentials.method.clone().unwrap_or_default(),
            flow: node.credentials.flow.clone().unwrap_or_default(),
            tls_enabled: node.tls.enabled,
            sni: node.tls.server_name.clone().unwrap_or_default(),
            alpn: node.tls.alpn.join(", "),
            fingerprint: node.tls.fingerprint.clone().unwrap_or_default(),
            insecure: node.tls.insecure,
            reality_public_key: node
                .tls
                .reality
                .as_ref()
                .map(|r| r.public_key.clone())
                .unwrap_or_default(),
            reality_short_id: node
                .tls
                .reality
                .as_ref()
                .map(|r| r.short_id.clone())
                .unwrap_or_default(),
            transport: TransportKind::from_transport(&node.transport),
            transport_path: path,
            transport_host: host,
            transport_service_name: service_name,
            udp: node.udp,
            tags: node.tags.join(", "),
        }
    }
}

impl NodeDraft {
    /// Updates editable parameters while retaining identity and fields absent from the form.
    pub fn apply_to(&self, original: &Node, core: crate::model::CoreKind) -> crate::Result<Node> {
        let mut node = self.into_node(core)?;
        node.id = original.id.clone();
        node.enabled = original.enabled;
        node.subscription_id = original.subscription_id.clone();
        node.source = original.source;
        node.sort = original.sort;
        if self.tags == NodeDraft::from(original).tags {
            node.tags = original.tags.clone();
        }
        if node.protocol == original.protocol {
            let mut credentials = original.credentials.clone();
            credentials.uuid = node.credentials.uuid;
            credentials.password = node.credentials.password;
            credentials.username = node.credentials.username;
            credentials.method = node.credentials.method;
            credentials.flow = node.credentials.flow;
            credentials.wireguard = node.credentials.wireguard;
            credentials.openvpn = node.credentials.openvpn;
            credentials.openconnect = node.credentials.openconnect;
            node.credentials = credentials;
            match (&mut node.transport, &original.transport) {
                (
                    Transport::WebSocket { headers, .. },
                    Transport::WebSocket { headers: old, .. },
                ) => {
                    *headers = old.clone();
                }
                (Transport::Kcp { seed }, Transport::Kcp { seed: old }) => *seed = old.clone(),
                _ => {}
            }
            // Imported HTTPS proxies can carry TLS even though the manual proxy form omits it.
            if original.protocol.is_plain_proxy() {
                node.tls = original.tls.clone();
            }
        }
        Ok(node)
    }

    pub fn into_node(&self, core: crate::model::CoreKind) -> crate::error::Result<Node> {
        let server = self.server.trim();
        if server.is_empty() {
            return Err(crate::error::Error::other("服务器地址不能为空"));
        }
        if server.contains("://") || server.contains('/') || server.chars().any(char::is_whitespace)
        {
            return Err(crate::error::Error::other(
                "服务器地址只填主机名或 IP，不要带协议头、路径或空格",
            ));
        }
        let server = server
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(server)
            .to_string();
        if self.port == 0 {
            return Err(crate::error::Error::other("端口必须是 1-65535"));
        }
        let protocol = self.protocol;
        if !protocol.supported_by(core) {
            return Err(crate::error::Error::other(format!(
                "{} 不支持 {}，请切换到 sing-box 后再添加",
                core.display_name(),
                protocol.display_name()
            )));
        }

        let mut node = Node::new(
            if self.name.trim().is_empty() {
                format!("{}:{}", server, self.port)
            } else {
                self.name.trim().to_string()
            },
            protocol,
            server,
            self.port,
        );
        node.source = NodeSource::Manual;
        node.udp = self.udp;
        node.tags = split_list(&self.tags);

        // --- credentials ------------------------------------------------
        let mut credentials = Credentials::default();
        match protocol {
            Protocol::Vmess | Protocol::Vless => {
                credentials.uuid = Some(self.require_uuid()?);
                if protocol == Protocol::Vless {
                    let flow = self.flow.trim();
                    if !flow.is_empty() {
                        if !matches!(flow, "xtls-rprx-vision" | "xtls-rprx-vision-udp443") {
                            return Err(crate::error::Error::other(
                                "VLESS flow 只支持 xtls-rprx-vision，留空表示不使用",
                            ));
                        }
                        credentials.flow = Some(flow.to_string());
                    }
                }
            }
            Protocol::Trojan | Protocol::Hysteria2 | Protocol::Anytls => {
                credentials.password = Some(self.require_password()?);
            }
            Protocol::Shadowsocks => {
                let method = self.method.trim();
                if method.is_empty() {
                    return Err(crate::error::Error::other(
                        "Shadowsocks 需要填写加密方式，例如 aes-256-gcm",
                    ));
                }
                credentials.method = Some(method.to_string());
                credentials.password = Some(self.require_password()?);
            }
            Protocol::Tuic => {
                credentials.uuid = Some(self.require_uuid()?);
                credentials.password = Some(self.require_password()?);
            }
            Protocol::Socks | Protocol::Http => {
                let username = self.username.trim();
                let password = self.password.trim();
                if username.is_empty() != password.is_empty() {
                    return Err(crate::error::Error::other(
                        "用户名与密码需要同时填写，或同时留空",
                    ));
                }
                if !username.is_empty() {
                    credentials.username = Some(username.to_string());
                    credentials.password = Some(password.to_string());
                }
            }
            Protocol::Openvpn | Protocol::Openconnect => {
                credentials.username = none_if_empty(self.username.trim());
                credentials.password = none_if_empty(&self.password);
                if protocol == Protocol::Openvpn {
                    self.openvpn.validate()?;
                    if self.insecure {
                        return Err(crate::Error::config(
                            "OpenVPN 不支持跳过证书验证，请填写 CA 或服务器证书指纹",
                        ));
                    }
                    credentials.openvpn = Some(self.openvpn.clone());
                } else {
                    self.openconnect.validate()?;
                    credentials.openconnect = Some(self.openconnect.clone());
                }
            }
            Protocol::Wireguard => {
                let reserved = split_list(&self.wireguard_reserved)
                    .iter()
                    .map(|v| v.parse::<u8>())
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| {
                        crate::Error::config("WireGuard reserved 必须是三个 0–255 的整数")
                    })?;
                let wg = WireguardOptions {
                    private_key: self.wireguard_private_key.trim().into(),
                    public_key: self.wireguard_public_key.trim().into(),
                    pre_shared_key: self.wireguard_pre_shared_key.trim().into(),
                    addresses: split_list(&self.wireguard_addresses),
                    allowed_ips: split_list(&self.wireguard_allowed_ips),
                    mtu: self
                        .wireguard_mtu
                        .trim()
                        .parse()
                        .map_err(|_| crate::Error::config("WireGuard MTU 必须是整数"))?,
                    reserved: reserved
                        .try_into()
                        .map_err(|_| crate::Error::config("WireGuard reserved 必须包含三个整数"))?,
                };
                wg.validate()?;
                credentials.wireguard = Some(wg);
            }
        }
        node.credentials = credentials;

        // --- TLS / Reality ----------------------------------------------
        let reality_key = self.reality_public_key.trim();
        let reality_short = self.reality_short_id.trim();
        let mut tls = TlsOptions::default();
        if !protocol.is_plain_proxy() && protocol != Protocol::Wireguard {
            // Hysteria2/TUIC/AnyTLS require TLS; Reality also implies TLS.
            tls.enabled = self.tls_enabled
                || matches!(
                    protocol,
                    Protocol::Hysteria2
                        | Protocol::Tuic
                        | Protocol::Anytls
                        | Protocol::Openvpn
                        | Protocol::Openconnect
                )
                || !reality_key.is_empty();
            tls.server_name = none_if_empty(self.sni.trim());
            tls.insecure = self.insecure;
            tls.alpn = split_list(&self.alpn);
            tls.fingerprint = none_if_empty(self.fingerprint.trim());
            if !reality_key.is_empty() || !reality_short.is_empty() {
                if reality_key.is_empty() {
                    return Err(crate::error::Error::other(
                        "Reality 需要填写公钥（public key），可从分享链接的 pbk 参数获取",
                    ));
                }
                tls.reality = Some(RealityOptions {
                    public_key: reality_key.to_string(),
                    short_id: reality_short.to_string(),
                });
                tls.enabled = true;
            }
        }
        node.tls = tls;

        // --- transport ---------------------------------------------------
        let path = normalize_path(&self.transport_path);
        let host = none_if_empty(self.transport_host.trim());
        let service_name = self.transport_service_name.trim().to_string();
        let transport = match self.transport {
            TransportKind::Tcp => Transport::Tcp,
            TransportKind::WebSocket => Transport::WebSocket {
                path,
                host,
                headers: BTreeMap::new(),
            },
            TransportKind::Grpc => {
                if service_name.is_empty() {
                    return Err(crate::error::Error::other(
                        "gRPC 传输需要填写 serviceName（可与服务端任意匹配，常见为 grpc）",
                    ));
                }
                Transport::Grpc { service_name }
            }
            TransportKind::Http2 => Transport::Http2 { path, host },
            TransportKind::HttpUpgrade => Transport::HttpUpgrade { path, host },
            TransportKind::Quic => Transport::Quic,
            TransportKind::Kcp => Transport::Kcp { seed: None },
        };
        if matches!(
            protocol,
            Protocol::Hysteria2
                | Protocol::Tuic
                | Protocol::Anytls
                | Protocol::Wireguard
                | Protocol::Openvpn
                | Protocol::Openconnect
        ) && !matches!(transport, Transport::Tcp)
        {
            return Err(crate::error::Error::other(
                "Hysteria2 / TUIC / AnyTLS / WireGuard / OpenVPN / OpenConnect 使用自有传输层，不能叠加 WebSocket / gRPC 等传输",
            ));
        }
        node.transport = transport;

        if protocol.is_vpn() {
            if node.tls.reality.is_some()
                || node.tls.fingerprint.is_some()
                || !node.tls.alpn.is_empty()
            {
                return Err(crate::Error::config(
                    "OpenVPN / OpenConnect 不支持通用 Reality、uTLS 或 ALPN 选项，请使用 VPN 参数",
                ));
            }
            let certificate = node
                .credentials
                .openvpn
                .as_ref()
                .map(|o| &o.certificates)
                .or_else(|| {
                    node.credentials
                        .openconnect
                        .as_ref()
                        .map(|o| &o.certificates)
                })
                .unwrap();
            let password_auth = node
                .credentials
                .username
                .as_ref()
                .is_some_and(|s| !s.is_empty())
                && node
                    .credentials
                    .password
                    .as_ref()
                    .is_some_and(|s| !s.is_empty());
            let session_auth = node
                .credentials
                .openconnect
                .as_ref()
                .is_some_and(|o| !o.cookie.is_empty());
            if !password_auth && certificate.certificate_pem.is_empty() && !session_auth {
                return Err(crate::Error::config(
                    "请填写 VPN 账号密码、客户端证书或已认证会话",
                ));
            }
        }

        Ok(node)
    }

    fn require_uuid(&self) -> crate::error::Result<String> {
        let raw = self.uuid.trim();
        if raw.is_empty() {
            return Err(crate::error::Error::other(
                "该协议需要 UUID / 用户 ID，请填写服务端提供的 id",
            ));
        }
        if uuid::Uuid::parse_str(raw).is_err() {
            return Err(crate::error::Error::other(
                "UUID 格式无效，应为 8-4-4-4-12 的十六进制（例如 11111111-2222-3333-4444-555555555555）",
            ));
        }
        Ok(raw.to_string())
    }

    fn require_password(&self) -> crate::error::Result<String> {
        let password = self.password.trim();
        if password.is_empty() {
            return Err(crate::error::Error::other("该协议需要填写密码"));
        }
        Ok(password.to_string())
    }
}

fn none_if_empty(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.trim().to_string())
    }
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split([',', ' ', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_path(value: &str) -> String {
    let path = value.trim();
    if path.is_empty() {
        "/".into()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
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

#[cfg(test)]
mod tests {
    #[test]
    fn wireguard_rejects_invalid_keys_addresses_and_reserved_bytes() {
        let mut draft = NodeDraft {
            protocol: Protocol::Wireguard,
            server: "example.com".into(),
            port: 51820,
            wireguard_private_key: crate::util::encode_base64(&[1; 32]),
            wireguard_public_key: crate::util::encode_base64(&[2; 32]),
            wireguard_addresses: "10.0.0.2/32".into(),
            ..Default::default()
        };
        assert!(draft.into_node(CoreKind::SingBox).is_ok());
        draft.wireguard_reserved = "1,2,256".into();
        assert!(draft.into_node(CoreKind::SingBox).is_err());
        draft.wireguard_reserved = "1,2,3".into();
        draft.wireguard_addresses = "invalid".into();
        assert!(draft.into_node(CoreKind::SingBox).is_err());
        draft.wireguard_addresses = "10.0.0.2/32".into();
        draft.wireguard_private_key = "secret-invalid-key".into();
        let error = draft.into_node(CoreKind::SingBox).unwrap_err().to_string();
        assert!(!error.contains("secret-invalid-key"));
    }

    #[test]
    fn editing_roundtrips_imported_parameters_and_all_transports() {
        let transports = [
            Transport::Tcp,
            Transport::WebSocket {
                path: "/custom".into(),
                host: Some("cdn.example.com".into()),
                headers: BTreeMap::from([("X-Test".into(), "fixture".into())]),
            },
            Transport::Grpc {
                service_name: "proxy".into(),
            },
            Transport::Http2 {
                path: "/h2".into(),
                host: Some("h2.example.com".into()),
            },
            Transport::HttpUpgrade {
                path: "/upgrade".into(),
                host: None,
            },
            Transport::Quic,
            Transport::Kcp {
                seed: Some("fixture-seed".into()),
            },
        ];
        for transport in transports {
            let mut node = vless_draft().into_node(CoreKind::SingBox).unwrap();
            node.transport = transport;
            node.credentials.alter_id = 64;
            node.credentials.plugin = Some("fixture-plugin".into());
            node.tls.server_name = Some("tls.example.com".into());
            node.tls.alpn = vec!["h2".into(), "http/1.1".into()];
            node.tls.fingerprint = Some("chrome".into());
            node.tls.reality = Some(RealityOptions {
                public_key: "fixture-key".into(),
                short_id: "abcd".into(),
            });
            node.tags = vec!["tag with spaces".into(), "other".into()];
            node.source = NodeSource::Subscription;
            node.subscription_id = Some("fixture-subscription".into());
            node.enabled = false;
            node.udp = false;
            node.sort = 42;
            assert_eq!(
                NodeDraft::from(&node)
                    .apply_to(&node, CoreKind::SingBox)
                    .unwrap(),
                node
            );
        }
    }

    #[test]
    fn editing_https_proxy_preserves_tls_and_protocol_changes_clear_old_credentials() {
        let mut node = Node::new("https", Protocol::Http, "proxy.example.com", 443);
        node.tls.enabled = true;
        node.tls.server_name = Some("tls.example.com".into());
        assert_eq!(
            NodeDraft::from(&node)
                .apply_to(&node, CoreKind::SingBox)
                .unwrap(),
            node
        );
        let mut draft = NodeDraft::from(&node);
        draft.protocol = Protocol::Trojan;
        draft.password = "fixture-password".into();
        node.credentials.plugin = Some("old-plugin".into());
        let edited = draft.apply_to(&node, CoreKind::SingBox).unwrap();
        assert_eq!(edited.id, node.id);
        assert_eq!(
            edited.credentials.password.as_deref(),
            Some("fixture-password")
        );
        assert!(edited.credentials.plugin.is_none());
    }

    #[test]
    fn anytls_manual_requires_password_and_tls() {
        let mut draft = NodeDraft {
            protocol: Protocol::Anytls,
            server: "example.com".into(),
            port: 443,
            ..Default::default()
        };
        assert!(draft.into_node(CoreKind::SingBox).is_err());
        draft.password = "fixture-password".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert!(node.tls.enabled);
        assert_eq!(
            node.credentials.password.as_deref(),
            Some("fixture-password")
        );
        assert!(
            draft
                .into_node(CoreKind::Xray)
                .unwrap_err()
                .to_string()
                .contains("sing-box")
        );
        draft.transport = TransportKind::WebSocket;
        assert!(draft.into_node(CoreKind::SingBox).is_err());
    }

    use super::*;
    use crate::model::CoreKind;

    fn vless_draft() -> NodeDraft {
        NodeDraft {
            name: "东京 VLESS".into(),
            protocol: Protocol::Vless,
            server: "jp.example.com".into(),
            port: 443,
            uuid: "11111111-2222-3333-4444-555555555555".into(),
            tls_enabled: true,
            sni: "jp.example.com".into(),
            transport: TransportKind::WebSocket,
            transport_path: "ws".into(),
            transport_host: "cdn.example.com".into(),
            alpn: "h2, http/1.1".into(),
            tags: "日本, 手动".into(),
            ..Default::default()
        }
    }

    #[test]
    fn manual_vless_draft_builds_node() {
        let node = vless_draft().into_node(CoreKind::SingBox).unwrap();
        assert_eq!(node.protocol, Protocol::Vless);
        assert_eq!(node.server, "jp.example.com");
        assert_eq!(node.port, 443);
        assert_eq!(node.source, NodeSource::Manual);
        assert_eq!(
            node.credentials.uuid.as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert!(node.tls.enabled);
        assert_eq!(node.tls.alpn, vec!["h2", "http/1.1"]);
        assert_eq!(node.tags, vec!["日本", "手动"]);
        match node.transport {
            Transport::WebSocket { path, host, .. } => {
                assert_eq!(path, "/ws", "missing slash is added");
                assert_eq!(host.as_deref(), Some("cdn.example.com"));
            }
            other => panic!("unexpected transport: {other:?}"),
        }
    }

    #[test]
    fn manual_draft_falls_back_to_endpoint_name() {
        let mut draft = vless_draft();
        draft.name = "   ".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert_eq!(node.name, "jp.example.com:443");
    }

    #[test]
    fn manual_draft_rejects_bad_endpoint_and_uuid() {
        let mut draft = vless_draft();
        draft.server = "https://jp.example.com".into();
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("不要带协议头")
        );

        let mut draft = vless_draft();
        draft.uuid = "not-a-uuid".into();
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("UUID 格式无效")
        );

        let mut draft = vless_draft();
        draft.uuid = String::new();
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("需要 UUID")
        );

        let mut draft = vless_draft();
        draft.port = 0;
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("端口")
        );
    }

    #[test]
    fn manual_draft_respects_core_capabilities() {
        let draft = NodeDraft {
            protocol: Protocol::Hysteria2,
            server: "de.example.com".into(),
            port: 8443,
            password: "secret".into(),
            ..Default::default()
        };
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert!(node.tls.enabled, "Hysteria2 is always TLS");
        let err = draft.into_node(CoreKind::Xray).unwrap_err().to_string();
        assert!(err.contains("sing-box"), "actionable hint: {err}");
    }

    #[test]
    fn manual_draft_validates_reality_and_transport_combinations() {
        let mut draft = vless_draft();
        draft.reality_short_id = "abcd1234".into();
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("public key")
        );

        let mut draft = vless_draft();
        draft.tls_enabled = false;
        draft.reality_public_key = "ml58sJg9puxNs502V-apSbKQGLslmU4T3W6btDPizWI".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert!(node.tls.enabled, "Reality implies TLS");
        assert!(node.tls.reality.is_some());

        let mut draft = vless_draft();
        draft.protocol = Protocol::Hysteria2;
        draft.password = "secret".into();
        draft.transport = TransportKind::WebSocket;
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("自有传输层")
        );
    }

    #[test]
    fn manual_draft_shadowsocks_and_plain_proxy_rules() {
        let mut draft = NodeDraft {
            protocol: Protocol::Shadowsocks,
            server: "ss.example.com".into(),
            port: 8388,
            password: "secret".into(),
            tls_enabled: true,
            ..Default::default()
        };
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("加密方式")
        );
        draft.method = "aes-256-gcm".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert!(!node.tls.enabled, "plain proxies drop TLS fields");
        assert_eq!(node.credentials.method.as_deref(), Some("aes-256-gcm"));

        let mut draft = NodeDraft {
            protocol: Protocol::Socks,
            server: "socks.example.com".into(),
            port: 1080,
            username: "user".into(),
            ..Default::default()
        };
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("同时填写")
        );
        draft.password = "pass".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert_eq!(node.credentials.username.as_deref(), Some("user"));
    }
}
