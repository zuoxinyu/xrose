//! Pure route planning. Only adapters translate this into proxy wire formats.
use super::{MeshPhase, MeshRuntimeView};
use crate::{Error, Result};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshRoute {
    pub cidr: String,
    pub interface: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshDns {
    pub suffix: String,
    pub address: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoexistencePlan {
    pub active: bool,
    pub routes: Vec<MeshRoute>,
    pub dns: Vec<MeshDns>,
}

pub fn safe_network(value: &str) -> Result<IpNet> {
    let n = value
        .parse::<IpNet>()
        .map_err(|_| Error::config("组网网段必须使用 CIDR 格式"))?;
    if n.prefix_len() < if n.addr().is_ipv4() { 8 } else { 32 }
        || n.addr().is_unspecified()
        || n.addr().is_loopback()
        || n.addr().is_multicast()
    {
        return Err(Error::config("组网不允许默认路由、回环或组播网段"));
    }
    Ok(n.trunc())
}
pub fn overlaps(a: &IpNet, b: &IpNet) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}
pub fn check_networks(values: &[String], protected: &[String]) -> Result<()> {
    for value in values {
        let n = safe_network(value)?;
        for other in protected {
            if let Ok(p) = other.parse::<IpNet>() {
                if p.prefix_len() != 0 && overlaps(&n, &p) {
                    return Err(Error::config(format!(
                        "组网网段 {n} 与现有网段 {p} 冲突，请更换虚拟网段或移除远端网段"
                    )));
                }
            }
        }
    }
    Ok(())
}

pub fn check_startup_networks(
    provider: super::MeshProvider,
    values: &[String],
    protected: &[String],
) -> Result<()> {
    if provider == super::MeshProvider::Tailscale {
        let spaces: [IpNet; 2] = [
            "100.64.0.0/10".parse().unwrap(),
            "fd7a:115c:a1e0::/48".parse().unwrap(),
        ];
        if protected
            .iter()
            .filter_map(|v| v.parse::<IpNet>().ok())
            .any(|p| p.prefix_len() != 0 && spaces.iter().any(|space| overlaps(space, &p)))
        {
            return Err(Error::config(
                "系统已有网卡占用 Tailscale 网段；请先断开独立 Tailscale 客户端或使用同网段的 VPN，再连接 Xross",
            ));
        }
    }
    check_networks(values, protected)
}
pub fn plan(networks: &[MeshRuntimeView], protected: &[String]) -> Result<CoexistencePlan> {
    let mut result = CoexistencePlan::default();
    result.active = networks.iter().any(|n| {
        matches!(
            n.phase,
            MeshPhase::Starting | MeshPhase::NeedsLogin | MeshPhase::Connected
        )
    });
    let mut used = protected.to_vec();
    for n in networks.iter().filter(|n| n.phase == MeshPhase::Connected) {
        check_networks(&n.routes, &used)?;
        for cidr in &n.routes {
            let cidr = safe_network(cidr)?.to_string();
            if !result
                .routes
                .iter()
                .any(|r| r.cidr == cidr && r.interface == n.interface)
            {
                result.routes.push(MeshRoute {
                    cidr,
                    interface: n.interface.clone(),
                });
            }
        }
        used.extend(n.routes.iter().cloned());
        if let (Some(suffix), Some(address)) = (&n.dns_suffix, &n.dns_address) {
            if !valid_suffix(suffix)
                || !address
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            {
                return Err(Error::config("组网返回的 DNS 配置无效"));
            }
            if result.dns.iter().any(|d| d.suffix == *suffix) {
                return Err(Error::config("组网 DNS 后缀冲突"));
            }
            result.dns.push(MeshDns {
                suffix: suffix.clone(),
                address: address.clone(),
            });
        }
    }
    // Host routes already covered by an interface's subnet must not cause a
    // proxy restart every time a peer joins or leaves.
    let all = result.routes.clone();
    result.routes.retain(|route| {
        let net = route.cidr.parse::<IpNet>().expect("validated route");
        !all.iter().any(|other| {
            other.interface == route.interface
                && other
                    .cidr
                    .parse::<IpNet>()
                    .is_ok_and(|n| n.prefix_len() < net.prefix_len() && n.contains(&net.network()))
        })
    });
    result.routes.sort_by(|a, b| a.cidr.cmp(&b.cidr));
    result.dns.sort_by(|a, b| a.suffix.cmp(&b.suffix));
    Ok(result)
}
pub fn valid_suffix(s: &str) -> bool {
    s.len() <= 253
        && s.contains('.')
        && s != "ts.net"
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tailscale_conflicts_explain_how_to_release_the_network() {
        for address in ["100.64.1.2/32", "fd7a:115c:a1e0::1234/128"] {
            let error = check_startup_networks(
                super::super::MeshProvider::Tailscale,
                &[],
                &[address.into()],
            )
            .unwrap_err();
            assert!(error.to_string().contains("独立 Tailscale 客户端"));
        }
        assert!(
            check_startup_networks(
                super::super::MeshProvider::Tailscale,
                &["192.168.1.0/24".into()],
                &["192.168.1.5/24".into()]
            )
            .is_err()
        );
        assert!(
            check_startup_networks(
                super::super::MeshProvider::EasyTier,
                &["10.126.126.0/24".into()],
                &["100.64.1.2/32".into()]
            )
            .is_ok()
        );
    }
    #[test]
    fn tailscale_internal_addresses_do_not_overlap_default_proxy_tun() {
        let tun = crate::model::TunOptions::default();
        assert!(
            check_networks(
                &[
                    super::super::TAILSCALE_TUN_NETWORK.into(),
                    super::super::TAILSCALE_TUN_IPV6_NETWORK.into()
                ],
                &[tun.inet4_address, tun.inet6_address],
            )
            .is_ok()
        );
    }
    #[test]
    fn protects_lan_and_both_ip_families() {
        assert!(check_networks(&["10.10.0.0/16".into()], &["10.10.10.0/24".into()]).is_err());
        assert!(check_networks(&["fd00::/64".into()], &["fd00::1/128".into()]).is_err());
        assert!(check_networks(&["10.126.126.0/24".into()], &["10.10.10.0/24".into()]).is_ok());
        assert!(safe_network("::/0").is_err());
    }
    #[test]
    fn rejects_cross_provider_overlap() {
        let a = MeshRuntimeView {
            phase: MeshPhase::Connected,
            routes: vec!["100.64.0.0/10".into()],
            ..Default::default()
        };
        let b = MeshRuntimeView {
            phase: MeshPhase::Connected,
            routes: vec!["100.80.0.0/16".into()],
            ..Default::default()
        };
        assert!(plan(&[a, b], &[]).is_err());
    }
    #[test]
    fn dns_is_scoped() {
        assert!(valid_suffix("example.ts.net"));
        assert!(!valid_suffix("ts.net"));
        assert!(!valid_suffix("a';evil.ts.net"));
    }

    #[test]
    fn joining_peer_does_not_change_an_existing_subnet_plan() {
        let mut network = MeshRuntimeView {
            phase: MeshPhase::Connected,
            interface: "mesh-test".into(),
            routes: vec!["100.64.0.0/10".into()],
            ..Default::default()
        };
        let original = plan(&[network.clone()], &[]).unwrap();
        network
            .routes
            .extend(["100.80.1.2/32".into(), "100.90.1.3/32".into()]);
        assert_eq!(plan(&[network], &[]).unwrap(), original);
    }
}
