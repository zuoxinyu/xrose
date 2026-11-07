//! DNS presets and validation. DNS is the most error-prone part of a proxy
//! config, so both compile-time validation and one-click templates live here.

pub mod probe;

use serde::{Deserialize, Serialize};

use crate::model::{DnsConfig, DnsServer, DnsServerType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DnsPreset {
    /// fake-ip + split China/overseas. Best for sing-box.
    #[default]
    SplitFakeIp,
    /// Split DNS without fake-ip (works on Xray too).
    Split,
    /// Encrypted only; no split. Bypasses ISP DNS but may leak less metadata.
    Encrypted,
    /// System resolver only.
    System,
}

impl DnsPreset {
    pub fn label(&self) -> &'static str {
        match self {
            DnsPreset::SplitFakeIp => "分流 + fake-ip（推荐 sing-box）",
            DnsPreset::Split => "分流 DNS（兼容 Xray）",
            DnsPreset::Encrypted => "全加密 DNS（DoH/DoT）",
            DnsPreset::System => "系统 DNS",
        }
    }

    pub fn all() -> [DnsPreset; 4] {
        [
            DnsPreset::SplitFakeIp,
            DnsPreset::Split,
            DnsPreset::Encrypted,
            DnsPreset::System,
        ]
    }
}

/// Identify resolver policy independently from the enable switch and Hosts.
pub fn detect_preset(config: &DnsConfig) -> Option<DnsPreset> {
    DnsPreset::all().into_iter().find(|preset| {
        let mut expected = DnsConfig::default();
        apply_preset(&mut expected, *preset);
        expected.enabled = config.enabled;
        expected.hosts = config.hosts.clone();
        expected == *config
    })
}

/// Referenced resolvers must be detached before deletion.
pub fn deletion_reason(config: &DnsConfig, tag: &str) -> Option<&'static str> {
    if config.final_server.as_deref() == Some(tag) {
        Some("此服务器是默认 DNS，请先选择其他默认服务器")
    } else if config.fake_ip && config.servers.iter().any(|s| s.tag == tag && s.is_fakeip()) {
        Some("此服务器正在被 Fake-IP 使用，请先关闭 Fake-IP")
    } else {
        None
    }
}

pub fn apply_preset(config: &mut DnsConfig, preset: DnsPreset) {
    match preset {
        DnsPreset::SplitFakeIp => {
            *config = DnsConfig::default();
            config.fake_ip = true;
        }
        DnsPreset::Split => {
            *config = DnsConfig::default();
            config.fake_ip = false;
            config.servers.retain(|s| !s.is_fakeip());
            config.final_server = Some("remote".into());
        }
        DnsPreset::Encrypted => {
            *config = DnsConfig::default();
            config.fake_ip = false;
            config.servers = vec![
                DnsServer::https("remote", "1.1.1.1"),
                DnsServer::https("backup", "dns.google"),
            ];
            config.servers[0].detour = Some("proxy".into());
            config.final_server = Some("remote".into());
        }
        DnsPreset::System => {
            *config = DnsConfig::default();
            config.fake_ip = false;
            config.servers = vec![DnsServer::udp("local", "system")];
            config.final_server = Some("local".into());
        }
    }
}

/// Validation that does not require a running core.
pub fn validate(config: &DnsConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if !config.enabled {
        return issues;
    }
    let mut tags = std::collections::HashSet::new();
    for server in &config.servers {
        if server.tag.trim().is_empty() {
            issues.push("DNS 服务器缺少 tag".into());
        } else if !tags.insert(server.tag.clone()) {
            issues.push(format!("DNS 服务器 tag“{}”重复", server.tag));
        }
        if !server.is_fakeip() && server.address.trim().is_empty() {
            issues.push(format!("DNS 服务器“{}”缺少地址", server.tag));
            continue;
        }
        if !server.is_fakeip() {
            let address = server.address.trim();
            if server.server_type == DnsServerType::Udp
                && address != "system"
                && address != "local"
                && !address.contains(':')
                && address.parse::<std::net::IpAddr>().is_err()
                && !address.contains('.')
            {
                issues.push(format!(
                    "DNS 服务器“{}”的地址看起来无效：{address}",
                    server.tag
                ));
            }
        }
        for domain in &server.domains {
            if domain.trim().is_empty() {
                issues.push(format!("DNS 服务器“{}”包含空域名规则", server.tag));
            }
        }
    }
    if config.fake_ip {
        if config.fake_ip_range.trim().is_empty() {
            issues.push("fake-ip 启用了但没有设置网段".into());
        } else if !valid_ipv4_cidr(&config.fake_ip_range) {
            issues.push(format!("fake-ip 网段无效：{}", config.fake_ip_range));
        }
        if !config.servers.iter().any(|s| s.is_fakeip()) {
            // The adapters synthesize the fakeip server, so this is only a hint.
        }
    }
    if let Some(final_tag) = &config.final_server {
        if let Some(server) = config.servers.iter().find(|s| &s.tag == final_tag) {
            if server.is_fakeip() {
                issues.push("final DNS 不能是 fakeip".into());
            }
        } else if !final_tag.is_empty() {
            issues.push(format!("final DNS 服务器“{final_tag}”不存在"));
        }
    }
    for (domain, addresses) in &config.hosts {
        if domain.trim().is_empty() {
            issues.push("hosts 中存在空域名".into());
        }
        if addresses.is_empty() {
            issues.push(format!("hosts 中“{domain}”没有地址"));
        }
    }
    issues
}

fn valid_ipv4_cidr(value: &str) -> bool {
    let Some((ip, prefix)) = value.trim().split_once('/') else {
        return false;
    };
    ip.parse::<std::net::Ipv4Addr>().is_ok()
        && prefix.parse::<u8>().map(|p| p <= 32).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_presets_but_not_custom_resolvers() {
        for preset in DnsPreset::all() {
            let mut config = DnsConfig::default();
            apply_preset(&mut config, preset);
            config.enabled = false;
            config
                .hosts
                .insert("host.example".into(), vec!["192.0.2.1".into()]);
            assert_eq!(detect_preset(&config), Some(preset));
            config.servers.last_mut().unwrap().address = "192.0.2.2".into();
            assert_eq!(detect_preset(&config), None);
        }
    }

    #[test]
    fn protects_default_and_active_fakeip_resolvers() {
        let mut config = DnsConfig::default();
        assert!(deletion_reason(&config, "local").is_some());
        assert!(deletion_reason(&config, "fakeip").is_some());
        assert!(deletion_reason(&config, "remote").is_none());
        config.fake_ip = false;
        assert!(deletion_reason(&config, "fakeip").is_none());
    }

    #[test]
    fn default_config_is_valid() {
        let config = DnsConfig::default();
        assert!(validate(&config).is_empty(), "{:?}", validate(&config));
    }

    #[test]
    fn duplicate_tags_are_reported() {
        let mut config = DnsConfig::default();
        config.servers.push(DnsServer::udp("cn", "9.9.9.9"));
        assert!(validate(&config).iter().any(|i| i.contains("重复")));
    }

    #[test]
    fn bad_fakeip_range_is_reported() {
        let mut config = DnsConfig::default();
        config.fake_ip_range = "not-a-cidr".into();
        assert!(validate(&config).iter().any(|i| i.contains("fake-ip 网段")));
    }

    #[test]
    fn presets_change_transport() {
        let mut config = DnsConfig::default();
        apply_preset(&mut config, DnsPreset::System);
        assert_eq!(config.servers.len(), 1);
        assert!(!config.fake_ip);
        apply_preset(&mut config, DnsPreset::Encrypted);
        assert!(
            config
                .servers
                .iter()
                .all(|s| s.server_type == DnsServerType::Https)
        );
    }
}
