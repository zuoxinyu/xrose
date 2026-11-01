use serde::{Deserialize, Serialize};

use crate::util::new_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InboundKind {
    #[default]
    Mixed,
    Socks,
    Http,
    Tun,
    Tproxy,
    Redirect,
}

impl InboundKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            InboundKind::Mixed => "mixed",
            InboundKind::Socks => "socks",
            InboundKind::Http => "http",
            InboundKind::Tun => "tun",
            InboundKind::Tproxy => "tproxy",
            InboundKind::Redirect => "redirect",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "mixed" => Some(InboundKind::Mixed),
            "socks" | "socks5" => Some(InboundKind::Socks),
            "http" => Some(InboundKind::Http),
            "tun" => Some(InboundKind::Tun),
            "tproxy" => Some(InboundKind::Tproxy),
            "redirect" => Some(InboundKind::Redirect),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TunStack {
    #[default]
    Mixed,
    Gvisor,
    System,
}

impl TunStack {
    pub fn as_str(&self) -> &'static str {
        match self {
            TunStack::Mixed => "mixed",
            TunStack::Gvisor => "gvisor",
            TunStack::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunOptions {
    pub interface_name: Option<String>,
    pub mtu: u32,
    pub stack: TunStack,
    pub auto_route: bool,
    pub strict_route: bool,
    pub route_exclude_addresses: Vec<String>,
    pub inet4_address: String,
    pub inet6_address: String,
    pub dns_hijack: bool,
}

impl Default for TunOptions {
    fn default() -> Self {
        Self {
            interface_name: None,
            mtu: 9000,
            stack: TunStack::Mixed,
            auto_route: true,
            strict_route: true,
            route_exclude_addresses: Vec::new(),
            inet4_address: "172.19.0.1/30".into(),
            inet6_address: "fdfe:dcba:9876::1/126".into(),
            dns_hijack: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Inbound {
    pub id: String,
    pub tag: String,
    pub kind: InboundKind,
    #[serde(default)]
    pub listen: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub allow_lan: bool,
    #[serde(default)]
    pub udp: bool,
    #[serde(default)]
    pub sniff: bool,
    #[serde(default)]
    pub tun: TunOptions,
    #[serde(default = "crate::model::default_true")]
    pub enabled: bool,
}

impl Inbound {
    pub fn new(tag: impl Into<String>, kind: InboundKind, port: u16) -> Self {
        Self {
            id: new_id("in"),
            tag: tag.into(),
            kind,
            listen: "127.0.0.1".into(),
            port,
            allow_lan: false,
            udp: true,
            sniff: true,
            tun: TunOptions::default(),
            enabled: true,
        }
    }

    pub fn listen_address(&self) -> &str {
        if self.allow_lan {
            "0.0.0.0"
        } else if self.listen.is_empty() {
            "127.0.0.1"
        } else {
            &self.listen
        }
    }
}

/// Convenience used when replacing the inbound list from the UI.
pub fn default_inbounds() -> Vec<Inbound> {
    let mut mixed = Inbound::new("mixed-in", InboundKind::Mixed, 7890);
    mixed.sniff = true;
    let mut tun = Inbound::new("tun-in", InboundKind::Tun, 0);
    tun.enabled = false;
    vec![mixed, tun]
}
