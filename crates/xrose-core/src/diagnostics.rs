//! Connection filtering, bounded diagnostic recordings and exact rule targets.

use std::collections::BTreeSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Matchers;
use crate::runtime::ClashConnection;
use crate::trace::TraceSession;

/// Matches a connection against a whitespace-tokenized omni search query across
/// domain, IP, port, protocol, process, rule, and outbound chain metadata.
pub fn connection_matches_query(connection: &ClashConnection, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let m = &connection.metadata;
    let host_port = if m.host.is_empty() || m.destination_port.is_empty() {
        None
    } else {
        Some(format!("{}:{}", m.host, m.destination_port).to_lowercase())
    };
    let sniff_port = if m.sniff_host.is_empty() || m.destination_port.is_empty() {
        None
    } else {
        Some(format!("{}:{}", m.sniff_host, m.destination_port).to_lowercase())
    };
    let dest_port = if m.destination_ip.is_empty() || m.destination_port.is_empty() {
        None
    } else {
        Some(format!("{}:{}", m.destination_ip, m.destination_port).to_lowercase())
    };
    let fields = [
        m.host.to_lowercase(),
        m.sniff_host.to_lowercase(),
        m.source_ip.to_lowercase(),
        m.destination_ip.to_lowercase(),
        m.source_port.to_lowercase(),
        m.destination_port.to_lowercase(),
        m.process.to_lowercase(),
        m.process_path.to_lowercase(),
        m.network.to_lowercase(),
        m.kind.to_lowercase(),
        connection.rule.to_lowercase(),
        connection.rule_payload.to_lowercase(),
    ];
    let chains: Vec<String> = connection.chains.iter().map(|c| c.to_lowercase()).collect();

    query.split_whitespace().all(|token| {
        fields.iter().any(|field| field.contains(token))
            || chains.iter().any(|chain| chain.contains(token))
            || token.strip_prefix(':').is_some_and(|port| {
                !port.is_empty() && m.destination_port.to_lowercase().contains(port)
            })
            || [
                host_port.as_deref(),
                sniff_port.as_deref(),
                dest_port.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|endpoint| endpoint.contains(token))
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiagnosticRuleKind {
    #[default]
    Domain,
    Ip,
    Process,
}

#[derive(Debug, Clone, Default)]
pub struct DiagnosticTarget {
    pub domain: String,
    pub ip: String,
    pub process: String,
}

impl DiagnosticTarget {
    pub fn from_connection(connection: &ClashConnection) -> Self {
        let host = connection.host();
        Self {
            domain: if host.parse::<IpAddr>().is_err() {
                host
            } else {
                String::new()
            },
            ip: connection.metadata.destination_ip.clone(),
            process: connection.process_name(),
        }
    }

    pub fn from_trace(trace: &TraceSession) -> Self {
        let host_ip = trace.host.parse::<IpAddr>().ok();
        let domain = if host_ip.is_none() {
            trace.host.clone()
        } else {
            trace
                .sni
                .as_deref()
                .filter(|sni| !sni.is_empty() && sni.parse::<IpAddr>().is_err())
                .unwrap_or_default()
                .to_string()
        };
        Self {
            domain,
            ip: host_ip
                .map(|ip| ip.to_string())
                .or_else(|| trace.resolved_ips.first().cloned())
                .unwrap_or_default(),
            process: String::new(),
        }
    }

    pub fn value(&self, kind: DiagnosticRuleKind) -> &str {
        match kind {
            DiagnosticRuleKind::Domain => &self.domain,
            DiagnosticRuleKind::Ip => &self.ip,
            DiagnosticRuleKind::Process => &self.process,
        }
    }

    pub fn matchers(&self, kind: DiagnosticRuleKind) -> Result<Matchers> {
        let value = self.value(kind).trim();
        if value.is_empty() {
            return Err(Error::config(
                "该连接缺少此匹配条件，请选择域名、IP 或进程中的其他项",
            ));
        }
        let mut matchers = Matchers::default();
        match kind {
            DiagnosticRuleKind::Domain => {
                if value.parse::<IpAddr>().is_ok() || value.contains(['/', ':', ' ']) {
                    return Err(Error::config("域名无效，请使用完整域名"));
                }
                matchers.domains.push(value.to_lowercase());
            }
            DiagnosticRuleKind::Ip => {
                let ip: IpAddr = value
                    .parse()
                    .map_err(|_| Error::config("缺少有效的目标 IP"))?;
                matchers
                    .ip_cidrs
                    .push(format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 }));
            }
            DiagnosticRuleKind::Process => matchers.process_names.push(value.to_string()),
        }
        Ok(matchers)
    }
}

/// Returns true for Fake-IP ranges configured by Xrose (`198.18.0.0/15`, `fc00::/18`).
pub fn is_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            a == 198 && (b == 18 || b == 19)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            segments[0] == 0xfc00 && (segments[1] & 0xc000) == 0
        }
    }
}

/// Ports that overwhelmingly speak TLS and benefit from an end-to-end TLS/HTTPS
/// handshake instead of a bare SOCKS5 TCP probe.
pub fn is_tls_port(port: u16) -> bool {
    matches!(
        port,
        443 | 465 | 853 | 993 | 995 | 2053 | 2083 | 2087 | 2096 | 8443
    )
}

pub(crate) fn format_url_host(host: &str) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

pub fn connection_domain_test_target(connection: &ClashConnection) -> Option<String> {
    let domain = connection.host();
    if domain.is_empty() || domain.parse::<IpAddr>().is_ok() {
        return None;
    }
    let port = connection
        .metadata
        .destination_port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)?;
    if connection.metadata.network.eq_ignore_ascii_case("udp") {
        return Some(format!("udp://{domain}:{port}"));
    }
    Some(match port {
        443 => format!("https://{domain}"),
        80 => format!("http://{domain}"),
        _ if is_tls_port(port) || !connection.metadata.sniff_host.is_empty() => {
            format!("tls://{domain}:{port}")
        }
        _ => format!("tcp://{domain}:{port}"),
    })
}

pub fn connection_ip_test_target(connection: &ClashConnection) -> Option<String> {
    let ip: IpAddr = connection
        .metadata
        .destination_ip
        .trim()
        .trim_matches(['[', ']'])
        .parse()
        .ok()?;
    if is_fake_ip(ip) {
        return None;
    }
    let port = connection
        .metadata
        .destination_port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)?;
    let formatted_ip = format_url_host(&ip.to_string());
    if connection.metadata.network.eq_ignore_ascii_case("udp") {
        return Some(format!("udp://{formatted_ip}:{port}"));
    }
    let host = connection.host();
    let sni = (!host.is_empty() && host.parse::<IpAddr>().is_err()).then_some(host);
    Some(match (port, sni) {
        (80, _) => format!("tcp://{formatted_ip}:80"),
        (_, Some(sni)) => format!("tls://{formatted_ip}:{port}?sni={sni}"),
        (p, None) if is_tls_port(p) => format!("tls://{formatted_ip}:{p}"),
        (_, None) => format!("tcp://{formatted_ip}:{port}"),
    })
}

pub fn connection_test_target(connection: &ClashConnection) -> Option<String> {
    let host = connection.host();
    if host.is_empty() {
        return None;
    }
    let port = connection
        .metadata
        .destination_port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)?;
    if connection.metadata.network.eq_ignore_ascii_case("udp") {
        let host = format_url_host(&host);
        return Some(format!("udp://{host}:{port}"));
    }
    if host.parse::<IpAddr>().is_err() {
        let non_standard_tun_ip = port != 443
            && port != 80
            && (!connection.metadata.sniff_host.is_empty()
                || connection
                    .metadata
                    .kind
                    .to_ascii_lowercase()
                    .contains("tun"));
        if non_standard_tun_ip && let Some(ip_target) = connection_ip_test_target(connection) {
            return Some(ip_target);
        }
        return connection_domain_test_target(connection);
    }
    let host = format_url_host(&host);
    if is_tls_port(port) {
        Some(format!("tls://{host}:{port}"))
    } else {
        Some(format!("tcp://{host}:{port}"))
    }
}

pub const RECORDING_CONNECTION_LIMIT: usize = 2000;
pub const RECORDING_TRACE_LIMIT: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedConnection {
    pub connection: ClashConnection,
    pub first_seen: i64,
    pub last_seen: i64,
    pub closed_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticRecording {
    pub active: bool,
    pub started_at: Option<i64>,
    pub stopped_at: Option<i64>,
    pub connections: Vec<RecordedConnection>,
    pub traces: Vec<TraceSession>,
    pub evicted_connections: u64,
    pub evicted_traces: u64,
}

impl DiagnosticRecording {
    pub fn start(&mut self, now: i64) {
        *self = Self {
            active: true,
            started_at: Some(now),
            ..Self::default()
        };
    }

    pub fn stop(&mut self, now: i64) {
        self.active = false;
        self.stopped_at = Some(now);
    }

    pub fn observe(&mut self, connections: &[ClashConnection], now: i64) {
        if !self.active {
            return;
        }
        let present: BTreeSet<_> = connections.iter().map(|c| (&c.id, &c.start)).collect();
        for entry in &mut self.connections {
            if entry.closed_at.is_none()
                && !present.contains(&(&entry.connection.id, &entry.connection.start))
            {
                entry.closed_at = Some(now);
            }
        }
        for connection in connections {
            if let Some(entry) = self.connections.iter_mut().rev().find(|entry| {
                entry.connection.id == connection.id && entry.connection.start == connection.start
            }) {
                entry.connection = connection.clone();
                entry.last_seen = now;
                entry.closed_at = None;
            } else {
                self.connections.push(RecordedConnection {
                    connection: connection.clone(),
                    first_seen: now,
                    last_seen: now,
                    closed_at: None,
                });
            }
        }
        if self.connections.len() > RECORDING_CONNECTION_LIMIT {
            let excess = self.connections.len() - RECORDING_CONNECTION_LIMIT;
            self.connections.drain(..excess);
            self.evicted_connections += excess as u64;
        }
    }

    pub fn observe_trace(&mut self, trace: &TraceSession) {
        if !self.active || trace.finished_at.is_none() {
            return;
        }
        if let Some(entry) = self.traces.iter_mut().find(|entry| entry.id == trace.id) {
            *entry = trace.clone();
        } else {
            self.traces.push(trace.clone());
            if self.traces.len() > RECORDING_TRACE_LIMIT {
                self.traces.remove(0);
                self.evicted_traces += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(id: &str) -> ClashConnection {
        let mut c = ClashConnection::default();
        c.id = id.into();
        c.metadata.host = "Example.com".into();
        c.metadata.sniff_host = "cdn.example.com".into();
        c.metadata.destination_ip = "2001:db8::1".into();
        c.metadata.destination_port = "443".into();
        c.metadata.process_path = "C:\\Apps\\Browser.EXE".into();
        c
    }

    #[test]
    fn omni_search_matches_domains_ips_ports_processes_and_rules() {
        let mut c = connection("a");
        c.metadata.network = "tcp".into();
        c.metadata.kind = "tun/tun-in".into();
        c.rule = "RuleSet".into();
        c.rule_payload = "geosite-cn".into();
        c.chains = vec!["ProxyGroup".into(), "HK-01".into()];

        assert!(connection_matches_query(&c, ""));
        assert!(connection_matches_query(&c, " CDN.EXAMPLE "));
        assert!(connection_matches_query(&c, "browser.exe"));
        assert!(connection_matches_query(&c, "db8"));
        assert!(connection_matches_query(&c, ":443"));
        assert!(connection_matches_query(&c, "example.com:443"));
        assert!(connection_matches_query(&c, "browser hk-01 443"));
        assert!(connection_matches_query(&c, "geosite-cn tcp"));
        assert!(!connection_matches_query(&c, "browser udp"));
    }

    #[test]
    fn recording_updates_closes_bounds_and_freezes() {
        let mut r = DiagnosticRecording::default();
        r.start(1);
        let mut c = connection("a");
        r.observe(&[c.clone()], 2);
        c.download = 100;
        r.observe(&[c], 3);
        assert_eq!(r.connections.len(), 1);
        assert_eq!(r.connections[0].connection.download, 100);
        r.observe(&[], 4);
        assert_eq!(r.connections[0].closed_at, Some(4));
        r.stop(5);
        r.observe(&[connection("b")], 6);
        assert_eq!(r.connections.len(), 1);
        r.start(7);
        let many: Vec<_> = (0..2005).map(|i| connection(&i.to_string())).collect();
        r.observe(&many, 8);
        assert_eq!(r.connections.len(), RECORDING_CONNECTION_LIMIT);
        assert_eq!(r.evicted_connections, 5);
    }

    #[test]
    fn replay_uses_the_destination_protocol_host_and_port() {
        let mut c = connection("fixture");
        c.metadata.network = "UDP".into();
        assert_eq!(
            connection_test_target(&c).as_deref(),
            Some("udp://Example.com:443")
        );
        c.metadata.host.clear();
        assert_eq!(
            connection_test_target(&c).as_deref(),
            Some("udp://cdn.example.com:443")
        );
        for port in ["", "0", "65536"] {
            c.metadata.destination_port = port.into();
            assert!(connection_test_target(&c).is_none());
        }
    }

    #[test]
    fn exact_rules_and_ipv6_test_target() {
        let mut c = connection("a");
        let target = DiagnosticTarget::from_connection(&c);
        assert_eq!(
            target.matchers(DiagnosticRuleKind::Domain).unwrap().domains,
            ["example.com"]
        );
        assert_eq!(
            target.matchers(DiagnosticRuleKind::Ip).unwrap().ip_cidrs,
            ["2001:db8::1/128"]
        );
        assert_eq!(
            target
                .matchers(DiagnosticRuleKind::Process)
                .unwrap()
                .process_names,
            ["Browser.EXE"]
        );
        // Domain on 443 replays via HTTPS; original IP target preserves SNI over TLS.
        assert_eq!(
            connection_test_target(&c).as_deref(),
            Some("https://Example.com")
        );
        assert_eq!(
            connection_ip_test_target(&c).as_deref(),
            Some("tls://[2001:db8::1]:443?sni=Example.com")
        );
        // Non-standard port with sniffed SNI replays original IP + SNI directly.
        c.metadata.destination_port = "2084".into();
        c.metadata.kind = "tun/tun-in".into();
        assert_eq!(
            connection_test_target(&c).as_deref(),
            Some("tls://[2001:db8::1]:2084?sni=Example.com")
        );
        assert_eq!(
            connection_domain_test_target(&c).as_deref(),
            Some("tls://Example.com:2084")
        );
        // FakeIP destination is never offered as an original-IP replay target.
        c.metadata.destination_ip = "198.18.0.42".into();
        assert!(connection_ip_test_target(&c).is_none());
        assert!(is_fake_ip("198.19.255.1".parse().unwrap()));
        assert!(is_fake_ip("fc00::1234".parse().unwrap()));
        assert!(!is_fake_ip("2001:db8::1".parse().unwrap()));

        // IP-only connections use tls:// on 443 and tcp:// on plain ports.
        c.metadata.destination_ip = "2001:db8::1".into();
        c.metadata.destination_port = "443".into();
        c.metadata.host.clear();
        c.metadata.sniff_host.clear();
        assert_eq!(
            connection_test_target(&c).unwrap(),
            "tls://[2001:db8::1]:443"
        );
        c.metadata.destination_port = "8080".into();
        assert_eq!(
            connection_test_target(&c).unwrap(),
            "tcp://[2001:db8::1]:8080"
        );
        assert!(
            DiagnosticTarget::from_connection(&c)
                .matchers(DiagnosticRuleKind::Domain)
                .is_err()
        );

        let tls_target =
            crate::trace::parse_target("tls://[2001:db8::1]:2084?sni=www.example.com").unwrap();
        let trace = TraceSession::new(1, &tls_target);
        let from_trace = DiagnosticTarget::from_trace(&trace);
        assert_eq!(from_trace.domain, "www.example.com");
        assert_eq!(from_trace.ip, "2001:db8::1");
    }
}
