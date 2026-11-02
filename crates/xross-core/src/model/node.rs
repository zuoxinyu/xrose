use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Vmess,
    #[default]
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
    pub fn all() -> [Protocol; 9] {
        [
            Protocol::Vmess,
            Protocol::Vless,
            Protocol::Trojan,
            Protocol::Shadowsocks,
            Protocol::Hysteria2,
            Protocol::Tuic,
            Protocol::Socks,
            Protocol::Http,
            Protocol::Wireguard,
        ]
    }

    /// Protocols the manual "add node" form can produce. WireGuard has no key
    /// fields in the model yet, so offering it would only create a dead end.
    pub fn manual() -> [Protocol; 8] {
        [
            Protocol::Vmess,
            Protocol::Vless,
            Protocol::Trojan,
            Protocol::Shadowsocks,
            Protocol::Hysteria2,
            Protocol::Tuic,
            Protocol::Socks,
            Protocol::Http,
        ]
    }

    /// Whether the protocol carries credentials derived from a UUID.
    pub fn needs_uuid(&self) -> bool {
        matches!(
            self,
            Protocol::Vmess | Protocol::Vless | Protocol::Tuic | Protocol::Wireguard
        )
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
        }
    }
}

impl NodeDraft {
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
        if protocol == Protocol::Wireguard {
            return Err(crate::error::Error::other(
                "WireGuard 出站尚未支持（模型缺少密钥字段），请使用分享链接导入或等待后续版本",
            ));
        }
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
            Protocol::Trojan | Protocol::Hysteria2 => {
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
            Protocol::Wireguard => unreachable!("rejected above"),
        }
        node.credentials = credentials;

        // --- TLS / Reality ----------------------------------------------
        let reality_key = self.reality_public_key.trim();
        let reality_short = self.reality_short_id.trim();
        let mut tls = TlsOptions::default();
        if !protocol.is_plain_proxy() {
            // Hysteria2/TUIC are TLS-only; Reality implies TLS.
            tls.enabled = self.tls_enabled
                || matches!(protocol, Protocol::Hysteria2 | Protocol::Tuic)
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
        if matches!(protocol, Protocol::Hysteria2 | Protocol::Tuic)
            && !matches!(transport, Transport::Tcp)
        {
            return Err(crate::error::Error::other(
                "Hysteria2 / TUIC 使用自有传输层，不能叠加 WebSocket / gRPC 等传输",
            ));
        }
        node.transport = transport;

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
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("不要带协议头"));

        let mut draft = vless_draft();
        draft.uuid = "not-a-uuid".into();
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("UUID 格式无效"));

        let mut draft = vless_draft();
        draft.uuid = String::new();
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("需要 UUID"));

        let mut draft = vless_draft();
        draft.port = 0;
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("端口"));
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
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("public key"));

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
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("自有传输层"));
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
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("加密方式"));
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
        assert!(draft
            .into_node(CoreKind::SingBox)
            .unwrap_err()
            .to_string()
            .contains("同时填写"));
        draft.password = "pass".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert_eq!(node.credentials.username.as_deref(), Some("user"));
    }
}
