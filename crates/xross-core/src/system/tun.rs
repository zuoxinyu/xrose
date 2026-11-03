//! Pre-flight diagnostics for TUN mode, shown before the user hits start.

use std::net::TcpListener;

use serde::{Deserialize, Serialize};

use crate::model::{AppModel, InboundKind};
use crate::settings::Settings;

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
    diagnose_for_process(model, settings, None)
}

pub(crate) fn diagnose_for_process(
    model: &AppModel,
    settings: &Settings,
    core_pid: Option<u32>,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let caps = settings.core.capabilities();

    let tun = model
        .inbounds
        .iter()
        .find(|i| i.enabled && i.kind == InboundKind::Tun);
    if tun.is_some() {
        if caps.native_tun {
            checks.push(DoctorCheck::ok(
                "TUN 支持",
                format!("{} 内建 TUN", settings.core.display_name()),
            ));
        } else {
            checks.push(DoctorCheck::error(
                "TUN 支持",
                format!(
                    "{} 不内建 TUN。请切换到 sing-box，或安装 tun2socks",
                    settings.core.display_name()
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
        if !super::privilege::is_elevated() && !super::privilege::helper_available() {
            checks.push(DoctorCheck::warn(
                "权限",
                "TUN 路由通常需要管理员/root 权限，或安装 Xross 提权助手",
            ));
        } else {
            checks.push(DoctorCheck::ok("权限", "已具备提升权限或助手可用"));
        }
        if tun.map(|i| i.tun.auto_route).unwrap_or(false) && settings.system_proxy {
            checks.push(DoctorCheck::warn(
                "路由冲突",
                "TUN（auto_route）与系统代理同时开启可能造成回环，建议只启用一个",
            ));
        }
    }

    for inbound in model.inbounds.iter().filter(|i| i.enabled) {
        if inbound.kind == InboundKind::Tun || inbound.port == 0 {
            continue;
        }
        checks.push(check_port(
            &format!("端口 {}", inbound.tag),
            inbound.port,
            core_pid,
            "启动可能失败",
        ));
    }
    if settings.core == crate::model::CoreKind::SingBox {
        checks.push(check_port(
            "Clash API 端口",
            settings.api_port,
            core_pid,
            "流量/连接面板将不可用",
        ));
    }
    if settings.core == crate::model::CoreKind::Xray {
        checks.push(check_port(
            "Xray Stats 端口",
            settings.stats_port,
            core_pid,
            "流量统计将不可用",
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

fn check_port(name: &str, port: u16, core_pid: Option<u32>, conflict: &str) -> DoctorCheck {
    if is_port_free(port) {
        DoctorCheck::ok(name, format!("{port} 可用"))
    } else if let Some(pid) = core_pid.filter(|pid| super::port_owner::owns_listener(*pid, port)) {
        DoctorCheck::ok(name, format!("{port} 由当前 core 使用（PID {pid}）"))
    } else {
        DoctorCheck::warn(name, format!("{port} 已被其他进程占用，{conflict}"))
    }
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
        let own = diagnose_for_process(&model, &settings, Some(std::process::id()));
        let check = own.iter().find(|c| c.name == "Clash API 端口").unwrap();
        assert_eq!(check.level, CheckLevel::Ok);
        assert!(check.detail.contains("当前 core"));
        let other = diagnose_for_process(&model, &settings, Some(u32::MAX));
        assert_eq!(
            other
                .iter()
                .find(|c| c.name == "Clash API 端口")
                .unwrap()
                .level,
            CheckLevel::Warn
        );
    }
}
