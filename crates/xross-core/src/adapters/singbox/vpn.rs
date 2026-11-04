//! sing-box 1.14 client VPN endpoint translation. No native schema is stored in AppModel.

use crate::model::{
    Node, NodeSource, OpenConnectOptions, OpenVpnOptions, Protocol, VpnCertificates,
};
use crate::{Error, Result};
use serde_json::{Value, json};

pub(crate) fn compile(node: &Node) -> Result<Value> {
    let cred = &node.credentials;
    let mut value = json!({"tag":node.outbound_tag(), "system":false,
        "username":cred.username, "password":cred.password});
    let cert;
    if node.protocol == Protocol::Openvpn {
        let options = cred
            .openvpn
            .as_ref()
            .ok_or_else(|| Error::config("缺少 OpenVPN 配置"))?;
        options.validate()?;
        if node.tls.insecure {
            return Err(Error::config("OpenVPN 不支持跳过证书验证"));
        }
        cert = &options.certificates;
        value["type"] = json!("openvpn-client");
        value["server"] = json!(node.server);
        value["server_port"] = json!(node.port);
        value["network"] = json!(options.transport);
        value["mtu"] = json!(options.mtu);
        value["data_ciphers"] = json!(options.data_ciphers);
        value["data_ciphers_fallback"] = json!(options.fallback_cipher);
        value["auth"] = json!(options.auth_digest);
        value["auth_retry"] = json!("nointeract");
        value["compression"] = json!(options.compression);
        value["compression_lzo"] = json!(options.compression_lzo);
        value["allow_compression"] = json!(options.allow_compression);
        if options.keepalive_interval > 0 {
            value["ping_interval"] = json!(format!("{}s", options.keepalive_interval));
        }
        if options.keepalive_timeout > 0 {
            value["ping_restart"] = json!(format!("{}s", options.keepalive_timeout));
        }
        if let Some(seconds) = options.renegotiate_seconds {
            if seconds == 0 {
                value["renegotiate_disabled"] = json!(true);
            } else {
                value["renegotiate_interval"] = json!(format!("{seconds}s"));
            }
        }
        value["tls"] = json!({"server_name":node.tls.server_name,
            "server_name_type":options.certificate_name_match,
            "remote_certificate_tls":options.certificate_purpose,
            "certificate":cert.authority_pem});
        if !options.control_mode.is_empty() {
            value["tls"]["control_wrap"] = json!({"type":options.control_mode,
                "key":options.control_key,"direction":options.control_direction});
        }
    } else {
        let options = cred
            .openconnect
            .as_ref()
            .ok_or_else(|| Error::config("缺少 OpenConnect 配置"))?;
        options.validate()?;
        cert = &options.certificates;
        value["type"] = json!("openconnect");
        value["server"] = json!(format!("https://{}{}", node.endpoint(), options.path));
        value["flavor"] = json!(options.flavor);
        value["auth_group"] = json!(options.auth_group);
        value["cookie"] = json!(options.cookie);
        value["no_udp"] = json!(options.disable_udp);
        value["mtu"] = json!(options.mtu);
        value["user_agent"] = json!(options.user_agent);
        value["reported_os"] = json!(options.reported_os);
        value["tls"] = json!({"server_name":node.tls.server_name,"insecure":node.tls.insecure,
            "certificate_authority":cert.authority_pem,"client_key_password":cert.private_key_password});
    }
    if cred.username.as_deref().unwrap_or("").is_empty()
        || cred.password.as_deref().unwrap_or("").is_empty()
    {
        let cookie = cred
            .openconnect
            .as_ref()
            .is_some_and(|o| !o.cookie.is_empty());
        if cert.certificate_pem.is_empty() && !cookie {
            return Err(Error::config(
                "VPN 需要账号密码、客户端证书或已认证会话，请编辑节点补充凭据",
            ));
        }
    }
    value["tls"]["client_certificate"] = json!(cert.certificate_pem);
    value["tls"]["client_key"] = json!(cert.private_key_pem);
    value["tls"]["peer_fingerprint"] = json!(cert.peer_fingerprints);
    // Empty PEM strings are not valid certificate list entries.
    value["tls"]
        .as_object_mut()
        .unwrap()
        .retain(|_, v| !v.is_null() && v != "");
    value
        .as_object_mut()
        .unwrap()
        .retain(|_, v| !v.is_null() && v != "");
    Ok(value)
}

pub(crate) fn parse(value: &Value) -> Result<Node> {
    let protocol = Protocol::parse(&string(value, "type"))
        .filter(Protocol::is_vpn)
        .ok_or_else(|| Error::config("VPN 类型无效"))?;
    let tls = value.get("tls").cloned().unwrap_or(json!({}));
    let mut node;
    if protocol == Protocol::Openconnect {
        check_fields(
            value,
            &[
                "type",
                "tag",
                "system",
                "name",
                "server",
                "username",
                "password",
                "flavor",
                "auth_group",
                "cookie",
                "no_udp",
                "mtu",
                "user_agent",
                "reported_os",
                "tls",
            ],
        )?;
        check_fields(
            &tls,
            &[
                "server_name",
                "insecure",
                "certificate_authority",
                "client_certificate",
                "client_key",
                "client_key_password",
                "peer_fingerprint",
            ],
        )?;
        let server = string(value, "server");
        let server = if server.contains("://") {
            server
        } else {
            format!("https://{server}")
        };
        let url =
            url::Url::parse(&server).map_err(|_| Error::config("OpenConnect 服务器 URL 无效"))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::config(
                "OpenConnect 服务器必须使用 HTTPS，不能含账号、查询参数或片段",
            ));
        }
        node = Node::new(
            string(value, "tag"),
            protocol,
            url.host_str()
                .ok_or_else(|| Error::config("OpenConnect 缺少服务器"))?
                .trim_matches(['[', ']']),
            url.port().unwrap_or(443),
        );
        let options = OpenConnectOptions {
            use_pushed_dns: true,
            flavor: nonempty(value, "flavor", "anyconnect"),
            path: url.path().into(),
            auth_group: string(value, "auth_group"),
            cookie: string(value, "cookie"),
            disable_udp: value["no_udp"].as_bool().unwrap_or(false),
            mtu: number(value, "mtu", 0)?,
            user_agent: string(value, "user_agent"),
            reported_os: string(value, "reported_os"),
            certificates: certificates(&tls, "certificate_authority")?,
        };
        options.validate()?;
        node.credentials.openconnect = Some(options);
        node.tls.insecure = tls["insecure"].as_bool().unwrap_or(false);
    } else {
        check_fields(
            value,
            &[
                "type",
                "tag",
                "system",
                "name",
                "server",
                "server_port",
                "mode",
                "network",
                "username",
                "password",
                "mtu",
                "data_ciphers",
                "data_ciphers_fallback",
                "cipher",
                "auth",
                "auth_retry",
                "compression",
                "compression_lzo",
                "allow_compression",
                "ping_interval",
                "ping_restart",
                "renegotiate_interval",
                "renegotiate_disabled",
                "tls",
            ],
        )?;
        check_fields(
            &tls,
            &[
                "server_name",
                "server_name_type",
                "certificate",
                "client_certificate",
                "client_key",
                "peer_fingerprint",
                "remote_certificate_tls",
                "control_wrap",
            ],
        )?;
        if !matches!(string(value, "mode").as_str(), "" | "tls") {
            return Err(Error::config("当前仅支持 OpenVPN TLS 客户端模式"));
        }
        if !matches!(
            string(value, "auth_retry").as_str(),
            "" | "none" | "nointeract"
        ) {
            return Err(Error::config(
                "当前 OpenVPN 不支持交互认证，请使用账号密码或证书",
            ));
        }
        node = Node::new(
            string(value, "tag"),
            protocol,
            string(value, "server"),
            number(value, "server_port", 1194)?,
        );
        let control = tls.get("control_wrap").cloned().unwrap_or(json!({}));
        check_fields(&control, &["type", "key", "direction"])?;
        let options = OpenVpnOptions {
            use_pushed_dns: true,
            certificates: certificates(&tls, "certificate")?,
            transport: nonempty(value, "network", "udp"),
            mtu: number(value, "mtu", 0)?,
            data_ciphers: strings(value, "data_ciphers")?,
            fallback_cipher: nonempty(value, "data_ciphers_fallback", &string(value, "cipher")),
            auth_digest: string(value, "auth"),
            control_mode: string(&control, "type"),
            control_key: strings(&control, "key")?.join("\n"),
            control_direction: string(&control, "direction"),
            certificate_name_match: string(&tls, "server_name_type"),
            certificate_purpose: nonempty(&tls, "remote_certificate_tls", "server"),
            compression: string(value, "compression"),
            compression_lzo: string(value, "compression_lzo"),
            allow_compression: string(value, "allow_compression"),
            keepalive_interval: seconds(value, "ping_interval")?.unwrap_or(0),
            keepalive_timeout: seconds(value, "ping_restart")?.unwrap_or(0),
            renegotiate_seconds: if value["renegotiate_disabled"] == true {
                Some(0)
            } else {
                seconds(value, "renegotiate_interval")?
            },
        };
        options.validate()?;
        node.credentials.openvpn = Some(options);
    }
    if value["system"] == true || !string(value, "name").is_empty() {
        return Err(Error::config(
            "VPN 节点使用用户态连接，请移除 system / name 系统接口选项后导入",
        ));
    }
    if node.server.is_empty() || node.port == 0 {
        return Err(Error::config("VPN 服务器或端口无效"));
    }
    node.source = NodeSource::Import;
    node.credentials.username = value
        .get("username")
        .and_then(Value::as_str)
        .map(str::to_owned);
    node.credentials.password = value
        .get("password")
        .and_then(Value::as_str)
        .map(str::to_owned);
    node.tls.enabled = true;
    node.tls.server_name = tls
        .get("server_name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(node)
}

fn certificates(value: &Value, ca: &str) -> Result<VpnCertificates> {
    Ok(VpnCertificates {
        authority_pem: strings(value, ca)?.join("\n"),
        certificate_pem: strings(value, "client_certificate")?.join("\n"),
        private_key_pem: strings(value, "client_key")?.join("\n"),
        private_key_password: string(value, "client_key_password"),
        peer_fingerprints: strings(value, "peer_fingerprint")?,
    })
}
fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").into()
}
fn nonempty(value: &Value, key: &str, default: &str) -> String {
    let s = string(value, key);
    if s.is_empty() { default.into() } else { s }
}
fn number(value: &Value, key: &str, default: u16) -> Result<u16> {
    match value.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| Error::config(format!("VPN {key} 必须是 0–65535 的整数"))),
    }
}
fn strings(value: &Value, key: &str) -> Result<Vec<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(if s.is_empty() {
            vec![]
        } else {
            vec![s.clone()]
        }),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::config(format!("VPN {key} 必须为字符串列表")))
            })
            .collect(),
        _ => Err(Error::config(format!("VPN {key} 必须为字符串或列表"))),
    }
}
fn seconds(value: &Value, key: &str) -> Result<Option<u32>> {
    let s = string(value, key);
    if s.is_empty() {
        return Ok(None);
    }
    let (n, m) = if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else {
        return Err(Error::config(format!(
            "VPN {key} 需要以 s / m / h 为单位的整数时长"
        )));
    };
    n.parse::<u32>()
        .ok()
        .and_then(|n| n.checked_mul(m))
        .map(Some)
        .ok_or_else(|| Error::config(format!("VPN {key} 时长无效")))
}
fn check_fields(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::config("VPN 配置必须为对象"))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::config(
            "VPN 配置包含暂不支持的选项；请使用账号密码、内嵌 PEM 证书及客户端基本选项",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openconnect_import_compile_and_share_roundtrip_preserve_auth_and_url() {
        let source = json!({"type":"openconnect","tag":"office","server":"https://[2001:db8::1]:8443/vpn/team",
            "flavor":"fortinet","username":"fixture-user","password":"fixture-password","auth_group":"employees",
            "cookie":"fake-cookie","no_udp":true,"mtu":1300,
            "tls":{"server_name":"vpn.example","certificate_authority":"-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----",
                "client_certificate":"-----BEGIN CERTIFICATE-----\nfake-client\n-----END CERTIFICATE-----",
                "client_key":"-----BEGIN ENCRYPTED PRIVATE KEY-----\nfake-key\n-----END ENCRYPTED PRIVATE KEY-----","client_key_password":"fake-key-pass"}});
        let node = parse(&source).unwrap();
        assert_eq!(node.server, "2001:db8::1");
        assert_eq!(node.port, 8443);
        let value = compile(&node).unwrap();
        assert_eq!(value["server"], source["server"]);
        assert_eq!(value["tls"]["client_key_password"], "fake-key-pass");
        assert_eq!(value["no_udp"], true);
        assert!(value.get("server_port").is_none());
        assert!(value["tls"].get("enabled").is_none());
        let mut draft = crate::model::NodeDraft::from(&node);
        draft.name = "renamed".into();
        let edited = draft
            .apply_to(&node, crate::model::CoreKind::SingBox)
            .unwrap();
        assert_eq!(edited.credentials, node.credentials);
        let link = crate::subscription::node_to_share_link(&edited).unwrap();
        let restored = crate::subscription::parse_share_link(&link).unwrap();
        assert_eq!(restored.credentials, edited.credentials);
        assert_ne!(restored.id, edited.id);
    }

    #[test]
    fn openvpn_compiles_own_tls_schema_and_roundtrips() {
        let source = json!({"type":"openvpn-client","tag":"office","server":"vpn.example","server_port":1194,
            "network":"tcp","username":"fixture","password":"fake-password","data_ciphers":["AES-128-GCM"],"auth":"SHA256",
            "tls":{"server_name":"VPN Server","server_name_type":"name","remote_certificate_tls":"server",
                "control_wrap":{"type":"tls_auth","key":"fake-control-key","direction":"client"}}});
        let node = parse(&source).unwrap();
        let value = compile(&node).unwrap();
        assert_eq!(value["type"], "openvpn-client");
        assert_eq!(value["system"], false);
        assert_eq!(value["tls"]["control_wrap"], source["tls"]["control_wrap"]);
        assert!(value["tls"].get("enabled").is_none());
        assert!(value.get("transport").is_none());
        let restored = parse(&value).unwrap();
        assert_eq!(restored.credentials, node.credentials);
        let link = crate::subscription::node_to_share_link(&node).unwrap();
        assert_eq!(
            crate::subscription::parse_share_link(&link)
                .unwrap()
                .credentials,
            node.credentials
        );
    }

    #[test]
    fn rejects_lossy_or_untrusted_vpn_imports() {
        for source in [
            json!({"type":"openconnect","server":"https://vpn.example/?secret=fake"}),
            json!({"type":"openconnect","server":"https://user:fake@vpn.example/"}),
            json!({"type":"openconnect","server":"vpn.example","tls":{"client_key_path":"/tmp/private"}}),
            json!({"type":"openconnect","server":"vpn.example","csd":{"wrapper_path":"/tmp/run"}}),
            json!({"type":"openvpn-client","server":"vpn.example","mode":"static_key"}),
            json!({"type":"openvpn-client","server":"vpn.example","auth_retry":"interact"}),
        ] {
            assert!(parse(&source).is_err());
        }
    }
}
