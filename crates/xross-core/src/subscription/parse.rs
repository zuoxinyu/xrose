//! Subscription format detection and parsing: share links, base64 blobs,
//! Clash YAML, SIP008 and sing-box JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::model::{GroupKind, Node, Protocol, RealityOptions, TlsOptions, Transport};
use crate::util::{decode_base64_any, looks_base64, truthy};

use super::links::parse_share_link;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    ShareLinks,
    Base64,
    ClashYaml,
    Sip008,
    SingBoxJson,
    #[default]
    Unknown,
}

impl SourceFormat {
    pub fn label(&self) -> &'static str {
        match self {
            SourceFormat::ShareLinks => "分享链接",
            SourceFormat::Base64 => "Base64 订阅",
            SourceFormat::ClashYaml => "Clash YAML",
            SourceFormat::Sip008 => "SIP008",
            SourceFormat::SingBoxJson => "sing-box JSON",
            SourceFormat::Unknown => "未知格式",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedGroup {
    pub name: String,
    pub kind: GroupKind,
    /// Member names as written in the subscription.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedSubscription {
    pub format: SourceFormat,
    pub nodes: Vec<Node>,
    pub groups: Vec<ParsedGroup>,
    pub warnings: Vec<String>,
}

impl ParsedSubscription {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Full parse, including outer base64 decoding and format detection.
pub fn parse_subscription(content: &str) -> ParsedSubscription {
    let content = content.trim_start_matches('\u{feff}').trim();
    if content.is_empty() {
        return ParsedSubscription {
            warnings: vec!["订阅内容为空".into()],
            ..Default::default()
        };
    }

    if looks_base64(content) && !content.contains("://") {
        if let Some(decoded) = decode_base64_any(content) {
            if let Ok(text) = String::from_utf8(decoded) {
                let mut parsed = parse_subscription(&text);
                if !parsed.is_empty() {
                    if parsed.format == SourceFormat::ShareLinks {
                        parsed.format = SourceFormat::Base64;
                    }
                    return parsed;
                }
            }
        }
    }

    let lower = content.to_ascii_lowercase();
    if content.starts_with('{') || content.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
            let mut parsed = if value.get("outbounds").is_some() {
                parse_singbox_json(&value)
            } else if value.get("servers").is_some() {
                parse_sip008(&value)
            } else {
                ParsedSubscription {
                    format: SourceFormat::Unknown,
                    warnings: vec!["无法识别的 JSON 订阅结构".into()],
                    ..Default::default()
                }
            };
            if lower.contains("\"proxies\"") {
                // Clash's JSON-ish superset: its YAML parser handles JSON too.
                let mut yaml_parsed = parse_clash_yaml(content);
                if !yaml_parsed.nodes.is_empty() {
                    yaml_parsed.warnings.extend(parsed.warnings);
                    parsed = yaml_parsed;
                }
            }
            return parsed;
        }
    }
    if lower.contains("proxies:") || lower.contains("proxy-groups:") {
        let parsed = parse_clash_yaml(content);
        if !parsed.nodes.is_empty() {
            return parsed;
        }
    }
    parse_share_links(content)
}

pub fn parse_share_links(content: &str) -> ParsedSubscription {
    let mut parsed = ParsedSubscription {
        format: SourceFormat::ShareLinks,
        ..Default::default()
    };
    let mut unparsed = 0usize;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        // Some providers prefix nodes with metadata lines.
        if line.starts_with("STATUS=") || line.starts_with("REMARKS=") {
            continue;
        }
        match parse_share_link(line) {
            Some(node) => parsed.nodes.push(node),
            None => unparsed += 1,
        }
    }
    if parsed.nodes.is_empty() {
        parsed.format = SourceFormat::Unknown;
        parsed.warnings.push("订阅中没有可识别的分享链接".into());
    } else if unparsed > 0 {
        parsed
            .warnings
            .push(format!("{unparsed} 行无法解析，已跳过"));
    }
    parsed
}

pub fn parse_clash_yaml(content: &str) -> ParsedSubscription {
    let mut parsed = ParsedSubscription {
        format: SourceFormat::ClashYaml,
        ..Default::default()
    };
    let root: Value = match serde_yaml::from_str(content) {
        Ok(value) => value,
        Err(err) => {
            parsed.warnings.push(format!("YAML 解析失败：{err}"));
            return parsed;
        }
    };
    let Some(proxies) = root.get("proxies").and_then(|v| v.as_sequence()) else {
        parsed.warnings.push("YAML 中没有 proxies 列表".into());
        return parsed;
    };
    let mut name_to_id: BTreeMap<String, String> = BTreeMap::new();
    for entry in proxies {
        match clone_to_node(entry) {
            Ok(node) => {
                name_to_id.insert(node.name.clone(), node.id.clone());
                parsed.nodes.push(node);
            }
            Err(reason) => parsed.warnings.push(reason),
        }
    }
    if let Some(groups) = root.get("proxy-groups").and_then(|v| v.as_sequence()) {
        for group in groups {
            let name = yaml_str(group, "name").unwrap_or_default();
            let kind = yaml_str(group, "type")
                .and_then(|t| GroupKind::parse(&t))
                .unwrap_or(GroupKind::Selector);
            let mut members = Vec::new();
            if let Some(list) = group.get("proxies").and_then(|v| v.as_sequence()) {
                for member in list {
                    if let Some(name) = member.as_str() {
                        if name == "DIRECT" || name == "REJECT" || name == "PASS" {
                            continue;
                        }
                        if name_to_id.contains_key(name) {
                            members.push(name.to_string());
                        }
                    }
                }
            }
            if !name.is_empty() && !members.is_empty() {
                parsed.groups.push(ParsedGroup {
                    name,
                    kind,
                    members,
                });
            }
        }
    }
    parsed
}

fn clone_to_node(value: &Value) -> std::result::Result<Node, String> {
    let name = yaml_str(value, "name").unwrap_or_else(|| "未命名节点".into());
    let proxy_type = yaml_str(value, "type").unwrap_or_default();
    let protocol = Protocol::parse(&proxy_type)
        .ok_or_else(|| format!("节点“{name}”类型 {proxy_type} 不支持"))?;
    let server = yaml_str(value, "server").ok_or_else(|| format!("节点“{name}”缺少 server"))?;
    let port = yaml_u16(value, "port").ok_or_else(|| format!("节点“{name}”缺少 port"))?;
    let mut node = Node::new(name.clone(), protocol, server, port);
    node.source = crate::model::NodeSource::Subscription;

    let tls_enabled = yaml_bool(value, "tls").unwrap_or(false)
        || matches!(
            proxy_type.as_str(),
            "trojan" | "hysteria2" | "tuic" | "anytls"
        );
    node.tls = TlsOptions {
        enabled: tls_enabled,
        server_name: yaml_str(value, "servername")
            .or_else(|| yaml_str(value, "sni"))
            .or_else(|| yaml_str(value, "server-name")),
        insecure: yaml_bool(value, "skip-cert-verify").unwrap_or(false),
        alpn: yaml_string_list(value, "alpn"),
        fingerprint: yaml_str(value, "client-fingerprint"),
        reality: value.get("reality-opts").and_then(|_| {
            yaml_str(value.get("reality-opts").unwrap(), "public-key").map(|public_key| {
                RealityOptions {
                    public_key,
                    short_id: yaml_str(value.get("reality-opts").unwrap(), "short-id")
                        .unwrap_or_default(),
                }
            })
        }),
    };

    match protocol {
        Protocol::Vmess => {
            node.credentials.uuid = yaml_str(value, "uuid");
            node.credentials.alter_id = yaml_u16(value, "alterId").unwrap_or(0) as u32;
        }
        Protocol::Vless => {
            node.credentials.uuid = yaml_str(value, "uuid");
            node.credentials.flow = yaml_str(value, "flow");
        }
        Protocol::Trojan | Protocol::Anytls => {
            node.credentials.password = yaml_str(value, "password")
        }
        Protocol::Shadowsocks => {
            node.credentials.method = yaml_str(value, "cipher");
            node.credentials.password = yaml_str(value, "password");
            node.credentials.plugin = yaml_str(value, "plugin");
        }
        Protocol::Hysteria2 => {
            node.credentials.password =
                yaml_str(value, "password").or_else(|| yaml_str(value, "auth"));
            node.credentials.obfs = yaml_str(value, "obfs");
            node.credentials.obfs_password = yaml_str(value, "obfs-password");
        }
        Protocol::Tuic => {
            node.credentials.uuid = yaml_str(value, "uuid");
            node.credentials.password = yaml_str(value, "password");
            node.credentials.congestion_control = yaml_str(value, "congestion-controller");
            node.credentials.udp_relay_mode = yaml_str(value, "udp-relay-mode");
        }
        Protocol::Socks | Protocol::Http => {
            node.credentials.username = yaml_str(value, "username");
            node.credentials.password = yaml_str(value, "password");
        }
        Protocol::Wireguard => {
            return Err(format!("节点“{name}”为 WireGuard，暂不支持"));
        }
    }

    let network = yaml_str(value, "network")
        .unwrap_or_else(|| "tcp".into())
        .to_ascii_lowercase();
    let ws = value.get("ws-opts");
    let grpc = value.get("grpc-opts");
    node.transport = match network.as_str() {
        "ws" => {
            let path = ws
                .and_then(|w| yaml_str(w, "path"))
                .unwrap_or_else(|| "/".into());
            let mut headers = BTreeMap::new();
            let host = ws
                .and_then(|w| w.get("headers"))
                .and_then(|h| yaml_str(h, "Host").or_else(|| yaml_str(h, "host")));
            if let Some(h) = &host {
                headers.insert("Host".into(), h.clone());
            }
            Transport::WebSocket {
                path,
                host,
                headers,
            }
        }
        "grpc" => Transport::Grpc {
            service_name: grpc
                .and_then(|g| yaml_str(g, "grpc-service-name"))
                .unwrap_or_default(),
        },
        "h2" | "http" => Transport::Http2 {
            path: value
                .get("h2-opts")
                .and_then(|o| yaml_str(o, "path"))
                .unwrap_or_else(|| "/".into()),
            host: value.get("h2-opts").and_then(|o| yaml_str(o, "host")),
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: value
                .get("http-opts")
                .and_then(|o| yaml_str(o, "path"))
                .unwrap_or_else(|| "/".into()),
            host: value.get("http-opts").and_then(|o| yaml_str(o, "host")),
        },
        "quic" => Transport::Quic,
        _ => Transport::Tcp,
    };
    Ok(node)
}

pub fn parse_sip008(value: &serde_json::Value) -> ParsedSubscription {
    let mut parsed = ParsedSubscription {
        format: SourceFormat::Sip008,
        ..Default::default()
    };
    let Some(servers) = value.get("servers").and_then(|v| v.as_array()) else {
        parsed.warnings.push("SIP008 缺少 servers 数组".into());
        return parsed;
    };
    for server in servers {
        let name = server
            .get("remarks")
            .and_then(|v| v.as_str())
            .or_else(|| server.get("id").and_then(|v| v.as_str()))
            .unwrap_or("SIP008");
        let Some(host) = server.get("server").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(port) = server.get("server_port").and_then(|v| v.as_u64()) else {
            continue;
        };
        if port > u16::MAX as u64 {
            continue;
        }
        let mut node = Node::new(name, Protocol::Shadowsocks, host, port as u16);
        node.credentials.method = server
            .get("method")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        node.credentials.password = server
            .get("password")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        node.credentials.plugin = server
            .get("plugin")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        node.credentials.plugin_opts = server
            .get("plugin_opts")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        node.source = crate::model::NodeSource::Subscription;
        parsed.nodes.push(node);
    }
    if parsed.nodes.is_empty() {
        parsed.warnings.push("SIP008 中没有有效节点".into());
    }
    parsed
}

pub fn parse_singbox_json(value: &serde_json::Value) -> ParsedSubscription {
    let mut parsed = ParsedSubscription {
        format: SourceFormat::SingBoxJson,
        ..Default::default()
    };
    let Some(outbounds) = value.get("outbounds").and_then(|v| v.as_array()) else {
        parsed
            .warnings
            .push("sing-box JSON 缺少 outbounds 数组".into());
        return parsed;
    };
    for outbound in outbounds {
        let Some(outbound_type) = outbound.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if matches!(
            outbound_type,
            "selector" | "urltest" | "direct" | "block" | "dns"
        ) {
            continue;
        }
        let Some(protocol) = Protocol::parse(outbound_type) else {
            parsed
                .warnings
                .push(format!("出站类型 {outbound_type} 不支持"));
            continue;
        };
        if protocol == Protocol::Wireguard {
            parsed.warnings.push("跳过 WireGuard 出站".into());
            continue;
        }
        let Some(host) = outbound.get("server").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(port) = outbound.get("server_port").and_then(|v| v.as_u64()) else {
            continue;
        };
        if port > u16::MAX as u64 {
            continue;
        }
        let name = outbound
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or(outbound_type);
        let mut node = Node::new(name, protocol, host, port as u16);
        node.source = crate::model::NodeSource::Subscription;
        let string = |key: &str| {
            outbound
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        match protocol {
            Protocol::Vmess | Protocol::Vless => {
                node.credentials.uuid = string("uuid");
                node.credentials.flow = string("flow");
                node.credentials.alter_id = outbound
                    .get("alter_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }
            Protocol::Trojan | Protocol::Hysteria2 | Protocol::Anytls => {
                node.credentials.password = string("password");
                node.credentials.obfs = outbound
                    .get("obfs")
                    .and_then(|o| o.get("type"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                node.credentials.obfs_password = outbound
                    .get("obfs")
                    .and_then(|o| o.get("password"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            Protocol::Shadowsocks => {
                node.credentials.method = string("method");
                node.credentials.password = string("password");
                node.credentials.plugin = string("plugin");
                node.credentials.plugin_opts = string("plugin_opts");
            }
            Protocol::Tuic => {
                node.credentials.uuid = string("uuid");
                node.credentials.password = string("password");
                node.credentials.congestion_control = string("congestion_control");
                node.credentials.udp_relay_mode = string("udp_relay_mode");
            }
            Protocol::Socks | Protocol::Http => {
                node.credentials.username = string("username");
                node.credentials.password = string("password");
            }
            Protocol::Wireguard => {}
        }
        if let Some(tls) = outbound.get("tls") {
            node.tls.enabled = tls.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            node.tls.server_name = tls
                .get("server_name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            node.tls.insecure = tls
                .get("insecure")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            node.tls.alpn = tls
                .get("alpn")
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(utls) = tls.get("utls") {
                node.tls.fingerprint = utls
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
            if let Some(reality) = tls.get("reality") {
                node.tls.reality =
                    reality
                        .get("public_key")
                        .and_then(|v| v.as_str())
                        .map(|public_key| RealityOptions {
                            public_key: public_key.to_string(),
                            short_id: reality
                                .get("short_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        });
            }
        }
        if let Some(transport) = outbound.get("transport") {
            let transport_type = transport
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("tcp");
            node.transport = match transport_type {
                "ws" => {
                    let mut headers = BTreeMap::new();
                    if let Some(map) = transport.get("headers").and_then(|v| v.as_object()) {
                        for (k, v) in map {
                            if let Some(v) = v.as_str() {
                                headers.insert(k.clone(), v.to_string());
                            }
                        }
                    }
                    Transport::WebSocket {
                        path: transport
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("/")
                            .into(),
                        host: headers.get("Host").cloned(),
                        headers,
                    }
                }
                "grpc" => Transport::Grpc {
                    service_name: transport
                        .get("service_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                },
                "http" | "h2" => Transport::Http2 {
                    path: transport
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .into(),
                    host: transport
                        .get("host")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
                "httpupgrade" => Transport::HttpUpgrade {
                    path: transport
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .into(),
                    host: transport
                        .get("host")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                },
                "quic" => Transport::Quic,
                _ => Transport::Tcp,
            };
        }
        parsed.nodes.push(node);
    }
    if parsed.nodes.is_empty() {
        parsed
            .warnings
            .push("sing-box JSON 中没有可导入的节点".into());
    }
    parsed
}

fn yaml_str(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn yaml_u16(value: &Value, key: &str) -> Option<u16> {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_u64().and_then(|v| u16::try_from(v).ok()),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}

fn yaml_bool(value: &Value, key: &str) -> Option<bool> {
    match value.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => Some(truthy(s)),
        Some(Value::Number(n)) => Some(n.as_u64().unwrap_or(0) > 0),
        _ => None,
    }
}

fn yaml_string_list(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::Sequence(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn imports_anytls_links_clash_and_singbox() {
        let sources = [
            "anytls://fake@example.com:443?sni=tls.example.com&insecure=1#test".to_string(),
            crate::util::encode_base64(b"anytls://fake@example.com:443?sni=tls.example.com&insecure=1#test"),
            "proxies:\n  - {name: test, type: anytls, server: example.com, port: 443, password: fake, sni: tls.example.com, skip-cert-verify: true}".to_string(),
            r#"{"outbounds":[{"type":"anytls","tag":"test","server":"example.com","server_port":443,"password":"fake","tls":{"enabled":true,"server_name":"tls.example.com","insecure":true}}]}"#.to_string(),
        ];
        for source in sources {
            let parsed = parse_subscription(&source);
            assert_eq!(parsed.nodes.len(), 1);
            let node = &parsed.nodes[0];
            assert_eq!(node.protocol, Protocol::Anytls);
            assert_eq!(node.credentials.password.as_deref(), Some("fake"));
            assert!(node.tls.enabled);
            assert!(node.tls.insecure);
            assert_eq!(node.tls.server_name.as_deref(), Some("tls.example.com"));
        }
    }

    use super::*;

    #[test]
    fn parses_base64_links() {
        let links = "vmess://eyJ2IjoiMiIsInBzIjoiQSIsImFkZCI6ImV4YW1wbGUuY29tIiwicG9ydCI6IjQ0MyIsImlkIjoiMTExMTExMTEtMjIyMi0zMzMzLTQ0NDQtNTU1NTU1NTU1NTU1IiwibmV0IjoicmF3IiwidGxzIjoidGxzIn0=\nvless://11111111-2222-3333-4444-555555555555@b.example.com:443?security=tls&sni=b.example.com#B";
        let encoded = crate::util::encode_base64(links.as_bytes());
        let parsed = parse_subscription(&encoded);
        assert_eq!(parsed.format, SourceFormat::Base64);
        assert_eq!(parsed.nodes.len(), 2);
    }

    #[test]
    fn parses_clash_yaml() {
        let yaml = r#"
proxies:
  - name: "香港 01"
    type: vmess
    server: hk.example.com
    port: 443
    uuid: 11111111-2222-3333-4444-555555555555
    alterId: 0
    cipher: auto
    tls: true
    servername: hk.example.com
    network: ws
    ws-opts:
      path: /ws
      headers:
        Host: hk.example.com
  - name: "日本 ss"
    type: ss
    server: jp.example.com
    port: 8388
    cipher: aes-256-gcm
    password: secret
proxy-groups:
  - name: 节点选择
    type: select
    proxies:
      - "香港 01"
      - "日本 ss"
"#;
        let parsed = parse_subscription(yaml);
        assert_eq!(parsed.format, SourceFormat::ClashYaml);
        assert_eq!(parsed.nodes.len(), 2);
        assert_eq!(parsed.groups.len(), 1);
        assert_eq!(parsed.groups[0].members.len(), 2);
        assert!(matches!(
            parsed.nodes[0].transport,
            Transport::WebSocket { .. }
        ));
    }

    #[test]
    fn parses_sip008() {
        let json = r#"{"version":1,"servers":[{"id":"1","remarks":"ss","server":"1.2.3.4","server_port":8388,"method":"chacha20-ietf-poly1305","password":"p"}]}"#;
        let parsed = parse_subscription(json);
        assert_eq!(parsed.format, SourceFormat::Sip008);
        assert_eq!(parsed.nodes.len(), 1);
    }

    #[test]
    fn parses_singbox_json() {
        let json = r#"{"outbounds":[{"type":"vless","tag":"v","server":"a.com","server_port":443,"uuid":"11111111-2222-3333-4444-555555555555","tls":{"enabled":true,"server_name":"a.com"}},{"type":"selector","tag":"s","outbounds":["v"]}]}"#;
        let parsed = parse_subscription(json);
        assert_eq!(parsed.format, SourceFormat::SingBoxJson);
        assert_eq!(parsed.nodes.len(), 1);
    }

    #[test]
    fn garbage_is_reported() {
        let parsed = parse_subscription("not a subscription");
        assert!(parsed.nodes.is_empty());
        assert!(!parsed.warnings.is_empty());
    }
}
