//! Pre-flight diagnostics for TUN mode, shown before the user hits start.

use std::net::{IpAddr, SocketAddr, TcpListener};
#[cfg(target_os = "linux")]
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{AppModel, InboundKind};
use crate::settings::{Language, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckLevel {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub level: CheckLevel,
    pub detail: String,
}

impl DoctorCheck {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            level: CheckLevel::Ok,
            detail: detail.into(),
        }
    }
    fn warn(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            level: CheckLevel::Warn,
            detail: detail.into(),
        }
    }
    fn error(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            level: CheckLevel::Error,
            detail: detail.into(),
        }
    }
}

pub fn is_port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Runs local, side-effect-free checks. Results never block starting; the
/// caller decides what to do with warnings.
pub fn diagnose(model: &AppModel, settings: &Settings) -> Vec<DoctorCheck> {
    diagnose_for_process(model, settings, None, None)
}

pub(crate) fn diagnose_for_process(
    model: &AppModel,
    settings: &Settings,
    core_pid: Option<u32>,
    helper_session: Option<&str>,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let caps = settings.core.capabilities();
    let lang = settings.language;
    let helper = super::privilege::status();

    let tun = model
        .inbounds
        .iter()
        .find(|i| i.enabled && i.kind == InboundKind::Tun);
    if tun.is_some() {
        if caps.native_tun {
            checks.push(DoctorCheck::ok(
                localize(lang, "TUN 支持", "TUN support"),
                format!(
                    "{} {}",
                    settings.core.display_name(),
                    localize(lang, "内建 TUN", "built-in TUN")
                ),
            ));
        } else {
            checks.push(DoctorCheck::error(
                localize(lang, "TUN 支持", "TUN support"),
                format!(
                    "{} {}",
                    settings.core.display_name(),
                    localize(
                        lang,
                        "不支持 TUN，请更新内核",
                        "does not support TUN; update the core"
                    )
                ),
            ));
        }
        #[cfg(target_os = "linux")]
        {
            if Path::new("/dev/net/tun").exists() {
                checks.push(DoctorCheck::ok("TUN 设备", "/dev/net/tun 存在"));
            } else {
                checks.push(DoctorCheck::error(
                    "TUN 设备",
                    "/dev/net/tun 不存在，请加载 tun 模块或使用 tproxy",
                ));
            }
        }
        #[cfg(target_os = "windows")]
        {
            checks.push(DoctorCheck::ok(
                "Wintun",
                "启动 TUN 前自动校验并部署驱动，由提权助手代启核心",
            ));
        }
        checks.push(privilege_check(&helper, true, lang));
        if tun.map(|i| i.tun.auto_route).unwrap_or(false) && settings.system_proxy {
            checks.push(DoctorCheck::ok(
                "流量接管",
                "系统代理和 TUN 自动路由均已启用。关闭系统代理后，TUN 仍会接管流量。",
            ));
        }
    } else {
        checks.push(privilege_check(&helper, false, lang));
    }

    // Take one socket-table snapshot for all occupied ports. Use the helper's
    // view for its own child; the GUI may not be allowed to inspect that PID.
    let owner = core_pid.map(|pid| PortOwner {
        pid,
        helper_session,
        listeners: Default::default(),
    });
    let port_check = |name: String, host: &str, port, conflict: &str| {
        let endpoint = match host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, port),
            Err(_) => {
                let host = if settings.privacy_mode {
                    "•••"
                } else {
                    host
                };
                return DoctorCheck::warn(
                    &name,
                    format!(
                        "{host}:{port} {}",
                        localize(
                            settings.language,
                            "监听地址无效，请检查配置",
                            "has an invalid listen address; check the configuration"
                        )
                    ),
                );
            }
        };
        check_port(&name, endpoint, owner.as_ref(), conflict, settings.language)
    };
    for inbound in crate::model::inbound::effective_inbounds(model, settings) {
        let ports = match inbound.listen_ports(settings.core) {
            Ok(ports) => ports,
            Err(err) => {
                checks.push(DoctorCheck::error("HTTP", err.to_string()));
                continue;
            }
        };
        for port in ports {
            let label = if port == inbound.port {
                localize(lang, "端口", "Port")
            } else {
                "HTTP"
            };
            checks.push(port_check(
                format!(
                    "{label} {}",
                    if settings.privacy_mode {
                        inbound.kind.as_str()
                    } else {
                        &inbound.tag
                    }
                ),
                inbound.listen_address(),
                port,
                localize(lang, "启动可能失败", "the core may fail to start"),
            ));
        }
    }
    if settings.core == crate::model::CoreKind::SingBox {
        checks.push(port_check(
            localize(lang, "Clash API 端口", "Clash API port").into(),
            "127.0.0.1",
            settings.api_port,
            localize(
                lang,
                "流量/连接面板将不可用",
                "traffic and connection data will be unavailable",
            ),
        ));
    }
    if settings.core == crate::model::CoreKind::Xray {
        checks.push(port_check(
            localize(lang, "Xray Stats 端口", "Xray Stats port").into(),
            "127.0.0.1",
            settings.stats_port,
            localize(
                lang,
                "流量统计将不可用",
                "traffic statistics will be unavailable",
            ),
        ));
    }

    if model.dns.fake_ip && !caps.fake_ip {
        checks.push(DoctorCheck::warn(
            "fake-ip",
            format!(
                "{} 不支持 fake-ip，将使用分流 DNS",
                settings.core.display_name()
            ),
        ));
    }
    if model
        .nodes
        .iter()
        .any(|n| n.enabled && !n.protocol.supported_by(settings.core))
    {
        checks.push(DoctorCheck::warn(
            "协议兼容性",
            format!(
                "部分节点（Hysteria2/TUIC/WireGuard）无法用于 {}",
                settings.core.display_name()
            ),
        ));
    }
    checks
}

fn localize<'a>(language: Language, zh: &'a str, en: &'a str) -> &'a str {
    if language == Language::En { en } else { zh }
}

pub(crate) fn privilege_check(
    helper: &super::privilege::HelperStatus,
    tun_enabled: bool,
    language: Language,
) -> DoctorCheck {
    let name = localize(language, "权限", "Permissions");
    let (warn, detail) = super::privilege::describe_privileges(helper, tun_enabled, language);
    if warn {
        DoctorCheck::warn(name, detail)
    } else {
        DoctorCheck::ok(name, detail)
    }
}

#[derive(Default)]
struct ListenerSnapshot {
    endpoints: Vec<SocketAddr>,
    complete: bool,
}

struct PortOwner<'a> {
    pid: u32,
    helper_session: Option<&'a str>,
    listeners: std::cell::OnceCell<ListenerSnapshot>,
}

impl PortOwner<'_> {
    fn listeners(&self) -> &ListenerSnapshot {
        self.listeners.get_or_init(|| {
            if let Some(session) = self.helper_session {
                if let Some(endpoints) = super::tun_process::session_listeners(session, self.pid) {
                    return ListenerSnapshot {
                        endpoints,
                        complete: true,
                    };
                }
                // With an older helper the GUI may still see the sockets. Only
                // positive matches are meaningful across the privilege boundary.
                return ListenerSnapshot {
                    endpoints: super::port_owner::listeners(self.pid).unwrap_or_default(),
                    complete: false,
                };
            }
            super::port_owner::listeners(self.pid)
                .map(|endpoints| ListenerSnapshot {
                    endpoints,
                    complete: true,
                })
                .unwrap_or_default()
        })
    }
}

fn check_port(
    name: &str,
    endpoint: SocketAddr,
    owner: Option<&PortOwner<'_>>,
    conflict: &str,
    lang: Language,
) -> DoctorCheck {
    let port = endpoint.port();
    let tr = |zh, en| localize(lang, zh, en);
    let bind_error = match TcpListener::bind(endpoint) {
        Ok(_) => return DoctorCheck::ok(name, format!("{port} {}", tr("可用", "is available"))),
        Err(e) => e,
    };
    // Windows may report access denied for an exclusively bound socket.
    // Positive ownership evidence still applies in that case.
    if let Some(owner) = owner {
        let pid = owner.pid;
        let snapshot = owner.listeners();
        let ip = endpoint.ip().to_canonical();
        let same_port = || snapshot.endpoints.iter().filter(|l| l.port() == port);
        if same_port().any(|l| {
            let bound = l.ip().to_canonical();
            bound == ip || (bound.is_unspecified() && bound.is_ipv4() == ip.is_ipv4())
        }) {
            return DoctorCheck::ok(
                name,
                format!(
                    "{port} {} (PID {pid})",
                    tr("由当前内核使用", "is in use by the current core")
                ),
            );
        }
        // Port numbers alone cannot distinguish a partial address overlap or
        // whether an IPv6 wildcard also covers IPv4.
        if (bind_error.kind() == std::io::ErrorKind::AddrInUse && !snapshot.complete)
            || same_port().any(|l| {
                ip.is_unspecified() || (ip.is_ipv4() && l.is_ipv6() && l.ip().is_unspecified())
            })
        {
            let detail = if owner.helper_session.is_some() && !snapshot.complete {
                tr(
                    "已占用，无法确认归属；请更新提权助手后重新检查",
                    "is in use; update the helper and re-check ownership",
                )
            } else {
                tr(
                    "已占用，无法确认归属",
                    "is in use; ownership could not be verified",
                )
            };
            return DoctorCheck::warn(name, format!("{port} {detail}"));
        }
    }
    if bind_error.kind() != std::io::ErrorKind::AddrInUse {
        return DoctorCheck::warn(
            name,
            format!(
                "{port} {}: {bind_error}",
                tr(
                    "无法监听，请检查地址和权限",
                    "cannot be bound; check the address and permissions"
                )
            ),
        );
    }
    let detail = if lang == Language::En {
        format!("{port} is in use by another process; {conflict}")
    } else {
        format!("{port} 已被其他进程占用，{conflict}")
    };
    DoctorCheck::warn(name, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnose_reports_ports() {
        let model = AppModel::default();
        let settings = Settings::default();
        let checks = diagnose(&model, &settings);
        assert!(checks.iter().any(|c| c.name.contains("端口")));
    }

    #[test]
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn distinguishes_our_api_listener_from_a_conflict() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let settings = Settings {
            api_port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let model = AppModel::default();
        let own = diagnose_for_process(&model, &settings, Some(std::process::id()), None);
        let check = own.iter().find(|c| c.name == "Clash API 端口").unwrap();
        assert_eq!(check.level, CheckLevel::Ok);
        assert!(check.detail.contains("当前内核"));
        let other = diagnose_for_process(&model, &settings, None, None);
        assert_eq!(
            other
                .iter()
                .find(|c| c.name == "Clash API 端口")
                .unwrap()
                .level,
            CheckLevel::Warn
        );
    }
    #[test]
    fn helper_checks_both_ports_and_distinguishes_missing_evidence() {
        let mixed = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let api = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mixed = mixed.local_addr().unwrap();
        let api = api.local_addr().unwrap();
        let owner = |listeners: Option<Vec<SocketAddr>>| PortOwner {
            pid: 42,
            helper_session: Some("test-session"),
            listeners: ListenerSnapshot {
                complete: listeners.is_some(),
                endpoints: listeners.unwrap_or_default(),
            }
            .into(),
        };
        let checked = |addr, owner: &PortOwner| {
            check_port("端口", addr, Some(owner), "启动可能失败", Language::Zh)
        };
        let managed = owner(Some(vec![mixed, api]));
        for addr in [mixed, api] {
            let check = checked(addr, &managed);
            assert_eq!(check.level, CheckLevel::Ok);
            assert!(check.detail.contains("当前内核"));
        }
        let another_address = SocketAddr::new("127.0.0.2".parse().unwrap(), api.port());
        let foreign = checked(api, &owner(Some(vec![mixed, another_address])));
        assert_eq!(foreign.level, CheckLevel::Warn);
        assert!(foreign.detail.contains("其他进程"));
        let unknown = checked(api, &owner(None));
        assert_eq!(unknown.level, CheckLevel::Warn);
        assert!(unknown.detail.contains("更新提权助手"));
    }

    #[test]
    fn privilege_check_clarifies_gui_and_helper_permissions() {
        let running = crate::system::privilege::HelperStatus {
            supported: true,
            installed: true,
            running: true,
            needs_update: false,
            elevated: false,
            detail: String::new(),
            executable: None,
            running_tun_protocol: Some(1),
            running_mesh_protocol: Some(1),
        };
        let check = privilege_check(&running, true, Language::Zh);
        assert_eq!(check.name, "权限");
        assert_eq!(check.level, CheckLevel::Ok);
        assert!(check.detail.contains("GUI 程序未提权"));
        assert!(check.detail.contains("提权助手：运行中"));

        let stopped = crate::system::privilege::HelperStatus {
            running: false,
            running_tun_protocol: None,
            running_mesh_protocol: None,
            ..running
        };
        assert_eq!(
            privilege_check(&stopped, true, Language::Zh).level,
            CheckLevel::Warn
        );
        assert_eq!(
            privilege_check(&stopped, false, Language::En).level,
            CheckLevel::Ok
        );
    }
}
