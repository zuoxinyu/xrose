//! Import self-contained OpenVPN client profiles without executing directives or reading files.

use super::parse::{ParsedSubscription, SourceFormat};
use crate::model::{Node, NodeSource, OpenVpnOptions, Protocol};
use crate::{Error, Result};
use std::collections::BTreeMap;

pub(super) fn detect(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim();
        line == "client" || line.starts_with("remote ") || line.starts_with("remote\t")
    })
}

pub(super) fn parse(content: &str) -> ParsedSubscription {
    match parse_profile(content) {
        Ok(parsed) => parsed,
        Err(error) => ParsedSubscription {
            format: SourceFormat::OpenVpn,
            warnings: vec![error.to_string()],
            ..Default::default()
        },
    }
}

fn parse_profile(content: &str) -> Result<ParsedSubscription> {
    let mut result = ParsedSubscription {
        format: SourceFormat::OpenVpn,
        ..Default::default()
    };
    let mut options = OpenVpnOptions::default();
    let mut blocks = BTreeMap::new();
    let mut active: Option<(String, String)> = None;
    let mut directives = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some((tag, body)) = &mut active {
            if trimmed == format!("</{tag}>") {
                let (tag, body) = active.take().unwrap();
                if blocks.insert(tag, body).is_some() {
                    return Err(Error::config("OpenVPN 配置含重复的证书或凭据块"));
                }
            } else {
                body.push_str(line);
                body.push('\n');
            }
            continue;
        }
        if trimmed.starts_with('<') {
            let tag = trimmed
                .strip_prefix('<')
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("");
            if !matches!(
                tag,
                "ca" | "cert"
                    | "key"
                    | "tls-auth"
                    | "tls-crypt"
                    | "tls-crypt-v2"
                    | "auth-user-pass"
            ) {
                return Err(Error::config(
                    "OpenVPN 包含不支持的内嵌块；支持 ca / cert / key / tls-auth / tls-crypt / tls-crypt-v2 / auth-user-pass",
                ));
            }
            active = Some((tag.into(), String::new()));
            continue;
        }
        let tokens = tokenize(line)?;
        if !tokens.is_empty() {
            directives.push(tokens);
        }
    }
    if active.is_some() {
        return Err(Error::config("OpenVPN 内嵌证书或凭据块未闭合"));
    }
    let mut remotes = Vec::new();
    let mut port = 1194;
    let mut username = None;
    let mut password = None;
    let mut server_name = None;
    let mut auth_required = false;
    for fields in directives {
        let key = fields[0].as_str();
        let value = || {
            fields
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| Error::config(format!("OpenVPN {key} 缺少参数")))
        };
        let uint = || {
            value()?
                .parse::<u32>()
                .map_err(|_| Error::config(format!("OpenVPN {key} 必须为整数")))
        };
        match key {
            "client" | "tls-client" | "pull" => {}
            "dev" => {
                if !value()?.starts_with("tun") {
                    return Err(Error::config("仅支持 OpenVPN TUN 客户端，不支持 TAP"));
                }
            }
            "proto" => options.transport = network(value()?)?,
            "port" | "rport" => {
                port = uint()?
                    .try_into()
                    .map_err(|_| Error::config("OpenVPN 端口无效"))?
            }
            "remote" => {
                let host = value()?.to_string();
                let remote_port = fields
                    .get(2)
                    .map(|s| s.parse::<u16>())
                    .transpose()
                    .map_err(|_| Error::config("OpenVPN remote 端口无效"))?;
                let transport = fields.get(3).map(|s| network(s)).transpose()?;
                if fields.len() > 4 {
                    return Err(Error::config("OpenVPN remote 参数过多"));
                }
                remotes.push((host, remote_port, transport));
            }
            "auth-user-pass" => {
                auth_required = true;
                if fields.get(1).is_some_and(|v| v != "[inline]") {
                    return Err(external_file());
                }
            }
            "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "tls-crypt-v2" => {
                if value()? != "[inline]" || !blocks.contains_key(key) {
                    return Err(external_file());
                }
                if key == "tls-auth" {
                    if let Some(direction) = fields.get(2) {
                        options.control_direction = key_direction(direction)?;
                    }
                }
            }
            "key-direction" => options.control_direction = key_direction(value()?)?,
            "remote-cert-tls" => options.certificate_purpose = value()?.into(),
            "verify-x509-name" => {
                server_name = Some(value()?.into());
                options.certificate_name_match =
                    fields.get(2).cloned().unwrap_or_else(|| "subject".into());
            }
            "data-ciphers" | "ncp-ciphers" => {
                options.data_ciphers = value()?.split(':').map(str::to_owned).collect()
            }
            "data-ciphers-fallback" | "cipher" => options.fallback_cipher = value()?.into(),
            "auth" => options.auth_digest = value()?.into(),
            "tun-mtu" => {
                options.mtu = uint()?
                    .try_into()
                    .map_err(|_| Error::config("OpenVPN MTU 无效"))?
            }
            "ping" => options.keepalive_interval = uint()?,
            "ping-restart" => options.keepalive_timeout = uint()?,
            "keepalive" => {
                options.keepalive_interval = uint()?;
                options.keepalive_timeout = fields
                    .get(2)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| Error::config("OpenVPN keepalive 需要间隔和超时两个整数"))?;
            }
            "reneg-sec" => options.renegotiate_seconds = Some(uint()?),
            "compress" => {
                options.compression = fields.get(1).cloned().unwrap_or_else(|| "stub".into())
            }
            "comp-lzo" => {
                options.compression_lzo =
                    fields.get(1).cloned().unwrap_or_else(|| "adaptive".into())
            }
            "allow-compression" => options.allow_compression = value()?.into(),
            "nobind" | "persist-key" | "persist-tun" | "verb" | "mute" | "resolv-retry"
            | "auth-nocache" | "remote-random" => {
                result.warnings.push(format!(
                    "OpenVPN {key} 未映射，连接生命周期由 Xrose 和内核管理"
                ));
            }
            "route" | "route-ipv6" | "redirect-gateway" | "redirect-private" | "route-nopull"
            | "dhcp-option" => {
                return Err(Error::config(
                    "OpenVPN 配置含本地路由或 DNS 指令，请在 Xrose 中配置对应路由 / DNS 后移除这些指令再导入",
                ));
            }
            _ => {
                return Err(Error::config(
                    "OpenVPN 配置包含当前不支持的指令，未导入该配置",
                ));
            }
        }
    }
    options.certificates.authority_pem = blocks.remove("ca").unwrap_or_default();
    options.certificates.certificate_pem = blocks.remove("cert").unwrap_or_default();
    options.certificates.private_key_pem = blocks.remove("key").unwrap_or_default();
    for (tag, mode) in [
        ("tls-auth", "tls_auth"),
        ("tls-crypt", "tls_crypt"),
        ("tls-crypt-v2", "tls_crypt_v2"),
    ] {
        if let Some(body) = blocks.remove(tag) {
            if !options.control_mode.is_empty() {
                return Err(Error::config("OpenVPN 控制通道密钥模式不能同时设置多个"));
            }
            options.control_mode = mode.into();
            options.control_key = body;
        }
    }
    if let Some(body) = blocks.remove("auth-user-pass") {
        let lines: Vec<_> = body.lines().collect();
        if lines.len() != 2 {
            return Err(Error::config(
                "内嵌 auth-user-pass 必须恰好包含用户名和密码两行",
            ));
        }
        username = Some(lines[0].to_string());
        password = Some(lines[1].to_string());
    } else if auth_required {
        result
            .warnings
            .push("OpenVPN 需要账号密码，请编辑导入的节点补充凭据".into());
    }
    options.validate()?;
    if remotes.is_empty() {
        return Err(Error::config("OpenVPN 配置缺少 remote 服务器"));
    }
    if remotes.len() > 1 {
        result
            .warnings
            .push("多个 OpenVPN remote 已拆分为独立节点，可添加到出站组".into());
    }
    for (host, remote_port, transport) in remotes {
        let port = remote_port.unwrap_or(port);
        if port == 0
            || host.contains("://")
            || host.contains('/')
            || host.chars().any(char::is_whitespace)
        {
            return Err(Error::config("OpenVPN remote 地址或端口无效"));
        }
        let mut node = Node::new(
            format!("OpenVPN {host}:{port}"),
            Protocol::Openvpn,
            host.trim_matches(['[', ']']),
            port,
        );
        let mut options = options.clone();
        if let Some(transport) = transport {
            options.transport = transport;
        }
        node.credentials.openvpn = Some(options);
        node.credentials.username = username.clone();
        node.credentials.password = password.clone();
        node.tls.enabled = true;
        node.tls.server_name = server_name.clone();
        node.source = NodeSource::Import;
        result.nodes.push(node);
    }
    Ok(result)
}

fn external_file() -> Error {
    Error::config("OpenVPN 引用了外部证书、密钥或凭据文件，请使用包含内嵌内容的 .ovpn 配置")
}
fn key_direction(value: &str) -> Result<String> {
    match value {
        "0" => Ok("server".into()),
        "1" => Ok("client".into()),
        _ => Err(Error::config("OpenVPN key-direction 必须为 0 或 1")),
    }
}
fn network(value: &str) -> Result<String> {
    let value = value.strip_suffix("-client").unwrap_or(value);
    if matches!(value, "udp" | "udp4" | "udp6" | "tcp" | "tcp4" | "tcp6") {
        Ok(value.into())
    } else {
        Err(Error::config("OpenVPN 仅支持 UDP / TCP 客户端传输"))
    }
}

fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            token.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                token.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '#' | ';' if token.is_empty() => break,
            c if c.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(ch),
        }
    }
    if quote.is_some() || escaped {
        return Err(Error::config("OpenVPN 配置含未闭合引号或转义字符"));
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_inline_credentials_control_keys_and_multiple_remotes() {
        let parsed = parse(
            "client\ndev tun\nproto udp\nremote vpn.example 1194\nremote backup.example 443 tcp-client\nverify-x509-name \"VPN Server\" name\nkey-direction 1\nkeepalive 10 60\nreneg-sec 0\n<ca>\n-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----\n</ca>\n<tls-auth>\nfake-control-key\n</tls-auth>\n<auth-user-pass>\nfixture-user\nfixture-password with spaces\n</auth-user-pass>\n",
        );
        assert_eq!(parsed.nodes.len(), 2, "{:?}", parsed.warnings);
        let node = &parsed.nodes[1];
        let options = node.credentials.openvpn.as_ref().unwrap();
        assert_eq!(options.transport, "tcp");
        assert_eq!(options.control_direction, "client");
        assert_eq!(options.control_mode, "tls_auth");
        assert_eq!(options.control_key, "fake-control-key\n");
        assert_eq!(options.keepalive_timeout, 60);
        assert_eq!(options.renegotiate_seconds, Some(0));
        assert_eq!(node.tls.server_name.as_deref(), Some("VPN Server"));
        assert_eq!(
            node.credentials.password.as_deref(),
            Some("fixture-password with spaces")
        );
    }

    #[test]
    fn rejects_external_files_scripts_unknown_options_and_unclosed_blocks() {
        for directive in [
            "ca /tmp/private.pem",
            "up /tmp/run.sh",
            "dev tap",
            "<key>\nsecret",
            "fake-private-material",
        ] {
            let parsed = parse(&format!("client\nremote vpn.example 1194\n{directive}\n"));
            assert!(parsed.nodes.is_empty());
            assert!(!parsed.warnings.is_empty());
            assert!(!parsed.warnings.join(" ").contains("fake-private-material"));
        }
    }

    #[test]
    fn missing_auth_is_retained_for_editing_with_warning() {
        let parsed =
            super::super::parse_subscription("client\nremote vpn.example 1194\nauth-user-pass\n");
        assert_eq!(parsed.format, SourceFormat::OpenVpn);
        assert_eq!(parsed.nodes.len(), 1);
        assert!(parsed.warnings.iter().any(|s| s.contains("补充凭据")));
    }
}
