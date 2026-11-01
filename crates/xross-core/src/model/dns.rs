use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DnsServerType {
    #[default]
    Udp,
    Tcp,
    Tls,
    Https,
    Quic,
    H3,
    FakeIp,
}

impl DnsServerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DnsServerType::Udp => "udp",
            DnsServerType::Tcp => "tcp",
            DnsServerType::Tls => "tls",
            DnsServerType::Https => "https",
            DnsServerType::Quic => "quic",
            DnsServerType::H3 => "h3",
            DnsServerType::FakeIp => "fakeip",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "udp" => Some(DnsServerType::Udp),
            "tcp" => Some(DnsServerType::Tcp),
            "tls" | "dot" => Some(DnsServerType::Tls),
            "https" | "doh" => Some(DnsServerType::Https),
            "quic" | "doq" => Some(DnsServerType::Quic),
            "h3" => Some(DnsServerType::H3),
            "fakeip" | "fake-ip" => Some(DnsServerType::FakeIp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DnsServer {
    pub tag: String,
    #[serde(default)]
    pub server_type: DnsServerType,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub detour: Option<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub expect_ips: Vec<String>,
    #[serde(default)]
    pub client_subnet: Option<String>,
}

impl DnsServer {
    pub fn udp(tag: &str, address: &str) -> Self {
        Self {
            tag: tag.into(),
            server_type: DnsServerType::Udp,
            address: address.into(),
            domains: Vec::new(),
            detour: None,
            strategy: None,
            expect_ips: Vec::new(),
            client_subnet: None,
        }
    }
    pub fn https(tag: &str, address: &str) -> Self {
        Self {
            server_type: DnsServerType::Https,
            ..Self::udp(tag, address)
        }
    }
    pub fn display_address(&self) -> String {
        match self.server_type {
            DnsServerType::Udp | DnsServerType::Tcp => self.address.clone(),
            DnsServerType::Tls => format!("tls://{}", self.address),
            DnsServerType::Https => {
                if self.address.starts_with("https://") {
                    self.address.clone()
                } else {
                    format!("https://{}/dns-query", self.address)
                }
            }
            DnsServerType::Quic => format!("quic://{}", self.address),
            DnsServerType::H3 => format!("h3://{}", self.address),
            DnsServerType::FakeIp => "fakeip".into(),
        }
    }
    pub fn is_fakeip(&self) -> bool {
        self.server_type == DnsServerType::FakeIp || self.address.trim() == "fakeip"
    }
}

pub fn default_fakeip_range() -> String {
    "198.18.0.0/15".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DnsConfig {
    #[serde(default = "crate::model::default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub fake_ip: bool,
    #[serde(default = "default_fakeip_range")]
    pub fake_ip_range: String,
    #[serde(default)]
    pub fake_ip_filter: Vec<String>,
    #[serde(default)]
    pub servers: Vec<DnsServer>,
    #[serde(default)]
    pub hosts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub final_server: Option<String>,
    #[serde(default)]
    pub independent_cache: bool,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fake_ip: true,
            fake_ip_range: default_fakeip_range(),
            fake_ip_filter: vec!["*.lan".into(), "*.local".into(), "localhost".into()],
            servers: default_servers(),
            hosts: BTreeMap::new(),
            final_server: Some("local".into()),
            independent_cache: true,
        }
    }
}

pub fn default_servers() -> Vec<DnsServer> {
    let mut cn = DnsServer::udp("cn", "223.5.5.5");
    cn.domains = vec!["geosite:cn".into()];
    cn.detour = Some("direct".into());

    let mut remote = DnsServer::https("remote", "1.1.1.1");
    remote.domains = vec!["geosite:geolocation-!cn".into()];
    remote.detour = Some("proxy".into());

    let mut local = DnsServer::udp("local", "system");
    local.domains = Vec::new();

    let fakeip = DnsServer {
        tag: "fakeip".into(),
        server_type: DnsServerType::FakeIp,
        address: "fakeip".into(),
        domains: Vec::new(),
        detour: None,
        strategy: None,
        expect_ips: Vec::new(),
        client_subnet: None,
    };
    vec![fakeip, cn, remote, local]
}

/// Synthesizes a fresh DNS id (used by the UI when adding a server).
pub fn new_server_id() -> String {
    new_id("dns")
}
