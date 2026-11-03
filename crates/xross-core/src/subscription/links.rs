//! v2rayN / SIP002 share link parsing.

use std::collections::BTreeMap;

use serde_json::Value;
use url::Url;

use crate::model::{Node, Protocol, RealityOptions, TlsOptions, Transport};
use crate::util::{decode_base64_any, truthy};

pub fn is_share_link(line: &str) -> bool {
    const SCHEMES: [&str; 10] = [
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
    ];
    SCHEMES.iter().any(|s| line.starts_with(s))
}

pub fn parse_share_link(input: &str) -> Option<Node> {
    let line = input.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
        return None;
    }
    if let Some(rest) = line.strip_prefix("vmess://") {
        return parse_vmess(rest);
    }
    if let Some(rest) = line.strip_prefix("vless://") {
        return parse_standard_url("vless", rest);
    }
    if let Some(rest) = line.strip_prefix("trojan://") {
        return parse_standard_url("trojan", rest);
    }
    if let Some(rest) = line.strip_prefix("anytls://") {
        return parse_standard_url("anytls", rest);
    }
    if let Some(rest) = line.strip_prefix("ss://") {
        return parse_ss(rest);
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
        return parse_plain_proxy(Protocol::Socks, rest);
    }
    None
}

fn parse_vmess(rest: &str) -> Option<Node> {
    let decoded = decode_base64_any(rest)?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
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
    let name = get("ps")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("VMess {server}"));
    let mut node = Node::new(name, Protocol::Vmess, server, port);
    node.credentials.uuid = Some(uuid);
    node.credentials.alter_id = get("aid").and_then(|v| v.parse().ok()).unwrap_or(0);
    let host = get("host").filter(|h| !h.is_empty());
    let path = get("path").filter(|p| !p.is_empty());
    let tls = get("tls").unwrap_or_default();
    let sni = get("sni").filter(|s| !s.is_empty());
    node.tls = TlsOptions {
        enabled: tls.eq_ignore_ascii_case("tls") || sni.is_some(),
        server_name: sni.or_else(|| host.clone()),
        insecure: false,
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
        _ => Transport::Tcp,
    };
    Some(node)
}

fn parse_standard_url(scheme: &str, rest: &str) -> Option<Node> {
    let url = Url::parse(&format!("{scheme}://{rest}")).ok()?;
    let server = normalize_host(url.host_str()?);
    let port = url.port_or_known_default().unwrap_or(443);
    let username = percent_encoding::percent_decode_str(url.username())
        .decode_utf8()
        .ok()?
        .into_owned();
    let password = url
        .password()
        .map(|p| {
            percent_encoding::percent_decode_str(p)
                .decode_utf8()
                .map(|s| s.into_owned())
        })
        .transpose()
        .ok()?;
    let name = fragment_name(&url, &format!("{scheme} {server}"));
    let params: BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let get = |key: &str| params.get(key).filter(|v| !v.is_empty()).cloned();

    let mut node = Node::new(name, Protocol::parse(scheme)?, server, port);
    match scheme {
        "vless" => {
            node.credentials.uuid = Some(if username.is_empty() {
                password.unwrap_or_default()
            } else {
                username
            });
            node.credentials.flow = get("flow");
        }
        "trojan" | "anytls" => {
            node.credentials.password = Some(if username.is_empty() {
                password.unwrap_or_default()
            } else {
                username
            });
        }
        _ => return None,
    }

    let security = get("security").unwrap_or_else(|| "none".into());
    let sni = get("sni").or_else(|| get("peer"));
    let reality = if security.eq_ignore_ascii_case("reality") {
        get("pbk").map(|public_key| RealityOptions {
            public_key,
            short_id: get("sid").unwrap_or_default(),
        })
    } else {
        None
    };
    node.tls = TlsOptions {
        enabled: scheme == "anytls" || matches!(security.as_str(), "tls" | "reality" | "xtls"),
        server_name: sni.clone().or_else(|| get("host")),
        insecure: get("allowInsecure")
            .or_else(|| get("insecure"))
            .map(|v| truthy(&v))
            .unwrap_or(false),
        alpn: split_list(get("alpn")),
        fingerprint: get("fp"),
        reality,
    };

    let host = get("host").filter(|h| !h.is_empty());
    let path = get("path").filter(|p| !p.is_empty());
    node.transport = match get("type")
        .unwrap_or_else(|| "tcp".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "ws" => Transport::WebSocket {
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
        _ => Transport::Tcp,
    };
    Some(node)
}

fn parse_ss(rest: &str) -> Option<Node> {
    let (body, name) = split_fragment(rest);
    let body = body.trim();
    let (userinfo, host_port, query) = if let Some(at) = body.rfind('@') {
        let tail = &body[at + 1..];
        let (host_port, query) = tail.split_once('?').unwrap_or((tail, ""));
        (
            decode_userinfo(&body[..at]),
            host_port.to_string(),
            query.to_string(),
        )
    } else {
        let decoded = decode_base64_any(body)?;
        let text = String::from_utf8(decoded).ok()?;
        let (before_query, query) = text.split_once('?').unwrap_or((&text, ""));
        let at = before_query.rfind('@')?;
        (
            before_query[..at].to_string(),
            before_query[at + 1..].to_string(),
            query.to_string(),
        )
    };
    let (method, password) = userinfo.split_once(':')?;
    let (host, port) = split_host_port(&host_port)?;
    let mut node = Node::new(
        if name.is_empty() {
            format!("SS {host}")
        } else {
            percent_decode(&name)
        },
        Protocol::Shadowsocks,
        normalize_host(&host),
        port,
    );
    node.credentials.method = Some(method.to_string());
    node.credentials.password = Some(password.to_string());
    if !query.is_empty() {
        if let Ok(url) = Url::parse(&format!("https://placeholder/?{query}")) {
            for (k, v) in url.query_pairs() {
                if k == "plugin" {
                    let mut parts = v.split(';');
                    node.credentials.plugin = parts.next().map(|s| s.to_string());
                    let opts: Vec<&str> = parts.collect();
                    if !opts.is_empty() {
                        node.credentials.plugin_opts = Some(opts.join(";"));
                    }
                }
            }
        }
    }
    Some(node)
}

fn parse_hysteria2(rest: &str) -> Option<Node> {
    let url = Url::parse(&format!("hysteria2://{rest}")).ok()?;
    let server = normalize_host(url.host_str()?);
    let port = url.port().unwrap_or(443);
    let password = if !url.username().is_empty() {
        percent_decode(url.username())
    } else {
        url.password().map(percent_decode).unwrap_or_default()
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
    node.credentials.obfs = params.get("obfs").cloned().filter(|v| !v.is_empty());
    node.credentials.obfs_password = params.get("obfs-password").cloned();
    node.tls = TlsOptions {
        enabled: true,
        server_name: params.get("sni").cloned(),
        insecure: params.get("insecure").map(|v| truthy(v)).unwrap_or(false),
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
    let uuid = percent_decode(url.username());
    let password = url.password().map(percent_decode).unwrap_or_default();
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
    node.credentials.uuid = Some(uuid);
    node.credentials.password = Some(password);
    node.credentials.congestion_control = params.get("congestion_control").cloned();
    node.credentials.udp_relay_mode = params.get("udp_relay_mode").cloned();
    node.tls = TlsOptions {
        enabled: true,
        server_name: params.get("sni").cloned(),
        insecure: params
            .get("allow_insecure")
            .map(|v| truthy(v))
            .unwrap_or(false),
        alpn: split_list(params.get("alpn").cloned()),
        fingerprint: None,
        reality: None,
    };
    Some(node)
}

fn parse_plain_proxy(protocol: Protocol, rest: &str) -> Option<Node> {
    let (body, fragment) = split_fragment(rest);
    let (body, _query) = body.split_once('?').unwrap_or((&body, ""));
    let (userinfo, host_port) = match body.rfind('@') {
        Some(at) => (Some(&body[..at]), &body[at + 1..]),
        None => (None, body),
    };
    let (host, port) = split_host_port(host_port)?;
    let mut node = Node::new(
        format!("{} {host}", protocol.display_name()),
        protocol,
        normalize_host(&host),
        port,
    );
    if let Some(userinfo) = userinfo {
        let decoded = decode_userinfo(userinfo);
        if let Some((user, pass)) = decoded.split_once(':') {
            node.credentials.username = Some(percent_decode(user));
            node.credentials.password = Some(percent_decode(pass));
        } else {
            node.credentials.username = Some(percent_decode(&decoded));
        }
    }
    if !fragment.is_empty() {
        node.name = percent_decode(&fragment);
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
    if let Some(decoded) = decode_base64_any(value) {
        if let Ok(text) = String::from_utf8(decoded) {
            if text.contains(':') {
                return text;
            }
        }
    }
    percent_decode(value)
}

fn split_fragment(value: &str) -> (String, String) {
    value
        .split_once('#')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .unwrap_or_else(|| (value.to_string(), String::new()))
}

fn fragment_name(url: &Url, fallback: &str) -> String {
    url.fragment()
        .map(percent_decode)
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn split_host_port(value: &str) -> Option<(String, u16)> {
    let value = value.trim().trim_end_matches('/');
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        return Some((host.to_string(), tail.strip_prefix(':')?.parse().ok()?));
    }
    let (host, port) = value.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

pub(crate) fn percent_decode(value: &str) -> String {
    url::form_urlencoded::parse(value.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn format_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

pub(crate) fn percent_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
