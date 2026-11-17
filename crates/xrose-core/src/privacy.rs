//! Shared privacy-mode masking for IPs, endpoints, CIDRs, and MagicDNS domains.
use std::net::{IpAddr, SocketAddr};

/// Mask an IPv4 or IPv6 address while preserving the leading segment (`172.*.*.*` / `2001:*:*:*`).
pub fn mask_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let [a, ..] = v4.octets();
            format!("{a}.*.*.*")
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return format!("::ffff:{}", mask_ip(IpAddr::V4(v4)));
            }
            if v6.is_unspecified() || v6.is_loopback() {
                return "::*".to_string();
            }
            let [a, ..] = v6.segments();
            format!("{a:x}:*:*:*")
        }
    }
}

/// Mask the IP portion of a socket address while retaining its port (`172.*.*.*:443`, `[2001:*:*:*]:443`).
pub fn mask_socket_addr(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(v4) => format!("{}:{}", mask_ip(IpAddr::V4(*v4.ip())), v4.port()),
        SocketAddr::V6(v6) => format!("[{}]:{}", mask_ip(IpAddr::V6(*v6.ip())), v6.port()),
    }
}

/// Format an `ip` + `port` pair, masking the IP when `hide` is true while keeping `:port`.
pub fn format_ip_port(ip: &str, port: &str, hide: bool) -> String {
    let raw_ip = ip.trim();
    if raw_ip.is_empty() {
        return if port.is_empty() {
            String::new()
        } else {
            format!(":{port}")
        };
    }

    let clean = raw_ip.trim_matches(['[', ']']);
    let addr_only = clean.split('%').next().unwrap_or("");
    if hide {
        if let Ok(parsed) = addr_only.parse::<IpAddr>() {
            let masked = mask_ip(parsed);
            if port.is_empty() {
                masked
            } else if parsed.is_ipv6() {
                format!("[{masked}]:{port}")
            } else {
                format!("{masked}:{port}")
            }
        } else if port.is_empty() {
            "•••".to_string()
        } else {
            format!("•••:{port}")
        }
    } else if port.is_empty() {
        raw_ip.to_string()
    } else if clean.contains(':') {
        format!("[{clean}]:{port}")
    } else {
        format!("{clean}:{port}")
    }
}

/// Mask a bare IP, `ip/prefix` CIDR, or socket address while keeping its prefix length or port.
pub fn mask_ip_or_cidr(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "•••".to_string();
    }
    if let Ok(addr) = trimmed.parse::<SocketAddr>() {
        return mask_socket_addr(addr);
    }
    if let Some((ip_part, prefix)) = trimmed.split_once('/') {
        let prefix = prefix.trim();
        let ip_clean = ip_part
            .trim()
            .trim_matches(['[', ']'])
            .split('%')
            .next()
            .unwrap_or("");
        if !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_digit())
            && let Ok(ip) = ip_clean.parse::<IpAddr>()
        {
            return format!("{}/{prefix}", mask_ip(ip));
        }
    }
    let bracketed = trimmed.starts_with('[') && trimmed.ends_with(']');
    let inner = trimmed
        .trim_matches(['[', ']'])
        .split('%')
        .next()
        .unwrap_or("");
    if let Ok(ip) = inner.parse::<IpAddr>() {
        let masked = mask_ip(ip);
        return if bracketed {
            format!("[{masked}]")
        } else {
            masked
        };
    }
    "•••".to_string()
}

/// Mask the private identifier labels of a MagicDNS domain suffix (e.g. `alpha-bravo.ts.net` -> `***.ts.net`).
pub fn mask_magic_dns_suffix(suffix: &str) -> String {
    let trimmed = suffix.trim().trim_end_matches('.');
    let (wildcard, core) = match trimmed.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    if core.is_empty() {
        return if wildcard {
            "*.***".to_string()
        } else {
            "***".to_string()
        };
    }

    let labels: Vec<&str> = core.split('.').collect();
    let lower = core.to_ascii_lowercase();
    let keep = if lower.ends_with(".beta.tailscale.net") && labels.len() > 3 {
        3
    } else if (lower.ends_with(".ts.net") || lower.ends_with(".tailscale.net")) && labels.len() > 2
    {
        2
    } else if let Some(sfx) = psl::suffix(lower.as_bytes()).filter(|s| s.is_known()) {
        let sfx_labels = sfx.as_bytes().split(|&b| b == b'.').count();
        if sfx_labels < labels.len() {
            sfx_labels
        } else {
            1
        }
    } else {
        1
    };

    let mask_count = if labels.len() <= keep {
        labels.len()
    } else {
        labels.len() - keep
    };
    let masked_labels: Vec<&str> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| if i < mask_count { "***" } else { *label })
        .collect();
    let masked_core = masked_labels.join(".");
    if wildcard {
        format!("*.{masked_core}")
    } else {
        masked_core
    }
}

/// Mask an IP/CIDR/socket string, `MagicDNS · <suffix>` label, or host matching a MagicDNS suffix.
pub fn mask_magic_dns_name(value: &str, suffixes: &[&str]) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.parse::<SocketAddr>().is_ok() {
        return mask_ip_or_cidr(trimmed);
    }
    if let Some((ip_part, prefix)) = trimmed.split_once('/') {
        let prefix = prefix.trim();
        let ip_clean = ip_part
            .trim()
            .trim_matches(['[', ']'])
            .split('%')
            .next()
            .unwrap_or("");
        if !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_digit())
            && ip_clean.parse::<IpAddr>().is_ok()
        {
            return mask_ip_or_cidr(trimmed);
        }
    }
    if trimmed
        .trim_matches(['[', ']'])
        .split('%')
        .next()
        .is_some_and(|ip| ip.parse::<IpAddr>().is_ok())
    {
        return mask_ip_or_cidr(trimmed);
    }
    if let Some(rest) = trimmed.strip_prefix("MagicDNS · ") {
        return format!("MagicDNS · {}", mask_magic_dns_suffix(rest));
    }

    let clean = trimmed.trim_end_matches('.');
    let mut normalized_suffixes: Vec<&str> = suffixes
        .iter()
        .map(|s| s.trim().trim_start_matches("*.").trim_end_matches('.'))
        .filter(|s| !s.is_empty())
        .collect();
    normalized_suffixes.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for sfx in normalized_suffixes {
        let masked_sfx = mask_magic_dns_suffix(sfx);
        if clean.eq_ignore_ascii_case(sfx) {
            return masked_sfx;
        }
        if let Some(pos) = clean.len().checked_sub(sfx.len() + 1)
            && clean.is_char_boundary(pos)
            && clean.as_bytes()[pos] == b'.'
            && clean[pos + 1..].eq_ignore_ascii_case(sfx)
        {
            let prefix = &clean[..pos];
            return format!("{prefix}.{masked_sfx}");
        }
    }

    let lower = clean.to_ascii_lowercase();
    let tail = if lower.ends_with(".beta.tailscale.net") {
        Some(3usize)
    } else if lower.ends_with(".ts.net") || lower.ends_with(".tailscale.net") {
        Some(2usize)
    } else {
        None
    };
    if let Some(tail) = tail {
        let mut labels: Vec<&str> = clean.split('.').collect();
        if labels.len() >= tail + 1 {
            let idx = labels.len() - tail - 1;
            labels[idx] = "***";
            return labels.join(".");
        }
    }

    value.to_string()
}

/// Format and optionally mask a proxy endpoint (`host:port`, `[ipv6]:port`, or bare host/IP).
pub fn mask_proxy_endpoint(endpoint: &str, hide: bool) -> String {
    if !hide {
        let count = endpoint.chars().count();
        return if count > 32 {
            let start: String = endpoint.chars().take(14).collect();
            let end: String = endpoint.chars().skip(count - 12).collect();
            format!("{start}…{end}")
        } else {
            endpoint.to_string()
        };
    }

    if let Some(rest) = endpoint.strip_prefix('[')
        && let Some((host, port)) = rest.rsplit_once("]:")
    {
        let clean = host.split('%').next().unwrap_or("");
        if let Ok(ip) = clean.parse::<IpAddr>() {
            return format!("[{}]:{port}", mask_ip(ip));
        }
        return format!("••••••:{port}");
    }

    let trimmed = endpoint.trim();
    let bracketed = trimmed.starts_with('[') && trimmed.ends_with(']');
    if let Ok(ip) = trimmed
        .trim_matches(['[', ']'])
        .split('%')
        .next()
        .unwrap_or("")
        .parse::<IpAddr>()
    {
        let masked = mask_ip(ip);
        return if bracketed {
            format!("[{masked}]")
        } else {
            masked
        };
    }

    if let Some((host, port)) = endpoint.rsplit_once(':')
        && !host.contains(':')
    {
        if let Ok(ip) = host.trim().parse::<IpAddr>() {
            return format!("{}:{port}", mask_ip(ip));
        }
        return format!("••••••:{port}");
    }

    "••••••••".to_string()
}

/// Mask a label that may match a MagicDNS name or contain a node's server IP/domain.
pub fn mask_server_in_label(
    label: &str,
    server: &str,
    domain_mask: &str,
    suffixes: &[&str],
) -> String {
    let mut out = mask_magic_dns_name(label, suffixes);
    let clean = server.trim().trim_matches(['[', ']']);
    if clean.is_empty() {
        return out;
    }
    if let Ok(ip) = clean.split('%').next().unwrap_or("").parse::<IpAddr>() {
        let masked = mask_ip(ip);
        out = out.replace(&format!("[{clean}]"), &format!("[{masked}]"));
        out = out.replace(clean, &masked);
    } else {
        let masked_srv = mask_magic_dns_name(clean, suffixes);
        let replacement = if masked_srv != clean {
            masked_srv.as_str()
        } else {
            domain_mask
        };
        out = out.replace(clean, replacement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_ipv4_and_ipv6_uniformly() {
        assert_eq!(mask_ip("172.16.0.1".parse().unwrap()), "172.*.*.*");
        assert_eq!(mask_ip("10.0.0.2".parse().unwrap()), "10.*.*.*");
        assert_eq!(mask_ip("100.64.0.10".parse().unwrap()), "100.*.*.*");
        assert_eq!(mask_ip("127.0.0.1".parse().unwrap()), "127.*.*.*");
        assert_eq!(mask_ip("2001:db8:1234::1".parse().unwrap()), "2001:*:*:*");
        assert_eq!(mask_ip("fd7a:115c::1".parse().unwrap()), "fd7a:*:*:*");
        assert_eq!(
            mask_ip("::ffff:192.0.2.5".parse().unwrap()),
            "::ffff:192.*.*.*"
        );
        assert_eq!(mask_ip("::1".parse().unwrap()), "::*");
        assert_eq!(mask_ip("::".parse().unwrap()), "::*");
    }

    #[test]
    fn masks_sockets_cidrs_and_ip_ports() {
        assert_eq!(
            mask_socket_addr("172.16.0.1:30319".parse().unwrap()),
            "172.*.*.*:30319"
        );
        assert_eq!(
            mask_socket_addr("[2001:db8::1]:443".parse().unwrap()),
            "[2001:*:*:*]:443"
        );
        assert_eq!(mask_ip_or_cidr("10.0.0.0/24"), "10.*.*.*/24");
        assert_eq!(mask_ip_or_cidr("10.0.0.2/32"), "10.*.*.*/32");
        assert_eq!(mask_ip_or_cidr("2001:db8::/64"), "2001:*:*:*/64");
        assert_eq!(mask_ip_or_cidr("2001:db8::1"), "2001:*:*:*");
        assert_eq!(mask_ip_or_cidr("[2001:db8::1]"), "[2001:*:*:*]");
        assert_eq!(mask_ip_or_cidr("not-an-ip"), "•••");

        assert_eq!(format_ip_port("", "443", true), ":443");
        assert_eq!(
            format_ip_port("172.16.0.1", "30736", true),
            "172.*.*.*:30736"
        );
        assert_eq!(
            format_ip_port("2001:db8::1", "443", true),
            "[2001:*:*:*]:443"
        );
        assert_eq!(format_ip_port("192.0.2.5", "443", false), "192.0.2.5:443");
    }

    #[test]
    fn masks_magic_dns_suffixes_and_hostnames() {
        assert_eq!(mask_magic_dns_suffix("alpha-bravo.ts.net"), "***.ts.net");
        assert_eq!(
            mask_magic_dns_suffix("*.alpha-bravo.ts.net"),
            "*.***.ts.net"
        );
        assert_eq!(
            mask_magic_dns_suffix("alpha-bravo.beta.tailscale.net"),
            "***.beta.tailscale.net"
        );
        assert_eq!(mask_magic_dns_suffix("vpn.example.com"), "***.***.com");
        assert_eq!(mask_magic_dns_suffix("vpn.example.co.uk"), "***.***.co.uk");
        assert_eq!(mask_magic_dns_suffix("mesh.internal"), "***.internal");
        assert_eq!(mask_magic_dns_suffix("tailnet"), "***");

        assert_eq!(
            mask_magic_dns_name("desktop.alpha-bravo.ts.net", &["alpha-bravo.ts.net"]),
            "desktop.***.ts.net"
        );
        assert_eq!(
            mask_magic_dns_name("*.alpha-bravo.ts.net", &["alpha-bravo.ts.net"]),
            "*.***.ts.net"
        );
        assert_eq!(
            mask_magic_dns_name("alpha-bravo.ts.net", &["alpha-bravo.ts.net"]),
            "***.ts.net"
        );
        assert_eq!(
            mask_magic_dns_name("desktop.vpn.example.com", &["vpn.example.com"]),
            "desktop.***.***.com"
        );
        assert_eq!(
            mask_magic_dns_name("desktop.alpha-bravo.ts.net", &[]),
            "desktop.***.ts.net"
        );
        assert_eq!(
            mask_magic_dns_name("MagicDNS · alpha-bravo.ts.net", &[]),
            "MagicDNS · ***.ts.net"
        );
        assert_eq!(mask_magic_dns_name("192.0.2.6", &[]), "192.*.*.*");
        assert_eq!(
            mask_magic_dns_name("outlook.office365.com", &["alpha-bravo.ts.net"]),
            "outlook.office365.com"
        );
        assert_eq!(
            mask_magic_dns_name("xrose-notebook", &["alpha-bravo.ts.net"]),
            "xrose-notebook"
        );
    }

    #[test]
    fn masks_proxy_endpoints_and_node_labels() {
        assert_eq!(
            mask_proxy_endpoint("172.16.0.1:30319", true),
            "172.*.*.*:30319"
        );
        assert_eq!(
            mask_proxy_endpoint("[2001:db8::1]:443", true),
            "[2001:*:*:*]:443"
        );
        assert_eq!(mask_proxy_endpoint("a.b.c.com:443", true), "••••••:443");
        assert_eq!(
            mask_proxy_endpoint("node.example:1080", true),
            "••••••:1080"
        );
        assert_eq!(mask_proxy_endpoint("2001:db8::1", true), "2001:*:*:*");
        assert_eq!(mask_proxy_endpoint("direct", true), "••••••••");
        assert_eq!(
            mask_proxy_endpoint("very-long-domain-name.example.com:8443", false),
            "very-long-doma…ple.com:8443"
        );

        assert_eq!(
            mask_server_in_label("SOCKS 192.0.2.5", "192.0.2.5", "••••••", &[]),
            "SOCKS 192.*.*.*"
        );
        assert_eq!(
            mask_server_in_label("SOCKS [2001:db8::1]", "2001:db8::1", "••••••", &[]),
            "SOCKS [2001:*:*:*]"
        );
        assert_eq!(
            mask_server_in_label("SOCKS node.example", "node.example", "••••••", &[]),
            "SOCKS ••••••"
        );
        assert_eq!(
            mask_server_in_label(
                "desktop.alpha-bravo.ts.net",
                "100.64.0.1",
                "•••",
                &["alpha-bravo.ts.net"]
            ),
            "desktop.***.ts.net"
        );
    }
}
