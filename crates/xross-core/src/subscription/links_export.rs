//! Node -> share link, for clipboard export and QR codes.

use crate::model::{Node, Protocol, Transport};
use crate::util::encode_base64;

use super::links::{format_host, percent_encode};

pub fn node_to_share_link(node: &Node) -> Option<String> {
    match node.protocol {
        Protocol::Vmess => export_vmess(node),
        Protocol::Vless => export_standard(node, "vless"),
        Protocol::Trojan => export_standard(node, "trojan"),
        Protocol::Shadowsocks => export_ss(node),
        Protocol::Hysteria2 => export_hysteria2(node),
        Protocol::Tuic => export_tuic(node),
        Protocol::Socks => export_plain(node, "socks"),
        Protocol::Http => export_plain(node, "http"),
        Protocol::Wireguard => None,
    }
}

fn export_vmess(node: &Node) -> Option<String> {
    let uuid = node.credentials.uuid.clone()?;
    let (net, kind, host, path) = match &node.transport {
        Transport::Tcp => ("tcp", "", "", ""),
        Transport::WebSocket { path, host, .. } => {
            ("ws", "", host.as_deref().unwrap_or(""), path.as_str())
        }
        Transport::Grpc { service_name } => ("grpc", "", "", service_name.as_str()),
        Transport::Http2 { path, host } => ("h2", "", host.as_deref().unwrap_or(""), path.as_str()),
        Transport::HttpUpgrade { path, host } => (
            "httpupgrade",
            "",
            host.as_deref().unwrap_or(""),
            path.as_str(),
        ),
        Transport::Quic => ("quic", "", "", ""),
        Transport::Kcp { seed } => ("kcp", seed.as_deref().unwrap_or("none"), "", ""),
    };
    let value = serde_json::json!({
        "v": "2",
        "ps": node.name,
        "add": node.server,
        "port": node.port.to_string(),
        "id": uuid,
        "aid": node.credentials.alter_id.to_string(),
        "scy": "auto",
        "net": net,
        "type": kind,
        "host": host,
        "path": path,
        "tls": if node.tls.enabled || node.tls.reality.is_some() { "tls" } else { "" },
        "sni": node.tls.server_name.clone().unwrap_or_default(),
        "alpn": node.tls.alpn.join(","),
        "fp": node.tls.fingerprint.clone().unwrap_or_default()
    });
    Some(format!(
        "vmess://{}",
        encode_base64(value.to_string().as_bytes())
    ))
}

fn export_standard(node: &Node, scheme: &str) -> Option<String> {
    let user = if scheme == "vless" {
        percent_encode(node.credentials.uuid.as_deref()?)
    } else {
        percent_encode(node.credentials.password.as_deref()?)
    };
    let mut params: Vec<(String, String)> = Vec::new();
    if scheme == "vless" {
        params.push(("encryption".into(), "none".into()));
    }
    let security = if node.tls.reality.is_some() {
        "reality"
    } else if node.tls.enabled {
        "tls"
    } else {
        "none"
    };
    params.push(("security".into(), security.into()));
    push_tls_params(node, &mut params);
    push_transport_params(node, &mut params);
    if let Some(flow) = &node.credentials.flow {
        if !flow.is_empty() {
            params.push(("flow".into(), flow.clone()));
        }
    }
    Some(format!(
        "{scheme}://{user}@{}:{}?{}#{}",
        format_host(&node.server),
        node.port,
        encode_query(&params),
        percent_encode(&node.name)
    ))
}

fn export_ss(node: &Node) -> Option<String> {
    let method = node.credentials.method.clone()?;
    let password = node.credentials.password.clone()?;
    let credentials = encode_base64(format!("{method}:{password}").as_bytes());
    let mut query = Vec::new();
    if let Some(plugin) = &node.credentials.plugin {
        let mut plugin_value = plugin.clone();
        if let Some(opts) = &node.credentials.plugin_opts {
            if !opts.is_empty() {
                plugin_value.push(';');
                plugin_value.push_str(opts);
            }
        }
        query.push(("plugin".into(), plugin_value));
    }
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", encode_query(&query))
    };
    Some(format!(
        "ss://{credentials}@{}:{}{suffix}#{}",
        format_host(&node.server),
        node.port,
        percent_encode(&node.name)
    ))
}

fn export_hysteria2(node: &Node) -> Option<String> {
    let password = node.credentials.password.clone()?;
    let mut params = Vec::new();
    if let Some(sni) = &node.tls.server_name {
        params.push(("sni".into(), sni.clone()));
    }
    if node.tls.insecure {
        params.push(("insecure".into(), "1".into()));
    }
    if let Some(obfs) = &node.credentials.obfs {
        params.push(("obfs".into(), obfs.clone()));
        if let Some(p) = &node.credentials.obfs_password {
            params.push(("obfs-password".into(), p.clone()));
        }
    }
    Some(format!(
        "hysteria2://{}@{}:{}?{}#{}",
        percent_encode(&password),
        format_host(&node.server),
        node.port,
        encode_query(&params),
        percent_encode(&node.name)
    ))
}

fn export_tuic(node: &Node) -> Option<String> {
    let uuid = node.credentials.uuid.clone()?;
    let password = node.credentials.password.clone().unwrap_or_default();
    let mut params = Vec::new();
    if let Some(sni) = &node.tls.server_name {
        params.push(("sni".into(), sni.clone()));
    }
    if node.tls.insecure {
        params.push(("allow_insecure".into(), "1".into()));
    }
    if let Some(cc) = &node.credentials.congestion_control {
        params.push(("congestion_control".into(), cc.clone()));
    }
    if let Some(mode) = &node.credentials.udp_relay_mode {
        params.push(("udp_relay_mode".into(), mode.clone()));
    }
    Some(format!(
        "tuic://{}:{}@{}:{}?{}#{}",
        percent_encode(&uuid),
        percent_encode(&password),
        format_host(&node.server),
        node.port,
        encode_query(&params),
        percent_encode(&node.name)
    ))
}

fn export_plain(node: &Node, scheme: &str) -> Option<String> {
    let mut result = format!("{scheme}://");
    if node.credentials.username.is_some() || node.credentials.password.is_some() {
        let user = node.credentials.username.clone().unwrap_or_default();
        let pass = node.credentials.password.clone().unwrap_or_default();
        result.push_str(&percent_encode(&format!("{user}:{pass}")));
        result.push('@');
    }
    result.push_str(&format_host(&node.server));
    result.push_str(&format!(":{}#{}", node.port, percent_encode(&node.name)));
    Some(result)
}

fn push_tls_params(node: &Node, params: &mut Vec<(String, String)>) {
    if let Some(sni) = &node.tls.server_name {
        params.push(("sni".into(), sni.clone()));
    }
    if node.tls.insecure {
        params.push(("allowInsecure".into(), "1".into()));
    }
    if !node.tls.alpn.is_empty() {
        params.push(("alpn".into(), node.tls.alpn.join(",")));
    }
    if let Some(fp) = &node.tls.fingerprint {
        params.push(("fp".into(), fp.clone()));
    }
    if let Some(reality) = &node.tls.reality {
        params.push(("pbk".into(), reality.public_key.clone()));
        if !reality.short_id.is_empty() {
            params.push(("sid".into(), reality.short_id.clone()));
        }
    }
}

fn push_transport_params(node: &Node, params: &mut Vec<(String, String)>) {
    match &node.transport {
        Transport::Tcp => params.push(("type".into(), "tcp".into())),
        Transport::WebSocket { path, host, .. } => {
            params.push(("type".into(), "ws".into()));
            params.push(("path".into(), path.clone()));
            if let Some(host) = host {
                params.push(("host".into(), host.clone()));
            }
        }
        Transport::Grpc { service_name } => {
            params.push(("type".into(), "grpc".into()));
            params.push(("serviceName".into(), service_name.clone()));
        }
        Transport::Http2 { path, host } => {
            params.push(("type".into(), "h2".into()));
            params.push(("path".into(), path.clone()));
            if let Some(host) = host {
                params.push(("host".into(), host.clone()));
            }
        }
        Transport::HttpUpgrade { path, host } => {
            params.push(("type".into(), "httpupgrade".into()));
            params.push(("path".into(), path.clone()));
            if let Some(host) = host {
                params.push(("host".into(), host.clone()));
            }
        }
        Transport::Quic => params.push(("type".into(), "quic".into())),
        Transport::Kcp { seed } => {
            params.push(("type".into(), "kcp".into()));
            if let Some(seed) = seed {
                params.push(("seed".into(), seed.clone()));
            }
        }
    }
}

fn encode_query(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Node;

    fn roundtrip(node: Node) -> Node {
        let link = node_to_share_link(&node).expect("link");
        super::super::links::parse_share_link(&link).expect("parsed")
    }

    #[test]
    fn vless_roundtrip() {
        let mut node = Node::new("test node", Protocol::Vless, "example.com", 443);
        node.credentials.uuid = Some("11111111-2222-3333-4444-555555555555".into());
        node.tls.enabled = true;
        node.tls.server_name = Some("cdn.example.com".into());
        node.transport = Transport::WebSocket {
            path: "/ws".into(),
            host: Some("cdn.example.com".into()),
            headers: Default::default(),
        };
        let parsed = roundtrip(node);
        assert_eq!(parsed.name, "test node");
        assert_eq!(parsed.tls.server_name.as_deref(), Some("cdn.example.com"));
    }

    #[test]
    fn ss_roundtrip() {
        let mut node = Node::new("ss", Protocol::Shadowsocks, "1.2.3.4", 8388);
        node.credentials.method = Some("aes-256-gcm".into());
        node.credentials.password = Some("secret".into());
        let parsed = roundtrip(node);
        assert_eq!(parsed.credentials.password.as_deref(), Some("secret"));
    }
}
