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
            checks.push(DoctorCheck::warn(
                "Wintun",
                "请确认 wintun.dll 与 core 位于同一目录，并以管理员身份运行",
            ));
        }
        if !super::privilege::is_elevated() && !super::privilege::helper_available() {
            checks.push(DoctorCheck::warn(
                "权限",
                "TUN 路由通常需要管理员/root 权限，或安装 Xross 特权助手",
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
        if is_port_free(inbound.port) {
            checks.push(DoctorCheck::ok(
                &format!("端口 {}", inbound.tag),
                format!("{} 可用", inbound.port),
            ));
        } else {
            checks.push(DoctorCheck::warn(
                &format!("端口 {}", inbound.tag),
                format!("{} 已被占用，启动可能失败", inbound.port),
            ));
        }
    }
    if !is_port_free(settings.api_port) {
        checks.push(DoctorCheck::warn(
            "Clash API 端口",
            format!("{} 已被占用，流量/连接面板将不可用", settings.api_port),
        ));
    }
    if settings.core == crate::model::CoreKind::Xray && !is_port_free(settings.stats_port) {
        checks.push(DoctorCheck::warn(
            "Xray Stats 端口",
            format!("{} 已被占用，流量统计将不可用", settings.stats_port),
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
}
