//! Node share links, including v2rayN, Shadowrocket and SIP002 formats.

use std::collections::BTreeMap;

use serde_json::Value;
use url::Url;

use crate::model::{Node, Protocol, RealityOptions, TlsOptions, Transport};
use crate::util::{decode_base64_any, truthy};

pub fn is_share_link(line: &str) -> bool {
    const SCHEMES: [&str; 14] = [
        "shadowrocket://add/",
        "xrose://vpn/",
        "vmess://",
        "vless://",
        "trojan://",
        "anytls://",
        "ss://",
        "hysteria2://",
        "hy2://",
        "tuic://",
        "socks://",
        "socks5://",
        "wireguard://",
        "wg://",
    ];
    SCHEMES.iter().any(|s| line.starts_with(s)) || is_http_proxy_link(line)
}

/// HTTP proxy exports need an explicit marker to distinguish them from subscription URLs.
pub(crate) fn is_http_proxy_link(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("http://") else {
        return false;
    };
    let body = rest.split('#').next().unwrap_or(rest);
    body.split_once('?').is_some_and(|(authority, query)| {
        !authority.contains('/')
            && url::form_urlencoded::parse(query.as_bytes())
                .any(|(k, v)| k == "xrose" && v == "proxy")
    })
}

pub fn parse_share_link(input: &str) -> Option<Node> {
    let line = input.trim();
    let unwrapped;
    let line = if let Some(inner) = line.strip_prefix("shadowrocket://add/") {
        if inner.contains("://") {
            inner
        } else {
            unwrapped = percent_decode(inner);
            &unwrapped
        }
    } else {
        line
    };
    if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
        return None;
    }
    if let Some(rest) = line.strip_prefix("xrose://vpn/") {
        let node: Node = serde_json::from_slice(&decode_base64_any(rest)?).ok()?;
        if !node.protocol.is_vpn() {
            return None;
        }
        let mut node = crate::model::NodeDraft::from(&node)
            .into_node(crate::model::CoreKind::SingBox)
            .ok()?;
        node.source = crate::model::NodeSource::Import;
        return Some(node);
    }
    if is_http_proxy_link(line) {
        return parse_proxy(Protocol::Http, line.strip_prefix("http://")?);
    }
    if let Some(rest) = line.strip_prefix("vmess://") {
        return parse_v2ray_link("vmess", rest);
    }
    if let Some(rest) = line.strip_prefix("vless://") {
        return parse_v2ray_link("vless", rest);
    }
    if let Some(rest) = line.strip_prefix("trojan://") {
        return parse_standard_url("trojan", rest);
    }
    if let Some(rest) = line.strip_prefix("anytls://") {
        return parse_standard_url("anytls", rest);
    }
    if let Some(rest) = line.strip_prefix("ss://") {
        return parse_proxy(Protocol::Shadowsocks, rest);
    }
    if let Some(rest) = line
        .strip_prefix("hysteria2://")
        .or_else(|| line.strip_prefix("hy2://"))
    {
        return parse_hysteria2(rest);
    }
    if let Some(rest) = line.strip_prefix("tuic://") {
        return parse_tuic(rest);
    }
    if let Some(rest) = line
        .strip_prefix("socks://")
        .or_else(|| line.strip_prefix("socks5://"))
    {
        return parse_proxy(Protocol::Socks, rest);
    }
    if line.starts_with("wireguard://") || line.starts_with("wg://") {
        return parse_wireguard(line);
    }
    None
}

fn parse_wireguard(line: &str) -> Option<Node> {
    let url = Url::parse(line).ok()?;
    let params: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let get = |keys: &[&str]| keys.iter().find_map(|key| params.get(*key).cloned());
    let draft = crate::model::NodeDraft {
        protocol: Protocol::Wireguard,
        name: fragment_name(&url, "WireGuard"),
        server: url.host_str()?.trim_matches(['[', ']']).into(),
        port: url.port().unwrap_or(51820),
        wireguard_private_key: percent_decode(url.username()),
        wireguard_public_key: get(&["publickey", "public_key", "publicKey"])?,
        wireguard_pre_shared_key: get(&["presharedkey", "pre_shared_key", "preSharedKey"])
            .unwrap_or_default(),
        wireguard_addresses: get(&["address", "local_address"])?,
        wireguard_allowed_ips: get(&["allowedips", "allowed_ips"])
            .unwrap_or_else(|| "0.0.0.0/0,::/0".into()),
        wireguard_mtu: get(&["mtu"]).unwrap_or_else(|| "1408".into()),
        wireguard_reserved: get(&["reserved"]).unwrap_or_else(|| "0,0,0".into()),
        ..Default::default()
    };
    draft.into_node(crate::model::CoreKind::SingBox).ok()
}

fn parse_v2ray_link(scheme: &str, rest: &str) -> Option<Node> {
    let (authority, suffix) = rest.split_at(rest.find(['?', '#']).unwrap_or(rest.len()));
    if authority.contains('@') {
        return parse_standard_url(scheme, rest);
    }
    let decoded = String::from_utf8(decode_base64_any(authority)?).ok()?;
    if scheme == "vmess" && decoded.trim_start().starts_with('{') {
        let mut node = parse_vmess_json(serde_json::from_str(&decoded).ok()?)?;
        if let Some((_, name)) = suffix.split_once('#').filter(|(_, name)| !name.is_empty()) {
            node.name = fragment_decode(name);
        }
        return Some(node);
    }

    // Shadowrocket encodes cipher:uuid@host:port, leaving the query outside.
    let (userinfo, endpoint) = decoded.rsplit_once('@')?;
    let (method, uuid) = userinfo.split_once(':')?;
    split_host_port(endpoint)?;
    let mut node = parse_standard_url(
        scheme,
        &format!("{}@{endpoint}{suffix}", percent_encode(uuid)),
    )?;
    if node.protocol == Protocol::Vmess {
        node.credentials.method = vmess_method(method);
    }
    Some(node)
}

fn vmess_method(method: &str) -> Option<String> {
    (!method.is_empty() && method != "auto").then(|| method.to_string())
}

fn parse_vmess_json(value: Value) -> Option<Node> {
    let get = |key: &str| -> Option<String> {
        match value.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    };
    let server = get("add")?;
    let port: u16 = get("port")?.parse().ok()?;
    let uuid = get("id")?;
    if server.is_empty() || port == 0 || uuid.is_empty() {
        return None;
    }
    let name = get("ps")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("VMess {server}"));
    let mut node = Node::new(name, Protocol::Vmess, server, port);
    node.credentials.uuid = Some(uuid);
    node.credentials.method = get("scy").and_then(|v| vmess_method(&v));
    node.credentials.alter_id = get("aid").and_then(|v| v.parse().ok()).unwrap_or(0);
    let host = get("host").filter(|h| !h.is_empty());
    let path = get("path").filter(|p| !p.is_empty());
    let tls = get("tls").unwrap_or_default();
    let sni = get("sni").filter(|s| !s.is_empty());
    node.tls = TlsOptions {
        enabled: tls.eq_ignore_ascii_case("tls") || sni.is_some(),
        server_name: sni.or_else(|| host.clone()),
        insecure: get("insecure").is_some_and(|v| truthy(&v)),
        alpn: split_list(get("alpn")),
        fingerprint: get("fp").filter(|f| !f.is_empty()),
        reality: None,
    };
    let net = get("net")
        .unwrap_or_else(|| "tcp".into())
        .to_ascii_lowercase();
    node.transport = match net.as_str() {
        "ws" => Transport::WebSocket {
            path: path.unwrap_or_else(|| "/".into()),
            host: host.clone(),
            headers: BTreeMap::new(),
        },
        "grpc" => Transport::Grpc {
            service_name: path.unwrap_or_default(),
        },
        "h2" | "http" => Transport::Http2 {
            path: path.unwrap_or_else(|| "/".into()),
            host: host.clone(),
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: path.unwrap_or_else(|| "/".into()),
            host: host.clone(),
        },
        "quic" => Transport::Quic,
        "kcp" => Transport::Kcp {
            seed: get("type").or(path),
        },
        "tcp" | "raw" | "" => Transport::Tcp,
        _ => return None,
    };
    Some(node)
}

fn parse_standard_url(scheme: &str, rest: &str) -> Option<Node> {
    let url = Url::parse(&format!("{scheme}://{rest}")).ok()?;
    let server = normalize_host(url.host_str()?);
    let port = url.port_or_known_default().unwrap_or(443);
    let raw_credential = if url.username().is_empty() {
        url.password()?
    } else {
        url.username()
    };
    let credential = percent_encoding::percent_decode_str(raw_credential)
        .decode_utf8()
        .ok()?
        .into_owned();
    if credential.is_empty() || port == 0 {
        return None;
    }
    let params: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let get = |key: &str| params.get(key).filter(|v| !v.is_empty()).cloned();

    let name = fragment_name(&url, &format!("{scheme} {server}"));
    let mut node = Node::new(name, Protocol::parse(scheme)?, server, port);
    match scheme {
        "vmess" => {
            node.credentials.uuid = Some(credential);
            node.credentials.method = get("encryption").and_then(|v| vmess_method(&v));
            node.credentials.alter_id = get("alterId")
                .or_else(|| get("aid"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
        "vless" => {
            node.credentials.uuid = Some(credential);
            node.credentials.flow = get("flow").or_else(|| match get("xtls").as_deref() {
                Some("1") => Some("xtls-rprx-direct".into()),
                Some("2") => Some("xtls-rprx-vision".into()),
                _ => None,
            });
        }
        "trojan" | "anytls" => {
            node.credentials.password = Some(credential);
        }
        _ => return None,
    }

    if scheme == "anytls" {
        node.credentials.anytls = Some(crate::model::AnytlsOptions {
            idle_session_check_interval: get("idle_session_check_interval"),
            idle_session_timeout: get("idle_session_timeout"),
            min_idle_session: get("min_idle_session").and_then(|v| v.parse().ok()),
        });
    }
    let security = get("security").map(|v| v.to_ascii_lowercase());
    let tls = match security.as_deref() {
        Some(mode) => matches!(mode, "tls" | "xtls" | "reality"),
        None => get("tls").map(|v| truthy(&v)).unwrap_or(scheme == "trojan"),
    };
    let sni = get("sni").or_else(|| get("peer"));
    let mut host = get("host")
        .or_else(|| get("obfsParam"))
        .or_else(|| get("wsHost"));
    let mut path = get("path").or_else(|| get("wspath"));
    let mut network = get("type")
        .or_else(|| get("obfs"))
        .or_else(|| get("network"));
    if scheme == "trojan"
        && network.is_none()
        && let Some(plugin) = get("plugin")
    {
        let mut parts = plugin.split(';');
        if parts.next()? != "obfs-local" {
            return None;
        }
        for (key, value) in parts.filter_map(|p| p.split_once('=')) {
            match key {
                "obfs" => network = Some(value.into()),
                "obfs-host" => host = Some(value.into()),
                "obfs-uri" => path = Some(value.into()),
                _ => {}
            }
        }
    }
    let reality = if security.as_deref() == Some("reality") || (security.is_none() && tls) {
        get("pbk").map(|public_key| RealityOptions {
            public_key,
            short_id: get("sid").unwrap_or_default(),
        })
    } else {
        None
    };
    node.tls = TlsOptions {
        enabled: scheme == "anytls" || tls,
        server_name: sni.or_else(|| host.clone()),
        insecure: get("allowInsecure")
            .or_else(|| get("insecure"))
            .map(|v| truthy(&v))
            .unwrap_or(false),
        alpn: split_list(get("alpn")),
        fingerprint: get("fp").or_else(|| get("fingerprint")),
        reality,
    };

    node.transport = match network
        .unwrap_or_else(|| "tcp".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "ws" | "websocket" => Transport::WebSocket {
            path: path.unwrap_or_else(|| "/".into()),
            host,
            headers: BTreeMap::new(),
        },
        "grpc" => Transport::Grpc {
            service_name: get("serviceName").or(path).unwrap_or_default(),
        },
        "h2" | "http" => Transport::Http2 {
            path: path.unwrap_or_else(|| "/".into()),
            host,
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: path.unwrap_or_else(|| "/".into()),
            host,
        },
        "quic" => Transport::Quic,
        "kcp" => Transport::Kcp { seed: get("seed") },
        "raw" | "tcp" | "none" => Transport::Tcp,
        _ => return None,
    };
    Some(node)
}

fn parse_hysteria2(rest: &str) -> Option<Node> {
    let url = Url::parse(&format!("hysteria2://{rest}")).ok()?;
    let server = normalize_host(url.host_str()?);
    let port = url.port().unwrap_or(443);
    let password = match url.password() {
        Some(pass) if !url.username().is_empty() => format!(
            "{}:{}",
            percent_decode(url.username()),
            percent_decode(pass)
        ),
        Some(pass) => percent_decode(pass),
        None => percent_decode(url.username()),
    };
    let params: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut node = Node::new(
        fragment_name(&url, &format!("Hysteria2 {server}")),
        Protocol::Hysteria2,
        server,
        port,
    );
    node.credentials.password = Some(password);
    node.credentials.obfs = params
        .get("obfs")
        .cloned()
        .filter(|v| !v.is_empty() && v != "none");
    node.credentials.obfs_password = params.get("obfs-password").cloned();
    node.tls = TlsOptions {
        enabled: true,
        server_name: params.get("sni").or_else(|| params.get("peer")).cloned(),
        insecure: params
            .get("insecure")
            .or_else(|| params.get("allowInsecure"))
            .is_some_and(|v| truthy(v)),
        alpn: split_list(params.get("alpn").cloned()),
        fingerprint: None,
        reality: None,
    };
    Some(node)
}

fn parse_tuic(rest: &str) -> Option<Node> {
    let url = Url::parse(&format!("tuic://{rest}")).ok()?;
    let server = normalize_host(url.host_str()?);
    let port = url.port().unwrap_or(443);
    let params: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut node = Node::new(
        fragment_name(&url, &format!("TUIC {server}")),
        Protocol::Tuic,
        server,
        port,
    );
    // v2rayN may escape the entire uuid:password pair as one URI component.
    let userinfo = percent_decode(url.username());
    let (uuid, password) = if userinfo.is_empty() {
        (params.get("uuid")?.clone(), params.get("password")?.clone())
    } else if let Some(password) = url.password() {
        (userinfo, percent_decode(password))
    } else {
        let (uuid, password) = userinfo.split_once(':').unwrap_or((&userinfo, ""));
        (uuid.into(), password.into())
    };
    node.credentials.uuid = Some(uuid);
    node.credentials.password = Some(password);
    node.credentials.congestion_control = params.get("congestion_control").cloned();
    node.credentials.udp_relay_mode = params.get("udp_relay_mode").cloned();
    node.tls = TlsOptions {
        enabled: true,
        server_name: params.get("sni").or_else(|| params.get("peer")).cloned(),
        insecure: params
            .get("allow_insecure")
            .or_else(|| params.get("insecure"))
            .or_else(|| params.get("allowInsecure"))
            .is_some_and(|v| truthy(v)),
        alpn: split_list(params.get("alpn").cloned()),
        fingerprint: None,
        reality: None,
    };
    Some(node)
}

fn parse_proxy(protocol: Protocol, rest: &str) -> Option<Node> {
    let (body, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (body, query) = body.split_once('?').unwrap_or((body, ""));
    let decoded;
    let (userinfo, endpoint) = if let Some((userinfo, endpoint)) = body.rsplit_once('@') {
        (Some(decode_userinfo(userinfo)), endpoint)
    } else if matches!(protocol, Protocol::Socks | Protocol::Http) && body.contains(':') {
        (None, body)
    } else {
        decoded = String::from_utf8(decode_base64_any(body)?).ok()?;
        match decoded.rsplit_once('@') {
            Some((userinfo, endpoint)) => (Some(userinfo.to_string()), endpoint),
            None if protocol == Protocol::Socks => (None, decoded.as_str()),
            _ => return None,
        }
    };
    // Older SS links can put plugin options inside the encoded endpoint.
    let (endpoint, query) = endpoint.split_once('?').unwrap_or((endpoint, query));
    let (host, port) = split_host_port(endpoint)?;
    let mut node = Node::new(
        format!(
            "{} {host}",
            if protocol == Protocol::Shadowsocks {
                "SS"
            } else {
                protocol.display_name()
            }
        ),
        protocol,
        host,
        port,
    );
    if let Some(userinfo) = userinfo {
        let (user, password) = userinfo
            .split_once(':')
            .map_or((userinfo.as_str(), None), |(u, p)| (u, Some(p.to_string())));
        if protocol == Protocol::Shadowsocks {
            node.credentials.method = Some(user.to_string());
            node.credentials.password = Some(password?);
        } else {
            node.credentials.username = Some(user.to_string());
            node.credentials.password = password;
        }
    }
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "remark" | "remarks" if !value.is_empty() => node.name = value.into_owned(),
            "plugin" if protocol == Protocol::Shadowsocks => {
                let (plugin, opts) = value.split_once(';').unwrap_or((&value, ""));
                node.credentials.plugin = Some(plugin.into());
                node.credentials.plugin_opts = (!opts.is_empty()).then(|| opts.into());
            }
            _ => {}
        }
    }
    if !fragment.is_empty() {
        node.name = fragment_decode(fragment);
    }
    Some(node)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn split_list(value: Option<String>) -> Vec<String> {
    value
        .map(|a| {
            a.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn decode_userinfo(value: &str) -> String {
    let value = percent_decode(value);
    if let Some(decoded) = decode_base64_any(&value) {
        if let Ok(text) = String::from_utf8(decoded) {
            if text.contains(':') {
                return text;
            }
        }
    }
    value
}

fn fragment_name(url: &Url, fallback: &str) -> String {
    url.fragment()
        .map(fragment_decode)
        .filter(|f| !f.is_empty())
        .or_else(|| {
            url.query_pairs()
                .find(|(k, v)| matches!(k.as_ref(), "remarks" | "remark") && !v.is_empty())
                .map(|(_, v)| v.into_owned())
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn split_host_port(value: &str) -> Option<(String, u16)> {
    let value = value.trim().trim_end_matches('/');
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        (host, tail.strip_prefix(':')?)
    } else {
        value.rsplit_once(':')?
    };
    let port = port.parse().ok()?;
    if port == 0
        || host.is_empty()
        || host.contains(['/', '?', '#', '@'])
        || host.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some((host.to_string(), port))
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

fn fragment_decode(value: &str) -> String {
    // Older Xrose links use form encoding for spaces in the display name.
    percent_decode(&value.replace('+', " "))
}

pub(crate) fn format_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

pub(crate) fn percent_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::encode_base64;

    #[test]
    fn shadowrocket_vmess_preserves_transport_tls_and_cipher() {
        let body =
            encode_base64(b"aes-128-gcm:00000000-0000-4000-8000-000000000001@[2001:db8::1]:8443");
        let link = format!(
            "vmess://{body}?remarks=%E6%B5%8B%E8%AF%95+WS&obfs=websocket&obfsParam=cdn.example&path=%2Fa%3Fb%3Dc&tls=1&peer=tls.example&allowInsecure=1&alterId=4"
        );
        let node = parse_share_link(&link).unwrap();
        assert_eq!(node.name, "测试 WS");
        assert_eq!((node.server.as_str(), node.port), ("2001:db8::1", 8443));
        assert_eq!(node.credentials.alter_id, 4);
        assert_eq!(node.credentials.method.as_deref(), Some("aes-128-gcm"));
        assert!(node.tls.enabled && node.tls.insecure);
        assert_eq!(node.tls.server_name.as_deref(), Some("tls.example"));
        assert!(
            matches!(&node.transport, Transport::WebSocket { path, host, .. } if path == "/a?b=c" && host.as_deref() == Some("cdn.example"))
        );
        let exported = super::super::links_export::node_to_share_link(&node).unwrap();
        let restored = parse_share_link(&exported).unwrap();
        assert_eq!(restored.credentials, node.credentials);
        assert_eq!(restored.tls, node.tls);
        assert_eq!(restored.transport, node.transport);
    }

    #[test]
    fn shadowrocket_vless_reality_and_standard_vmess() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let uuid = "00000000-0000-4000-8000-000000000002";
        let body = URL_SAFE_NO_PAD.encode(format!("none:{uuid}@node.example:443"));
        let link = format!(
            "vless://{body}?remark=old&tls=1&pbk=fixture&sid=abcd&xtls=2&fingerprint=chrome&obfs=grpc&path=rpc#New%20name"
        );
        let node = parse_share_link(&link).unwrap();
        assert_eq!(node.name, "New name");
        assert_eq!(node.credentials.uuid.as_deref(), Some(uuid));
        assert_eq!(node.credentials.flow.as_deref(), Some("xtls-rprx-vision"));
        assert!(node.tls.enabled);
        assert_eq!(node.tls.reality.unwrap().short_id, "abcd");
        assert_eq!(node.tls.fingerprint.as_deref(), Some("chrome"));
        assert!(
            matches!(node.transport, Transport::Grpc { service_name } if service_name == "rpc")
        );
        let standard = parse_share_link(&format!(
            "vmess://{uuid}@node.example:443?encryption=none&type=ws&security=tls"
        ))
        .unwrap();
        assert_eq!(standard.credentials.uuid.as_deref(), Some(uuid));
        assert_eq!(standard.credentials.method.as_deref(), Some("none"));
        assert!(standard.tls.enabled);
        assert!(parse_share_link("vless://broken?remarks=invalid").is_none());
    }

    #[test]
    fn trojan_websocket_plugin_uses_tls_by_default() {
        let node = parse_share_link("trojan://fixture%2Bvalue@node.example:443?peer=tls.example&plugin=obfs-local%3Bobfs%3Dwebsocket%3Bobfs-host%3Dcdn.example%3Bobfs-uri%3D%2Fws").unwrap();
        assert!(node.tls.enabled);
        assert_eq!(node.credentials.password.as_deref(), Some("fixture+value"));
        assert_eq!(node.tls.server_name.as_deref(), Some("tls.example"));
        assert!(
            matches!(node.transport, Transport::WebSocket { path, host, .. } if path == "/ws" && host.as_deref() == Some("cdn.example"))
        );
        assert!(
            !parse_share_link("trojan://fixture@node.example?security=none")
                .unwrap()
                .tls
                .enabled
        );
    }

    #[test]
    fn encoded_proxy_credentials_are_decoded_once() {
        for scheme in ["ss", "socks"] {
            let user = if scheme == "ss" {
                "aes-256-gcm"
            } else {
                "fixture"
            };
            let credentials = format!("{user}:fixture+%2F=:value");
            let auth = percent_encode(&encode_base64(credentials.as_bytes()));
            let endpoint = "node.example:1080";
            let full = encode_base64(format!("{credentials}@{endpoint}").as_bytes());
            for authority in [format!("{auth}@{endpoint}"), full] {
                let node =
                    parse_share_link(&format!("{scheme}://{authority}?remarks=fixture")).unwrap();
                assert_eq!(node.name, "fixture");
                assert_eq!(
                    node.credentials.password.as_deref(),
                    Some("fixture+%2F=:value")
                );
            }
        }
    }

    #[test]
    fn tuic_accepts_v2rayn_and_shadowrocket_credentials() {
        let uuid = "00000000-0000-4000-8000-000000000003";
        for link in [
            format!("tuic://{uuid}%3Afixture%2B%253A@node.example:443?allow_insecure=1"),
            format!("tuic://node.example:443?uuid={uuid}&password=fixture%2B%253A&insecure=1"),
        ] {
            let node = parse_share_link(&link).unwrap();
            assert_eq!(node.credentials.uuid.as_deref(), Some(uuid));
            assert_eq!(node.credentials.password.as_deref(), Some("fixture+%3A"));
            assert!(node.tls.insecure);
        }
    }

    #[test]
    fn parses_vmess() {
        let json = r#"{"v":"2","ps":"测试","add":"example.com","port":"443","id":"11111111-2222-3333-4444-555555555555","aid":"0","net":"ws","host":"cdn.example.com","path":"/ws","tls":"tls","sni":"cdn.example.com"}"#;
        let link = format!("vmess://{}", crate::util::encode_base64(json.as_bytes()));
        let node = parse_share_link(&link).unwrap();
        assert_eq!(node.name, "测试");
        assert_eq!(node.port, 443);
        assert!(matches!(node.transport, Transport::WebSocket { .. }));
        assert!(node.tls.enabled);
    }

    #[test]
    fn parses_vless_reality() {
        let link = "vless://11111111-2222-3333-4444-555555555555@example.com:443?encryption=none&security=reality&sni=www.example.com&type=grpc&serviceName=grpcsvc&pbk=abc&sid=def&flow=xtls-rprx-vision#节点%201";
        let node = parse_share_link(link).unwrap();
        assert_eq!(node.name, "节点 1");
        assert!(node.tls.reality.is_some());
        assert_eq!(node.credentials.flow.as_deref(), Some("xtls-rprx-vision"));
        assert!(matches!(node.transport, Transport::Grpc { .. }));
    }

    #[test]
    fn parses_shadowsocks() {
        let link = "ss://YWVzLTI1Ni1nY206cGFzc3dvcmQ=@1.2.3.4:8388#SS";
        let node = parse_share_link(link).unwrap();
        assert_eq!(node.server, "1.2.3.4");
        assert_eq!(node.credentials.method.as_deref(), Some("aes-256-gcm"));
        assert_eq!(node.credentials.password.as_deref(), Some("password"));
    }

    #[test]
    fn parses_hysteria2_and_tuic() {
        let hy = parse_share_link("hysteria2://pass@example.com:8443?sni=example.com&insecure=1&obfs=salamander&obfs-password=x#HY").unwrap();
        assert_eq!(hy.protocol, Protocol::Hysteria2);
        assert!(hy.tls.insecure);
        let tuic = parse_share_link("tuic://11111111-2222-3333-4444-555555555555:pw@example.com:443?congestion_control=bbr#TUIC").unwrap();
        assert_eq!(tuic.protocol, Protocol::Tuic);
        assert_eq!(tuic.credentials.password.as_deref(), Some("pw"));
    }
}
