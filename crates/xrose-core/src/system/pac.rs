//! A tiny local PAC HTTP server.
//!
//! System proxy backends on Windows/macOS can point at a PAC URL; GNOME and KDE
//! support it too. The server only serves `proxy.pac` and is bound to loopback.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacServerInfo {
    pub url: String,
    pub port: u16,
}

pub struct PacServer {
    content: Arc<parking_lot::RwLock<String>>,
    original: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub info: PacServerInfo,
}

impl PacServer {
    /// Starts the server on `127.0.0.1:port` (0 picks a free port).
    pub fn start(content: String, port: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| Error::other(format!("PAC 服务启动失败：{e}")))?;
        let actual_port = listener
            .local_addr()
            .map_err(|e| Error::other(format!("PAC 服务地址获取失败：{e}")))?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let body = Arc::new(parking_lot::RwLock::new(content.clone()));
        let shared = body.clone();
        let handle = std::thread::Builder::new()
            .name("xrose-pac".into())
            .spawn(move || {
                while !stop_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stop_thread.load(Ordering::SeqCst) {
                                break;
                            }
                            let body = body.clone();
                            std::thread::spawn(move || serve(stream, &body.read()));
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| Error::other(format!("PAC 线程启动失败：{e}")))?;
        Ok(Self {
            content: shared,
            original: content,
            stop,
            handle: Some(handle),
            info: PacServerInfo {
                url: format!("http://127.0.0.1:{actual_port}/proxy.pac"),
                port: actual_port,
            },
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.info.port));
        let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(50));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    pub(crate) fn update_script(
        &mut self,
        content: String,
        plan: &crate::mesh::coexist::CoexistencePlan,
    ) {
        self.original = content;
        self.update_mesh(plan);
    }

    pub fn update_mesh(&self, plan: &crate::mesh::coexist::CoexistencePlan) {
        let domains = plan
            .dns
            .iter()
            .map(|d| d.suffix.clone())
            .collect::<Vec<_>>();
        let cidrs = plan
            .routes
            .iter()
            .map(|r| r.cidr.clone())
            .collect::<Vec<_>>();
        *self.content.write() = self.original.replace(
            "function FindProxyForURL(url, host) {",
            &format!(
                "function FindProxyForURL(url, host) {{\n{}",
                direct_rules(&domains, &cidrs)
            ),
        );
    }
}

impl Drop for PacServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(mut stream: TcpStream, body: &str) {
    let mut buffer = [0u8; 1024];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.read(&mut buffer);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Without TUN, every proxied request must enter the direct monitor for
/// statistics and firewall enforcement, including PAC bypass destinations.
pub(crate) fn use_pac(enabled: bool, mode: crate::model::WorkMode, tun_enabled: bool) -> bool {
    enabled
        && mode != crate::model::WorkMode::Mesh
        && (mode == crate::model::WorkMode::Proxy || tun_enabled)
}

/// Generates a PAC file. Direct domains are matched as suffixes; everything
/// else goes through the proxy. GeoIP routing is intentionally not attempted in
/// PAC (browsers cannot evaluate it) — use TUN or the core's routing instead.
pub fn build_pac(
    proxy_host: &str,
    proxy_port: u16,
    direct_domains: &[String],
    direct_cidrs: &[String],
) -> String {
    let direct = direct_rules(direct_domains, direct_cidrs);
    format!(
        r#"// Generated by Xrose. Do not edit.
var proxy = "PROXY {proxy_host}:{proxy_port}; DIRECT";
function FindProxyForURL(url, host) {{
  host = host.toLowerCase();
  if (isPlainHostName(host) || host === "localhost" || host === "127.0.0.1" || host === "[::1]") return "DIRECT";
  if (shExpMatch(host, "*.local") || shExpMatch(host, "*.lan") || shExpMatch(host, "*.internal")) return "DIRECT";
  if (isInNet(host, "10.0.0.0", "255.0.0.0")) return "DIRECT";
  if (isInNet(host, "172.16.0.0", "255.240.0.0")) return "DIRECT";
  if (isInNet(host, "192.168.0.0", "255.255.0.0")) return "DIRECT";
  if (isInNet(host, "127.0.0.0", "255.0.0.0")) return "DIRECT";
{direct}  return proxy;
}}
"#
    )
}

fn direct_rules(direct_domains: &[String], direct_cidrs: &[String]) -> String {
    let mut direct = String::new();
    for domain in direct_domains {
        let clean = domain
            .trim()
            .trim_start_matches("domain:")
            .trim_start_matches("full:")
            .trim_start_matches('.');
        if clean.is_empty()
            || !clean
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.'))
        {
            continue;
        }
        direct.push_str(&format!(
            "  if (host === \"{clean}\" || dnsDomainIs(host, \".{clean}\")) return \"DIRECT\";\n"
        ));
    }
    for cidr in direct_cidrs {
        if let Ok(net) = cidr.parse::<ipnet::Ipv6Net>() {
            direct.push_str(&format!(
                "  if (typeof isInNetEx === \"function\" && isInNetEx(host.replace(/^\\[|\\]$/g, \"\"), \"{net}\")) return \"DIRECT\";\n"
            ));
            continue;
        }
        if let Some((ip, prefix)) = cidr.split_once('/') {
            if let (Ok(base), Ok(prefix)) = (ip.parse::<std::net::Ipv4Addr>(), prefix.parse::<u8>())
            {
                if prefix > 32 {
                    continue;
                }
                let mask = if prefix == 0 {
                    0u32
                } else {
                    u32::MAX << (32 - prefix)
                };
                direct.push_str(&format!(
                    "  if (isInNet(host, \"{}\", \"{}\")) return \"DIRECT\";\n",
                    base,
                    std::net::Ipv4Addr::from(mask)
                ));
            }
        }
    }
    direct
}

#[cfg(test)]
mod tests {
    #[test]
    fn direct_capture_uses_a_manual_proxy_without_tun() {
        use crate::model::WorkMode;
        assert!(!super::use_pac(true, WorkMode::Direct, false));
        assert!(super::use_pac(true, WorkMode::Direct, true));
        assert!(super::use_pac(true, WorkMode::Proxy, false));
        assert!(!super::use_pac(false, WorkMode::Proxy, true));
    }

    use super::*;

    #[test]
    fn mesh_rules_are_replaced_and_removed_without_losing_user_rules() {
        use crate::mesh::coexist::{CoexistencePlan, MeshDns, MeshRoute};
        let original = build_pac("127.0.0.1", 7890, &["example.com".into()], &[]);
        let server = PacServer::start(original.clone(), 0).unwrap();
        let plan = CoexistencePlan {
            active: true,
            dns: vec![MeshDns {
                suffix: "demo.ts.net".into(),
                address: "127.0.0.153".into(),
            }],
            routes: vec![MeshRoute {
                cidr: "fd7a:115c:a1e0::/48".into(),
                interface: "mesh-test".into(),
            }],
        };
        server.update_mesh(&plan);
        let first = server.content.read().clone();
        assert!(first.contains("demo.ts.net"));
        assert!(first.contains("isInNetEx"));
        assert!(first.contains("example.com"));
        server.update_mesh(&plan);
        assert_eq!(*server.content.read(), first);
        server.update_mesh(&CoexistencePlan::default());
        assert_eq!(
            *server.content.read(),
            original.replace(
                "function FindProxyForURL(url, host) {",
                "function FindProxyForURL(url, host) {\n"
            )
        );
    }

    #[test]
    fn pac_contains_proxy_and_direct_rules() {
        let pac = build_pac(
            "127.0.0.1",
            7890,
            &["example.com".into()],
            &["192.168.0.0/16".into()],
        );
        assert!(pac.contains("PROXY 127.0.0.1:7890"));
        assert!(pac.contains("example.com"));
        assert!(pac.contains("isInNet"));
        assert!(pac.contains("FindProxyForURL"));
    }

    #[test]
    fn server_serves_pac() {
        let pac = build_pac("127.0.0.1", 7890, &[], &[]);
        let server = PacServer::start(pac, 0).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", server.info.port)).unwrap();
        stream
            .write_all(b"GET /proxy.pac HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("FindProxyForURL"));
        assert!(server.info.url.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn stop_wakes_immediately_without_poll_delay() {
        let pac = build_pac("127.0.0.1", 7890, &[], &[]);
        let mut server = PacServer::start(pac, 0).unwrap();
        let started = std::time::Instant::now();
        server.stop();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "PAC server stop took {elapsed:?}"
        );
    }
}
