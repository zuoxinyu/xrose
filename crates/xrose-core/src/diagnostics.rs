//! Connection filtering, bounded diagnostic recordings and exact rule targets.

use std::collections::BTreeSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Matchers;
use crate::runtime::ClashConnection;
use crate::trace::TraceSession;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionFilterKind {
    #[default]
    All,
    Domain,
    Ip,
    Process,
}

impl ConnectionFilterKind {
    pub fn matches(self, connection: &ClashConnection, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        let m = &connection.metadata;
        let contains = |value: &str| value.to_lowercase().contains(&query);
        let domain = contains(&m.host) || contains(&m.sniff_host);
        let ip = contains(&m.source_ip) || contains(&m.destination_ip);
        let process = contains(&m.process) || contains(&m.process_path);
        query.is_empty()
            || match self {
                Self::All => domain || ip || process,
                Self::Domain => domain,
                Self::Ip => ip,
                Self::Process => process,
            }
    }
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
        Self {
            domain: if host_ip.is_none() {
                trace.host.clone()
            } else {
                String::new()
            },
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
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    Some(format!(
        "{}://{host}:{port}",
        if connection.metadata.network.eq_ignore_ascii_case("udp") {
            "udp"
        } else {
            "tcp"
        }
    ))
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
    fn filters_scope_case_and_sniffed_domains() {
        let c = connection("a");
        assert!(ConnectionFilterKind::Domain.matches(&c, " CDN.EXAMPLE "));
        assert!(!ConnectionFilterKind::Domain.matches(&c, "Browser"));
        assert!(ConnectionFilterKind::Process.matches(&c, "browser.exe"));
        assert!(ConnectionFilterKind::Ip.matches(&c, "db8"));
        assert!(ConnectionFilterKind::All.matches(&c, "Browser"));
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
        c.metadata.host.clear();
        c.metadata.sniff_host.clear();
        assert_eq!(
            connection_test_target(&c).unwrap(),
            "tcp://[2001:db8::1]:443"
        );
        assert!(
            DiagnosticTarget::from_connection(&c)
                .matchers(DiagnosticRuleKind::Domain)
                .is_err()
        );
    }
}
