//! Read-only route discovery and journaled split DNS on Unix platforms.
use super::NetworkBaseline;
use crate::{Error, Result};
use std::{path::Path, process::Command};

fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| Error::other(format!("无法运行 {program}，请安装对应网络工具")))?;
    if !output.status.success() {
        return Err(Error::other(format!(
            "{program} 网络操作失败，请检查权限和系统网络服务"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "linux")]
pub fn baseline_with_proxy_tun(addresses: &[String]) -> Result<NetworkBaseline> {
    let mut result = NetworkBaseline {
        proxy_tun_addresses: addresses.to_vec(),
        ..Default::default()
    };
    let links: Vec<serde_json::Value> =
        serde_json::from_str(&run("ip", &["-j", "address", "show"])?)?;
    for link in links {
        if link["ifname"] == "lo" {
            continue;
        }
        if let Some(items) = link["addr_info"].as_array() {
            for item in items {
                if let (Some(ip), Some(prefix)) =
                    (item["local"].as_str(), item["prefixlen"].as_u64())
                {
                    result.protected.push(format!("{ip}/{prefix}"));
                }
            }
        }
    }
    for family in ["-4", "-6"] {
        let routes: Vec<serde_json::Value> =
            serde_json::from_str(&run("ip", &["-j", family, "route", "show", "default"])?)?;
        for route in routes {
            result
                .defaults
                .push(format!("{family}|{}|{}", route["gateway"], route["dev"]));
        }
    }
    result.defaults.sort();
    Ok(result)
}

#[cfg(target_os = "macos")]
pub fn baseline_with_proxy_tun(addresses: &[String]) -> Result<NetworkBaseline> {
    let mut result = NetworkBaseline {
        proxy_tun_addresses: addresses.to_vec(),
        ..Default::default()
    };
    let interfaces = run("/sbin/ifconfig", &["-a"])?;
    for line in interfaces.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if let Some(address) = mac_ipv4_address(line) {
            result.protected.push(address);
        } else if fields.first() == Some(&"inet6") {
            if let Some(index) = fields.iter().position(|v| *v == "prefixlen") {
                if let Some(prefix) = fields.get(index + 1) {
                    result.protected.push(format!(
                        "{}/{}",
                        fields[1].split('%').next().unwrap_or(""),
                        prefix
                    ));
                }
            }
        }
    }
    for family in ["inet", "inet6"] {
        let routes = run("/usr/sbin/netstat", &["-rn", "-f", family])?;
        for line in routes.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.first() == Some(&"default") && fields.len() >= 4 {
                // Scoped defaults of unrelated VPN interfaces are not global defaults.
                if !fields[2].contains('I') {
                    result
                        .defaults
                        .push(format!("{family}|{}|{}", fields[1], fields[3]));
                }
            }
        }
    }
    result.defaults.sort();
    Ok(result)
}

#[cfg(any(target_os = "macos", test))]
fn mac_ipv4_address(line: &str) -> Option<String> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.first() != Some(&"inet") {
        return None;
    }
    let ip = fields.get(1)?.parse::<std::net::Ipv4Addr>().ok()?;
    if ip.is_loopback() {
        return None;
    }
    // Point-to-point interfaces include "--> peer" before the netmask.
    let mask = fields.get(fields.iter().position(|v| *v == "netmask")? + 1)?;
    let mask = u32::from_str_radix(mask.trim_start_matches("0x"), 16).ok()?;
    let prefix = mask.leading_ones();
    if mask.count_ones() != prefix {
        return None;
    }
    Some(format!("{ip}/{prefix}"))
}

#[cfg(target_os = "linux")]
pub fn interface_routes(interface: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for family in ["-4", "-6"] {
        let routes: Vec<serde_json::Value> = serde_json::from_str(&run(
            "ip",
            &[
                "-j", family, "route", "show", "table", "all", "dev", interface,
            ],
        )?)?;
        for route in routes {
            if let Some(dst) = route["dst"].as_str() {
                result.push(if dst == "default" {
                    if family == "-4" { "0.0.0.0/0" } else { "::/0" }.into()
                } else if dst.contains('/') {
                    dst.into()
                } else {
                    format!("{dst}/{}", if family == "-4" { 32 } else { 128 })
                });
            }
        }
    }
    Ok(result)
}

#[cfg(target_os = "macos")]
pub fn interface_routes(interface: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for family in ["inet", "inet6"] {
        let routes = run("/usr/sbin/netstat", &["-rn", "-f", family])?;
        for line in routes.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 4 && fields[3] == interface {
                if let Some(net) = mac_route(fields[0]) {
                    result.push(net);
                }
            }
        }
    }
    Ok(result)
}

#[cfg(any(target_os = "macos", test))]
fn mac_route(value: &str) -> Option<String> {
    if value == "default" {
        return Some("0.0.0.0/0".into());
    }
    let (ip, prefix) = value
        .split_once('/')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let ip = ip.split('%').next()?;
    if ip.contains(':') {
        return Some(format!("{ip}/{}", prefix.unwrap_or("128")));
    }
    let mut parts = ip.split('.').collect::<Vec<_>>();
    let inferred = (parts.len() * 8).to_string();
    while parts.len() < 4 {
        parts.push("0");
    }
    let result = format!("{}/{}", parts.join("."), prefix.unwrap_or(&inferred));
    result
        .parse::<ipnet::IpNet>()
        .ok()
        .map(|net| net.to_string())
}

pub fn set_dns(root: &Path, id: &str, suffix: &str, address: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let path = Path::new("/etc/resolver").join(suffix);
        let content = format!("# Xross mesh {id}\nnameserver {address}\n");
        crate::util::atomic_write_json(&root.join("dns-unix.json"), &(suffix, address))?;
        std::fs::create_dir_all("/etc/resolver")?;
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(content.as_bytes())?,
            Err(_) if std::fs::read_to_string(&path).ok().as_deref() == Some(&content) => {}
            Err(_) => {
                return Err(Error::other(
                    "该 DNS 后缀已由其他程序管理，请检查 /etc/resolver",
                ));
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Detect unavailable resolved before journaling an interface mutation,
        // so missing DNS integration can degrade to IP/proxy access.
        run("resolvectl", &["status"])?;
        let profile = super::super::MeshProfile {
            id: id.into(),
            provider: super::super::MeshProvider::Tailscale,
            ..Default::default()
        };
        let interface = profile.interface_name();
        crate::util::atomic_write_json(&root.join("dns-unix.json"), &(suffix, address))?;
        run("resolvectl", &["default-route", &interface, "no"])?;
        run("resolvectl", &["domain", &interface, &format!("~{suffix}")])?;
        run("resolvectl", &["dns", &interface, address])?;
    }
    Ok(())
}

pub fn cleanup_dns(root: &Path, id: &str) -> Result<()> {
    let journal = root.join("dns-unix.json");
    if !journal.exists() {
        return Ok(());
    }
    let (suffix, address): (String, String) = serde_json::from_slice(&std::fs::read(&journal)?)?;
    if !super::super::coexist::valid_suffix(&suffix)
        || !address
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return Err(Error::other("DNS 恢复记录无效"));
    }
    #[cfg(target_os = "macos")]
    {
        let path = Path::new("/etc/resolver").join(&suffix);
        if std::fs::read_to_string(&path).ok().as_deref()
            == Some(&format!("# Xross mesh {id}\nnameserver {address}\n"))
        {
            std::fs::remove_file(path)?;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let interface = super::super::MeshProfile {
            id: id.into(),
            provider: super::super::MeshProvider::Tailscale,
            ..Default::default()
        }
        .interface_name();
        if Path::new("/sys/class/net").join(&interface).exists() {
            run("resolvectl", &["revert", &interface])?;
        }
    }
    std::fs::remove_file(journal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_both_broadcast_and_point_to_point_ipv4_addresses() {
        assert_eq!(
            mac_ipv4_address("inet 192.168.1.5 netmask 0xffffff00 broadcast 192.168.1.255"),
            Some("192.168.1.5/24".into())
        );
        assert_eq!(
            mac_ipv4_address("inet 100.64.1.2 --> 100.64.1.2 netmask 0xffffffff"),
            Some("100.64.1.2/32".into())
        );
        assert!(mac_ipv4_address("inet 127.0.0.1 netmask 0xff000000").is_none());
        assert!(mac_ipv4_address("inet 10.0.0.1 netmask 0xff00ff00").is_none());
    }
    #[test]
    fn parses_bsd_compact_and_scoped_routes() {
        assert_eq!(mac_route("10.126.126"), Some("10.126.126.0/24".into()));
        assert_eq!(mac_route("100.64/10"), Some("100.64.0.0/10".into()));
        assert_eq!(mac_route("fe80::%utun123/64"), Some("fe80::/64".into()));
        assert_eq!(mac_route("default"), Some("0.0.0.0/0".into()));
        assert!(mac_route("garbage").is_none());
    }
}
