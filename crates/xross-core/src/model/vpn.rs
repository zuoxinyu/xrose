//! Client VPN intent and certificate material, stored with encrypted node credentials.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VpnCertificates {
    pub authority_pem: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub private_key_password: String,
    pub peer_fingerprints: Vec<String>,
}

impl VpnCertificates {
    pub fn validate(&self) -> Result<()> {
        if self.certificate_pem.is_empty() != self.private_key_pem.is_empty() {
            return Err(Error::config("客户端证书和私钥必须同时填写"));
        }
        for pem in [&self.authority_pem, &self.certificate_pem] {
            if !pem.is_empty() && !pem.contains("-----BEGIN CERTIFICATE-----") {
                return Err(Error::config("CA 和客户端证书必须填写 PEM 内容"));
            }
        }
        if !self.private_key_pem.is_empty() && !self.private_key_pem.contains("PRIVATE KEY-----") {
            return Err(Error::config("客户端私钥必须填写 PEM 内容"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenVpnOptions {
    pub certificates: VpnCertificates,
    pub use_pushed_dns: bool,
    pub transport: String,
    pub mtu: u16,
    pub data_ciphers: Vec<String>,
    pub fallback_cipher: String,
    pub auth_digest: String,
    pub control_mode: String,
    pub control_key: String,
    pub control_direction: String,
    pub certificate_name_match: String,
    pub certificate_purpose: String,
    pub compression: String,
    pub compression_lzo: String,
    pub allow_compression: String,
    pub keepalive_interval: u32,
    pub keepalive_timeout: u32,
    pub renegotiate_seconds: Option<u32>,
}

impl Default for OpenVpnOptions {
    fn default() -> Self {
        Self {
            certificates: VpnCertificates::default(),
            use_pushed_dns: true,
            transport: "udp".into(),
            mtu: 0,
            data_ciphers: Vec::new(),
            fallback_cipher: String::new(),
            auth_digest: String::new(),
            control_mode: String::new(),
            control_key: String::new(),
            control_direction: String::new(),
            certificate_name_match: String::new(),
            certificate_purpose: "server".into(),
            compression: String::new(),
            compression_lzo: String::new(),
            allow_compression: String::new(),
            keepalive_interval: 0,
            keepalive_timeout: 0,
            renegotiate_seconds: None,
        }
    }
}

impl OpenVpnOptions {
    pub fn validate(&self) -> Result<()> {
        self.certificates.validate()?;
        validate_mtu(self.mtu)?;
        if !matches!(
            self.transport.as_str(),
            "udp" | "udp4" | "udp6" | "tcp" | "tcp4" | "tcp6"
        ) {
            return Err(Error::config(
                "OpenVPN 传输必须为 UDP 或 TCP（可指定 IPv4 / IPv6）",
            ));
        }
        if !matches!(
            self.control_mode.as_str(),
            "" | "tls_auth" | "tls_crypt" | "tls_crypt_v2"
        ) || self.control_mode.is_empty() != self.control_key.is_empty()
        {
            return Err(Error::config(
                "OpenVPN 控制通道模式和密钥需同时填写，支持 tls_auth / tls_crypt / tls_crypt_v2",
            ));
        }
        if !matches!(self.control_direction.as_str(), "" | "client" | "server") {
            return Err(Error::config(
                "OpenVPN 密钥方向必须为 client、server 或留空",
            ));
        }
        if !matches!(
            self.certificate_name_match.as_str(),
            "" | "name" | "subject" | "name-prefix"
        ) || !matches!(
            self.certificate_purpose.as_str(),
            "server" | "client" | "none"
        ) {
            return Err(Error::config("OpenVPN 证书匹配方式或用途无效"));
        }
        if !self.certificates.private_key_password.is_empty()
            || self.certificates.private_key_pem.contains("ENCRYPTED")
        {
            return Err(Error::config(
                "当前 OpenVPN 内核接口不支持加密的客户端私钥，请先解锁私钥",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenConnectOptions {
    pub certificates: VpnCertificates,
    pub use_pushed_dns: bool,
    pub flavor: String,
    pub path: String,
    pub auth_group: String,
    pub cookie: String,
    pub disable_udp: bool,
    pub mtu: u16,
    pub user_agent: String,
    pub reported_os: String,
}

impl Default for OpenConnectOptions {
    fn default() -> Self {
        Self {
            certificates: VpnCertificates::default(),
            use_pushed_dns: true,
            flavor: "anyconnect".into(),
            path: "/".into(),
            auth_group: String::new(),
            cookie: String::new(),
            disable_udp: false,
            mtu: 0,
            user_agent: String::new(),
            reported_os: String::new(),
        }
    }
}

impl OpenConnectOptions {
    pub fn validate(&self) -> Result<()> {
        self.certificates.validate()?;
        validate_mtu(self.mtu)?;
        if !matches!(
            self.flavor.as_str(),
            "anyconnect" | "gp" | "fortinet" | "f5" | "pulse" | "nc"
        ) {
            return Err(Error::config(
                "OpenConnect 类型必须为 anyconnect / gp / fortinet / f5 / pulse / nc",
            ));
        }
        if !self.path.starts_with('/')
            || self.path.contains(['?', '#'])
            || self.path.chars().any(char::is_whitespace)
        {
            return Err(Error::config(
                "OpenConnect 路径必须以 / 开头，不能含查询参数、片段或空格",
            ));
        }
        Ok(())
    }
}

fn validate_mtu(mtu: u16) -> Result<()> {
    if mtu != 0 && mtu < 576 {
        return Err(Error::config("VPN MTU 必须为 0（自动）或 576–65535"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CoreKind, NodeDraft, Protocol, TransportKind};

    #[test]
    fn vpn_draft_checks_authentication_certificates_and_transport() {
        let mut draft = NodeDraft {
            protocol: Protocol::Openconnect,
            server: "vpn.example".into(),
            ..Default::default()
        };
        assert!(
            draft
                .into_node(CoreKind::SingBox)
                .unwrap_err()
                .to_string()
                .contains("凭据")
                || draft
                    .into_node(CoreKind::SingBox)
                    .unwrap_err()
                    .to_string()
                    .contains("账号密码")
        );
        draft.username = "fixture".into();
        draft.password = "fixture-pass".into();
        assert!(draft.into_node(CoreKind::SingBox).is_ok());
        assert!(draft.into_node(CoreKind::Xray).is_err());
        draft.openconnect.certificates.certificate_pem =
            "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----".into();
        assert!(draft.into_node(CoreKind::SingBox).is_err());
        draft.openconnect.certificates.private_key_pem =
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nfake\n-----END ENCRYPTED PRIVATE KEY-----"
                .into();
        draft.openconnect.certificates.private_key_password = "fake-private-password".into();
        let node = draft.into_node(CoreKind::SingBox).unwrap();
        assert_eq!(NodeDraft::from(&node).openconnect, draft.openconnect);
        draft.transport = TransportKind::WebSocket;
        assert!(draft.into_node(CoreKind::SingBox).is_err());
        draft.transport = TransportKind::Tcp;
        draft.protocol = Protocol::Openvpn;
        draft.openvpn.certificates = draft.openconnect.certificates.clone();
        let error = draft.into_node(CoreKind::SingBox).unwrap_err().to_string();
        assert!(error.contains("解锁私钥"));
        assert!(!error.contains("fake-private-password"));
    }
}
