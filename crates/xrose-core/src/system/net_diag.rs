//! On-demand host diagnostics. Keep original and privacy-safe presentations
//! separate at collection time so arbitrary OS strings cannot enter a private report.
use super::{
    port_owner, privilege,
    proxy::{ProxyBackend, ProxyManager},
};
use crate::{
    AppModel, Settings,
    mesh::MeshSnapshot,
    model::{CoreKind, inbound::effective_inbounds},
    settings::Language,
};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr, TcpListener, UdpSocket};
use std::process::Command;

fn tr(lang: Language, zh: &'static str, en: &'static str) -> &'static str {
    if lang == Language::En { en } else { zh }
}

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub label: String,
    full: String,
    private: String,
}

impl ReportRow {
    pub(crate) fn new(label: &str, full: String, private: String) -> Self {
        Self {
            label: label.into(),
            full,
            private,
        }
    }
    pub(crate) fn plain(label: &str, detail: String) -> Self {
        Self::new(label, detail.clone(), detail)
    }
    pub fn detail(&self, privacy: bool) -> &str {
        if privacy { &self.private } else { &self.full }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NetworkReport {
    pub rows: Vec<ReportRow>,
}

impl NetworkReport {
    pub fn text(&self, private: bool) -> String {
        let mut lines = vec!["Xrose".to_owned()];
        lines.extend(
            self.rows
                .iter()
                .map(|r| format!("{} · {}", r.label, r.detail(private))),
        );
        lines.join("\n")
    }
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(windows)]
fn interface_mtus() -> BTreeMap<u32, u32> {
    let cmd = "Get-NetIPInterface | ForEach-Object { '{0} {1}' -f $_.InterfaceIndex,$_.NlMtu }";
    output(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", cmd],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|s| {
        let mut parts = s.split_whitespace();
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    })
    .collect()
}

#[cfg(target_os = "macos")]
fn mtu(name: &str) -> Option<u32> {
    let text = output("/sbin/ifconfig", &[name])?;
    let mut parts = text.lines().next()?.split_whitespace();
    parts.find(|s| *s == "mtu")?;
    parts.next()?.parse().ok()
}

#[cfg(target_os = "linux")]
fn mtu(name: &str) -> Option<u32> {
    std::fs::read_to_string(
        std::path::Path::new("/sys/class/net")
            .join(name)
            .join("mtu"),
    )
    .ok()?
    .trim()
    .parse()
    .ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn mtu(_: &str) -> Option<u32> {
    None
}

// Route tables are authoritative even when the default path is a point-to-point
// tunnel with no gateway address attached to the enumerated interface.
#[cfg(target_os = "macos")]
fn route_table() -> BTreeMap<String, Vec<String>> {
    let mut routes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (family, flag) in [("IPv4", "-inet"), ("IPv6", "-inet6")] {
        if let Some(text) = output("/sbin/route", &["-n", "get", flag, "default"]) {
            let fields: BTreeMap<_, _> = text
                .lines()
                .filter_map(|line| line.trim().split_once(": "))
                .collect();
            if let Some(interface) = fields.get("interface") {
                let gateway = fields.get("gateway").copied().unwrap_or("—");
                routes
                    .entry((*interface).into())
                    .or_default()
                    .push(format!("{family} {gateway}"));
            }
        }
    }
    routes
}

#[cfg(target_os = "linux")]
fn route_table() -> BTreeMap<String, Vec<String>> {
    let mut routes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (family, flag) in [("IPv4", "-4"), ("IPv6", "-6")] {
        if let Some(text) = output("ip", &[flag, "-j", "route", "show", "default"])
            && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&text)
        {
            for entry in entries {
                if let Some(interface) = entry["dev"].as_str() {
                    let gateway = entry["gateway"].as_str().unwrap_or("—");
                    routes
                        .entry(interface.into())
                        .or_default()
                        .push(format!("{family} {gateway}"));
                }
            }
        }
    }
    routes
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn route_table() -> BTreeMap<String, Vec<String>> {
    BTreeMap::new()
}

fn network_rows(lang: Language, mesh: &MeshSnapshot) -> Vec<ReportRow> {
    let t = |zh, en| tr(lang, zh, en);
    let mut rows = Vec::new();
    let mut dns: BTreeSet<IpAddr> = BTreeSet::new();
    #[cfg(windows)]
    let mtus = interface_mtus();
    let routes = route_table();
    let mut have_default_route = false;
    for iface in netdev::get_interfaces().into_iter().filter(|i| i.is_up()) {
        let name = iface.friendly_name.as_deref().unwrap_or(&iface.name);
        let addresses: Vec<_> = iface
            .ipv4
            .iter()
            .map(ToString::to_string)
            .chain(iface.ipv6.iter().map(ToString::to_string))
            .collect();
        dns.extend(iface.dns_servers.iter().copied());
        let kind = if let Some(network) = mesh
            .networks
            .iter()
            .find(|n| n.interface == name || n.interface == iface.name)
        {
            network.provider.label().into()
        } else if iface.is_tun() {
            "TUN".into()
        } else {
            iface.if_type.name()
        };
        #[cfg(windows)]
        let mtu = mtus.get(&iface.index).copied();
        #[cfg(not(windows))]
        let mtu = mtu(&iface.name);
        let mtu = mtu.map_or_else(|| "—".into(), |n| n.to_string());
        let safe = format!(
            "#{} · {kind} · MTU {mtu} · IPv4 × {} / IPv6 × {}",
            iface.index,
            iface.ipv4.len(),
            iface.ipv6.len()
        );
        rows.push(ReportRow::new(
            t("网络接口", "Interface"),
            format!("{name} · {kind} · MTU {mtu} · {}", addresses.join(", ")),
            safe,
        ));
        let route = routes.get(&iface.name);
        if route.is_some() || (routes.is_empty() && iface.default) {
            let gateways = route.cloned().unwrap_or_else(|| {
                iface
                    .gateway
                    .iter()
                    .flat_map(|g| {
                        g.ipv4
                            .iter()
                            .map(ToString::to_string)
                            .chain(g.ipv6.iter().map(ToString::to_string))
                    })
                    .collect::<Vec<_>>()
            });
            rows.push(ReportRow::new(
                t("默认路由", "Default route"),
                format!("{name} · {}", gateways.join(", ")),
                format!("#{} · {kind}", iface.index),
            ));
            have_default_route = true;
        }
    }
    if !have_default_route {
        rows.push(ReportRow::plain(
            t("默认路由", "Default route"),
            t("未读取到默认路由", "Default route unavailable").into(),
        ));
    }
    #[cfg(target_os = "macos")]
    if let Some(resolvers) = output("/usr/sbin/scutil", &["--dns"]) {
        dns.extend(
            resolvers
                .lines()
                .filter(|l| l.trim().starts_with("nameserver["))
                .filter_map(|l| {
                    l.split_once(" : ")?
                        .1
                        .trim()
                        .split('%')
                        .next()?
                        .parse::<IpAddr>()
                        .ok()
                }),
        );
    }
    #[cfg(target_os = "linux")]
    if let Some(resolvers) = output("resolvectl", &["dns"]) {
        dns.extend(
            resolvers
                .lines()
                .filter_map(|l| l.split_once(": ").map(|(_, v)| v))
                .flat_map(str::split_whitespace)
                .filter_map(|v| v.split('%').next()?.parse::<IpAddr>().ok()),
        );
    }
    let list = dns
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    rows.push(ReportRow::new(
        t("系统 DNS", "System DNS"),
        if list.is_empty() {
            t("未读取到地址", "No addresses reported").into()
        } else {
            list
        },
        format!("{} × {}", t("解析器", "Resolvers"), dns.len()),
    ));
    rows
}

// Emit only enabled mechanisms and their authorities. Never copy userinfo,
// paths, query strings, bypass lists, or raw OS configuration into reports.
fn proxy_rows(
    manager: &ProxyManager,
    inbounds: &[u16],
    pac: u16,
    lang: Language,
) -> Vec<ReportRow> {
    let t = |zh, en| tr(lang, zh, en);
    let backend = manager.backend();
    let mut endpoints = Vec::<(&str, String)>::new();
    let mut automatic_discovery = false;
    let label = t("系统代理", "System proxy");
    if backend == ProxyBackend::MacOs {
        let Some(text) = output("/usr/sbin/scutil", &["--proxy"]) else {
            return vec![ReportRow::plain(
                label,
                t("状态不可用", "State unavailable").into(),
            )];
        };
        let values: BTreeMap<_, _> = text
            .lines()
            .filter_map(|l| l.trim().split_once(" : "))
            .collect();
        for key in ["HTTP", "HTTPS", "SOCKS"] {
            if values.get(format!("{key}Enable").as_str()) == Some(&"1") {
                if let (Some(host), Some(port)) = (
                    values.get(format!("{key}Proxy").as_str()),
                    values.get(format!("{key}Port").as_str()),
                ) {
                    endpoints.push((key, format!("{host}:{port}")));
                }
            }
        }
        if values.get("ProxyAutoConfigEnable") == Some(&"1") {
            if let Some(url) = values.get("ProxyAutoConfigURLString") {
                endpoints.push(("PAC", (*url).into()));
            }
        }
        automatic_discovery = values.get("ProxyAutoDiscoveryEnable") == Some(&"1");
    } else {
        let Ok(entries) = manager.capture() else {
            return vec![ReportRow::plain(
                label,
                t("状态不可用", "State unavailable").into(),
            )];
        };
        let get = |key: &str| {
            entries
                .get(key)
                .map(|v| v.trim_matches(['\'', '"']).trim())
                .unwrap_or("")
        };
        match backend {
            ProxyBackend::Windows => {
                if get("ProxyEnable") == "1" {
                    for value in get("ProxyServer").split(';').filter(|s| !s.is_empty()) {
                        let (scheme, addr) = value.split_once('=').unwrap_or(("all", value));
                        let kind = match scheme {
                            "socks" => "SOCKS",
                            "https" => "HTTPS",
                            "http" => "HTTP",
                            _ => "HTTP/SOCKS",
                        };
                        endpoints.push((kind, addr.into()));
                    }
                }
                if !get("AutoConfigURL").is_empty() {
                    endpoints.push(("PAC", get("AutoConfigURL").into()));
                }
            }
            ProxyBackend::Gnome => match get("org.gnome.system.proxy/mode") {
                "manual" => {
                    for (label, kind) in [("HTTP", "http"), ("SOCKS", "socks")] {
                        let host = get(&format!("org.gnome.system.proxy.{kind}/host"));
                        if !host.is_empty() {
                            endpoints.push((
                                label,
                                format!(
                                    "{host}:{}",
                                    get(&format!("org.gnome.system.proxy.{kind}/port"))
                                ),
                            ));
                        }
                    }
                }
                "auto" => {
                    endpoints.push(("PAC", get("org.gnome.system.proxy/autoconfig-url").into()))
                }
                _ => {}
            },
            ProxyBackend::Kde => match get("ProxyType") {
                "1" => {
                    for (kind, key) in [
                        ("HTTP", "httpProxy"),
                        ("HTTPS", "httpsProxy"),
                        ("SOCKS", "socksProxy"),
                    ] {
                        if !get(key).is_empty() {
                            endpoints.push((kind, get(key).replace(' ', ":")));
                        }
                    }
                }
                "2" => endpoints.push(("PAC", get("Proxy Config Script").into())),
                "3" | "4" => automatic_discovery = true,
                _ => {}
            },
            _ => {}
        }
    }
    let mut rows: Vec<_> = endpoints
        .into_iter()
        .map(|(kind, addr)| {
            let url = url::Url::parse(
                if addr.contains("://") {
                    addr.clone()
                } else {
                    format!("http://{addr}")
                }
                .as_str(),
            )
            .ok();
            let host = url.as_ref().and_then(|u| u.host_str()).unwrap_or("—");
            let port = url.as_ref().and_then(url::Url::port_or_known_default);
            let local = host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            let matches = local
                && port.is_some_and(|port| {
                    if kind == "PAC" {
                        port == pac
                    } else {
                        inbounds.contains(&port)
                    }
                });
            let scope = if matches {
                t("指向本机 Xrose", "Points to local Xrose")
            } else {
                t("其他端点", "Other endpoint")
            };
            ReportRow::new(
                label,
                format!("{kind} · {host}:{} · {scope}", port.unwrap_or(0)),
                format!("{kind} · {scope}"),
            )
        })
        .collect();
    if automatic_discovery {
        rows.push(ReportRow::plain(
            label,
            t("自动发现", "Automatic discovery").into(),
        ));
    }
    if rows.is_empty() {
        rows.push(ReportRow::plain(label, t("未启用", "Disabled").into()));
    }
    rows
}

pub(crate) fn collect(
    model: &AppModel,
    settings: &Settings,
    mesh: &MeshSnapshot,
    manager: &ProxyManager,
    helper: &privilege::HelperStatus,
) -> NetworkReport {
    let t = |zh, en| tr(settings.language, zh, en);
    let mut rows = network_rows(settings.language, mesh);
    let mut ports = Vec::new();
    let mut proxy_ports = Vec::new();
    let inbounds = effective_inbounds(model, settings);
    let tun_enabled = inbounds
        .iter()
        .any(|i| i.kind == crate::model::InboundKind::Tun);
    for inbound in inbounds {
        for port in inbound.listen_ports(settings.core).unwrap_or_default() {
            if let Ok(ip) = inbound.listen_address().parse::<IpAddr>() {
                if matches!(
                    inbound.kind,
                    crate::model::InboundKind::Mixed
                        | crate::model::InboundKind::Http
                        | crate::model::InboundKind::Socks
                ) {
                    proxy_ports.push(port);
                }
                let label = if inbound.allow_lan {
                    "LAN"
                } else {
                    inbound.kind.as_str()
                };
                ports.push((label, "tcp", SocketAddr::new(ip, port)));
            }
        }
    }
    rows.push(ReportRow::plain(
        t("代理后端", "Proxy backend"),
        manager.backend().label().into(),
    ));
    rows.push(ReportRow::plain(
        t("权限", "Privileges"),
        privilege::describe_privileges(helper, tun_enabled, settings.language).1,
    ));
    rows.extend(proxy_rows(
        manager,
        &proxy_ports,
        settings.pac_port,
        settings.language,
    ));
    let ipv4 = |p| SocketAddr::from(([127, 0, 0, 1], p));
    ports.push(("PAC", "tcp", ipv4(settings.pac_port)));
    if settings.core == CoreKind::SingBox {
        ports.push(("Clash API", "tcp", ipv4(settings.api_port)));
    } else {
        ports.push(("Xray Stats", "tcp", ipv4(settings.stats_port)));
    }
    for profile in &mesh.profiles {
        if profile.outbound_port != 0 {
            ports.push((
                t("组网出口", "Mesh exit"),
                "tcp",
                ipv4(profile.outbound_port),
            ));
        }
        ports.extend(
            profile
                .port_forwards
                .iter()
                .map(|f| (t("组网转发", "Mesh forward"), f.protocol.key(), f.listen)),
        );
    }
    let owners = port_owner::visible_owners();
    for (name, proto, addr) in ports {
        let owner = owners.iter().find(|o| {
            o.proto == proto
                && o.addr.port() == addr.port()
                && (o.addr.ip() == addr.ip()
                    || (o.addr.is_ipv4() == addr.is_ipv4()
                        && (o.addr.ip().is_unspecified() || addr.ip().is_unspecified())))
        });
        let (full, safe) = if let Some(owner) = owner {
            (
                format!("{} (PID {})", owner.name, owner.pid),
                format!("PID {}", owner.pid),
            )
        } else {
            let can_bind = if proto == "tcp" {
                TcpListener::bind(addr).map(drop)
            } else {
                UdpSocket::bind(addr).map(drop)
            };
            let status = match can_bind {
                Ok(()) => t("可用", "Available"),
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    t("已占用，归属不可见", "In use, owner unavailable")
                }
                Err(_) => t(
                    "无法监听，请检查地址与权限",
                    "Cannot bind; check address and permissions",
                ),
            };
            (status.to_string(), status.to_string())
        };
        rows.push(ReportRow::new(
            t("监听端口", "Listener"),
            format!("{name} · {proto} {addr} · {full}"),
            format!("{name} · {proto}/{} · {safe}", addr.port()),
        ));
    }
    let helper_label = if helper.needs_update {
        t("需更新", "Update required")
    } else if helper.running {
        t("运行中，版本匹配", "Running, version matches")
    } else if helper.installed {
        t("已安装，未运行", "Installed, stopped")
    } else if helper.supported {
        t("未安装", "Not installed")
    } else {
        t("不支持", "Unsupported")
    };
    rows.push(ReportRow::plain(
        t("提权助手", "Privilege helper"),
        format!("{helper_label} · Xrose {}", env!("CARGO_PKG_VERSION")),
    ));
    rows.push(ReportRow::plain(
        t("TUN 助手协议版本", "TUN helper protocol"),
        privilege::format_protocol_version(
            crate::system::tun_process::VERSION,
            helper.running,
            helper.needs_update,
            helper.running_tun_protocol,
            settings.language,
        ),
    ));
    rows.push(ReportRow::plain(
        t("Mesh 协议版本", "Mesh protocol"),
        privilege::format_protocol_version(
            crate::mesh::PROTOCOL_VERSION,
            helper.running,
            helper.needs_update,
            helper.running_mesh_protocol,
            settings.language,
        ),
    ));
    rows.push(ReportRow::plain(
        t("数据库版本", "Database schema"),
        format!("v{}", crate::storage::SCHEMA_VERSION),
    ));
    NetworkReport { rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_report_omits_dynamic_os_labels_addresses_and_paths() {
        let report = NetworkReport {
            rows: vec![
                ReportRow::new(
                    "网络接口",
                    "lab.example.org · 192.0.2.4 · 2001:db8::3".into(),
                    "#2 · MTU 1500".into(),
                ),
                ReportRow::new(
                    "监听端口",
                    "/example/users/fixture/program · 198.51.100.7".into(),
                    "tcp/443 · PID 42".into(),
                ),
            ],
        };
        for secret in [
            "lab.example.org",
            "192.0.2.4",
            "2001:db8::3",
            "fixture",
            "198.51.100.7",
        ] {
            assert!(report.text(false).contains(secret));
            assert!(!report.text(true).contains(secret));
        }
        assert!(report.text(true).contains("MTU 1500"));
    }
}
