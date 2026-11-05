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

    pub(crate) fn listen_ports(&self, core: super::CoreKind) -> crate::Result<Vec<u16>> {
        match self.kind {
            InboundKind::Tun => Ok(Vec::new()),
            InboundKind::Tproxy | InboundKind::Redirect if !cfg!(target_os = "linux") => {
                Ok(Vec::new())
            }
            InboundKind::Mixed if core == super::CoreKind::Xray => {
                let http = self.port.checked_add(1).ok_or_else(|| {
                    crate::Error::config("Xray 混合端口需小于 65535，以便分配 HTTP 端口")
                })?;
                Ok(vec![self.port, http])
            }
            _ => Ok(vec![self.port]),
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

/// Expand the optional LAN listener without modifying the saved profile.
pub(crate) fn effective_inbounds(
    model: &super::AppModel,
    settings: &crate::settings::Settings,
) -> Vec<Inbound> {
    let mut inbounds: Vec<_> = model
        .inbounds
        .iter()
        .filter(|i| i.enabled)
        .cloned()
        .collect();
    if settings.allow_lan && settings.separate_lan_port {
        if let Some(primary) = inbounds.iter_mut().find(|i| i.kind == InboundKind::Mixed) {
            let mut lan = primary.clone();
            lan.tag = format!("{}-lan", primary.tag);
            lan.allow_lan = true;
            lan.port = settings.lan_port;
            primary.allow_lan = false;
            primary.listen = "127.0.0.1".into();
            inbounds.push(lan);
        }
    }
    inbounds
}

/// Check the ports reserved by both the core and the enabled client services.
pub(crate) fn validate_listeners(
    inbounds: &[Inbound],
    core: super::CoreKind,
    settings: &crate::settings::Settings,
) -> crate::Result<()> {
    let mut ports = Vec::<(String, u16)>::new();
    let mut reserve = |host: &str, port| {
        let overlap = |other: &str| {
            host == other
                || host == "::"
                || other == "::"
                || (host == "0.0.0.0" && !other.contains(':'))
                || (other == "0.0.0.0" && !host.contains(':'))
        };
        if port == 0 || ports.iter().any(|(h, p)| *p == port && overlap(h)) {
            return Err(crate::Error::config(format!(
                "端口 {port} 无效或重复，请修改代理或管理端口"
            )));
        }
        ports.push((host.into(), port));
        Ok(())
    };
    reserve(
        "127.0.0.1",
        if core == super::CoreKind::Xray {
            settings.stats_port
        } else {
            settings.api_port
        },
    )?;
    if settings.pac_enabled {
        reserve("127.0.0.1", settings.pac_port)?;
    }
    for inbound in inbounds {
        if inbound.allow_lan && settings.lan_auth && !settings.lan_account.valid() {
            return Err(crate::Error::config("请输入有效的局域网用户名和密码"));
        }
        for port in inbound.listen_ports(core)? {
            reserve(inbound.listen_address(), port)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppModel, Settings, model::CoreKind};

    #[test]
    fn lan_port_must_leave_room_for_the_selected_cores_listeners() {
        let model = AppModel::default();
        let mut s = Settings {
            allow_lan: true,
            separate_lan_port: true,
            ..Default::default()
        };
        let valid = |s: &Settings, core| {
            validate_listeners(&effective_inbounds(&model, s), core, s).is_ok()
        };
        for port in [s.mixed_port, s.mixed_port + 1, s.stats_port - 1, u16::MAX] {
            s.lan_port = port;
            assert!(!valid(&s, CoreKind::Xray));
        }
        s.lan_port = s.mixed_port + 1;
        assert!(valid(&s, CoreKind::SingBox));
        s.pac_enabled = true;
        assert!(!valid(&s, CoreKind::SingBox));
        s.allow_lan = false;
        assert!(valid(&s, CoreKind::SingBox));
    }
}
