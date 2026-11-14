//! Runtime-only mesh client and exit-policy compilation.

use std::net::{IpAddr, SocketAddr};

use crate::{
    Error, Result,
    model::{
        AppModel, CoreKind, DnsConfig, DnsServer, GroupKind, Inbound, InboundKind, Matchers,
        MeshDnsMode, Node, OutboundGroup, Protocol, RouteAction, RouteRule, RoutingConfig,
        WorkMode,
    },
    settings::Settings,
};

use super::{CompileContext, CompiledConfig};

pub(super) const BOOTSTRAP_DNS: &str = "xrose-mesh-bootstrap";
pub(super) const EXIT_INBOUND: &str = "xrose-exit-policy";

pub(super) fn transport_process_path(core: CoreKind, path: &str) -> String {
    let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_owned()
    };
    if core == CoreKind::Xray {
        path.replace('\\', "/")
    } else {
        path
    }
}

pub(super) fn requires_proxy(model: &AppModel) -> bool {
    model.routing.final_action.kind == crate::model::RouteActionKind::Proxy
        || model
            .routing
            .rules_for_mode(model.work_mode)
            .any(|rule| rule.action.kind == crate::model::RouteActionKind::Proxy)
}

pub(super) fn policy_groups(model: &AppModel, core: CoreKind) -> Result<Vec<&OutboundGroup>> {
    let mut groups = Vec::new();
    let mut ids = model
        .routing
        .effective_rules()
        .map(|rule| &rule.action)
        .chain(std::iter::once(&model.routing.final_action))
        .filter(|action| action.kind == crate::model::RouteActionKind::Proxy)
        .map(|action| action.group.clone().or_else(|| model.default_group_id()))
        .collect::<Vec<_>>();
    for server in model
        .dns
        .servers
        .iter()
        .filter(|server| !server.is_fakeip())
    {
        match server.detour.as_deref().unwrap_or("").trim() {
            "" | "direct" => {}
            "proxy" | "default" => ids.push(model.default_group_id()),
            id if model.node(id).is_some() => {
                if !model.node(id).is_some_and(|node| node.usable_with(core)) {
                    return Err(Error::config(
                        "出口 DNS 引用了不可用节点，请修改 DNS 出站设置",
                    ));
                }
            }
            id => ids.push(Some(
                model
                    .groups
                    .iter()
                    .find(|group| group.id == id || group.tag() == id)
                    .map(|group| group.id.clone())
                    .unwrap_or_else(|| id.to_owned()),
            )),
        }
    }
    for id in ids {
        let group = id
            .as_deref()
            .and_then(|id| model.group(id))
            .filter(|group| {
                group.enabled
                    && group
                        .members
                        .iter()
                        .any(|id| model.node(id).is_some_and(|node| node.usable_with(core)))
            })
            .ok_or_else(|| {
                Error::config("出口分流规则引用了不可用出站组，请启用该组并添加可用节点")
            })?;
        if !groups.iter().any(|old: &&OutboundGroup| old.id == group.id) {
            groups.push(group);
        }
    }
    Ok(groups)
}

pub(super) fn validate_policy_dns_nodes(model: &AppModel, config: &CompiledConfig) -> Result<()> {
    for node in model.dns.servers.iter().filter_map(|server| {
        server
            .detour
            .as_deref()
            .and_then(|id| model.node(id.trim()))
    }) {
        if !config.json["outbounds"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(config.json["endpoints"].as_array().into_iter().flatten())
            .any(|outbound| outbound["tag"] == node.outbound_tag())
        {
            return Err(Error::config("出口 DNS 的出站节点无法编译，请检查节点配置"));
        }
    }
    Ok(())
}

/// Resolved session data. Credentials never enter the saved mesh-mode intent.
#[derive(Debug, Clone)]
pub struct MeshExitRuntime {
    pub node: Node,
    pub dns_address: Option<SocketAddr>,
    pub ipv6: bool,
    pub transport_process: Option<String>,
}

/// Derive the transport configuration without changing the saved proxy policy.
pub fn mesh_runtime_model(
    model: &AppModel,
    settings: &Settings,
    runtime: &MeshExitRuntime,
) -> Result<(AppModel, Settings)> {
    let target = model
        .mesh_mode
        .target
        .as_ref()
        .ok_or_else(|| Error::config("请先选择组网出口节点"))?;
    if runtime.node.id != target.gateway_id
        || !runtime.node.enabled
        || runtime.node.protocol != Protocol::Socks
        || !runtime
            .node
            .server
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
        || runtime.node.port == 0
        || runtime
            .node
            .credentials
            .password
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(Error::config("组网出口尚未就绪，请等待连接或重新选择出口"));
    }
    let mut effective = model.clone();
    effective.work_mode = WorkMode::Proxy;
    effective.nodes = vec![runtime.node.clone()];
    let mut group = OutboundGroup::new("组网出口", GroupKind::Selector);
    group.id = "xrose-mesh-system".into();
    group.members = vec![runtime.node.id.clone()];
    group.selected = Some(runtime.node.id.clone());
    effective.default_group = Some(group.id.clone());
    effective.groups = vec![group.clone()];
    let rule_sets = super::rule_set_tags(model, WorkMode::Mesh);
    effective.routing = RoutingConfig {
        final_action: RouteAction::proxy_via(&group.id),
        bypass_lan: model.mesh_mode.allow_lan,
        bypass_china_ips: false,
        rule_sets: model
            .routing
            .rule_sets
            .iter()
            .filter(|set| rule_sets.contains(&set.tag))
            .cloned()
            .collect(),
        rule_set_source: model.routing.rule_set_source,
        custom_rule_set_template: model.routing.custom_rule_set_template.clone(),
        ..Default::default()
    };
    effective.routing.rules.clear();
    effective.routing.rule_groups.clear();
    if !runtime.ipv6 {
        effective.routing.rules.push(RouteRule::new(
            "当前组网出口不支持 IPv6",
            Matchers {
                ip_cidrs: vec!["::/0".into()],
                ..Default::default()
            },
            RouteAction::block(),
        ));
    }
    let mut tun = Inbound::new("mesh-system-tun", InboundKind::Tun, 0);
    tun.tun.mtu = 1280;
    tun.sniff = false;
    effective.inbounds = vec![tun];

    let address = runtime
        .dns_address
        .map(|address| address.to_string())
        .unwrap_or_else(|| "1.1.1.1".into());
    let mut bootstrap = DnsServer::udp(BOOTSTRAP_DNS, &address);
    bootstrap.detour = Some(
        if runtime
            .dns_address
            .is_some_and(|address| address.ip().is_loopback())
        {
            "direct".into()
        } else {
            group.id.clone()
        },
    );
    effective.dns = match model.mesh_mode.dns {
        MeshDnsMode::Exit => DnsConfig {
            enabled: true,
            fake_ip: false,
            servers: Vec::new(),
            hosts: Default::default(),
            final_server: Some(BOOTSTRAP_DNS.into()),
            ..Default::default()
        },
        MeshDnsMode::Custom => {
            let mut dns = model.dns.clone();
            dns.enabled = true;
            dns.fake_ip = false;
            dns.servers.retain(|server| !server.is_fakeip());
            if dns.servers.is_empty()
                || dns.servers.iter().any(|server| {
                    matches!(server.address.trim(), "" | "system" | "local" | "localhost")
                })
            {
                return Err(Error::config(
                    "自定义组网 DNS 需要明确的服务器地址，请在 DNS 页面替换系统解析器",
                ));
            }
            if dns.servers.iter().any(|server| server.tag == BOOTSTRAP_DNS) {
                return Err(Error::config(
                    "DNS 名称与组网保留名称冲突，请重命名该服务器",
                ));
            }
            for server in &mut dns.servers {
                server.detour = Some(group.id.clone());
                let display = server.display_address();
                let url = if display.contains("://") {
                    display
                } else {
                    format!("udp://{display}")
                };
                if let Ok(url) = url::Url::parse(&url) {
                    if let Some(host) = url
                        .host_str()
                        .filter(|host| host.parse::<IpAddr>().is_err())
                    {
                        bootstrap.domains.push(format!("full:{host}"));
                    }
                }
            }
            if dns
                .final_server
                .as_ref()
                .is_none_or(|tag| !dns.servers.iter().any(|s| &s.tag == tag))
            {
                dns.final_server = dns
                    .servers
                    .iter()
                    .find(|s| s.domains.is_empty())
                    .map(|s| s.tag.clone());
            }
            if dns.final_server.is_none() {
                return Err(Error::config("请为自定义组网 DNS 选择默认解析服务器"));
            }
            dns
        }
    };
    bootstrap.domains.sort();
    bootstrap.domains.dedup();
    effective.dns.servers.push(bootstrap);
    let mut settings = settings.clone();
    settings.tun_enabled = true;
    settings.system_proxy = false;
    settings.pac_enabled = false;
    settings.allow_lan = false;
    settings.separate_lan_port = false;
    Ok((effective, settings))
}

pub(super) fn compile_client(
    core: CoreKind,
    model: &AppModel,
    settings: &Settings,
    ctx: &CompileContext,
) -> Result<CompiledConfig> {
    if cfg!(target_os = "macos")
        && core == CoreKind::Xray
        && model
            .mesh_mode
            .target
            .as_ref()
            .is_some_and(|target| target.provider == crate::mesh::MeshProvider::EasyTier)
        && ctx
            .core_version
            .as_ref()
            .is_some_and(|version| *version < semver::Version::new(26, 10, 31))
    {
        return Err(Error::config(
            "macOS 上的 EasyTier 系统接管需要 Xray 26.10.31 或更新版本，请更新内核或切换至 sing-box",
        ));
    }
    let runtime = ctx
        .mesh_exit
        .as_ref()
        .ok_or_else(|| Error::config("组网出口尚未连接，系统流量接管未启动"))?;
    let (effective, settings) = mesh_runtime_model(model, settings, runtime)?;
    let mut context = ctx.clone();
    context.mesh_transport = true;
    let mut config = super::adapter_for(core).compile(&effective, &settings, &context)?;
    if !runtime.ipv6 {
        config.warnings.push(
            "此组网出口仅支持 IPv4，IPv6 互联网流量已阻止，请更换支持 IPv6 的出口以启用".into(),
        );
    }
    Ok(config)
}

/// Compile forwarded traffic independently of this host's active mode/listeners.
pub fn compile_exit_gateway(
    model: &AppModel,
    settings: &Settings,
    ctx: &CompileContext,
    port: u16,
    password: &str,
) -> Result<CompiledConfig> {
    if port < 1024
        || password.len() < 16
        || password.len() > 255
        || password.chars().any(char::is_control)
    {
        return Err(Error::config(
            "出口分流服务的本地端口或凭据无效，请重新启动服务",
        ));
    }
    let mut effective = model.clone();
    effective.work_mode = WorkMode::Proxy;
    effective.inbounds = vec![Inbound::new(EXIT_INBOUND, InboundKind::Socks, port)];
    effective.dns.enabled = true;
    effective.dns.fake_ip = false;
    effective.dns.servers.retain(|server| !server.is_fakeip());
    let mut warnings = Vec::new();
    for rule in effective.routing.all_rules_mut() {
        if rule.enabled
            && (!rule.matchers.process_names.is_empty() || !rule.matchers.process_paths.is_empty())
        {
            rule.enabled = false;
            warnings.push(format!(
                "出口分流无法获取远端进程，已跳过规则「{}」；请改用域名或地址条件",
                rule.name
            ));
        } else if rule.enabled
            && (!rule.matchers.source_ip_cidrs.is_empty() || !rule.matchers.source_ports.is_empty())
        {
            rule.enabled = false;
            warnings.push(format!(
                "出口分流无法保留远端来源地址，已跳过规则「{}」；请改用目标条件",
                rule.name
            ));
        }
    }
    let mut settings = settings.clone();
    settings.tun_enabled = false;
    settings.system_proxy = false;
    settings.pac_enabled = false;
    settings.allow_lan = false;
    settings.separate_lan_port = false;
    let mut context = ctx.clone();
    context.mesh_exit = None;
    context.mesh_transport = false;
    policy_groups(&effective, settings.core)?;
    let mut config = super::adapter_for(settings.core).compile(&effective, &settings, &context)?;
    validate_policy_dns_nodes(&effective, &config)?;
    match settings.core {
        CoreKind::SingBox => {
            super::singbox::authenticate_exit_gateway(&mut config, &effective, password, &context)?
        }
        CoreKind::Xray => {
            super::xray::authenticate_exit_gateway(&mut config, &effective, password)?;
            config.warnings.push("Xray 出口域名分流依赖可识别的连接域名；加密或无法识别的连接使用 IP 与兜底规则，需 DNS 地址关联时请选择 sing-box".into());
        }
    }
    config.warnings.extend(warnings);
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mesh::MeshProvider, model::MeshExitTarget};
    use serde_json::json;

    fn fixture() -> (AppModel, Settings, CompileContext) {
        let mut model = AppModel::default();
        let proxy = Node::new("Fixture proxy", Protocol::Socks, "192.0.2.10", 1080);
        model.add_nodes([proxy]);
        model.work_mode = WorkMode::Mesh;
        model.routing.rules.push(RouteRule::new(
            "Fixture local block",
            Matchers {
                domains: vec!["blocked.example".into()],
                ..Default::default()
            },
            RouteAction::block(),
        ));
        model.routing.bypass_china_ips = true;
        model.routing.bypass_lan = true;
        let mut node = Node::new("Fixture mesh", Protocol::Socks, "127.0.0.1", 19081);
        node.id = "fixture-exit".into();
        node.credentials.username = Some("xrose".into());
        node.credentials.password = Some("fixture-secret-only".into());
        model.mesh_mode.target = Some(MeshExitTarget {
            profile_id: "fixture-network".into(),
            peer_id: "fixture-peer".into(),
            provider: MeshProvider::Tailscale,
            name: "Fixture exit".into(),
            address: "100.64.0.10".into(),
            gateway_id: node.id.clone(),
        });
        let settings = Settings::default();
        let mut ctx =
            CompileContext::new(&settings, "fixture-data".into(), "fixture-runtime".into());
        ctx.mesh_exit = Some(MeshExitRuntime {
            node,
            dns_address: Some("127.0.0.153:53".parse().unwrap()),
            ipv6: true,
            transport_process: None,
        });
        (model, settings, ctx)
    }

    #[test]
    fn mesh_worker_bypass_is_exact_and_precedes_dns_and_exit_routing() {
        assert_eq!(
            transport_process_path(CoreKind::SingBox, r"\\?\C:\Xrose\worker.exe"),
            r"C:\Xrose\worker.exe"
        );
        assert_eq!(
            transport_process_path(CoreKind::Xray, r"\\?\C:\Xrose\worker.exe"),
            "C:/Xrose/worker.exe"
        );
        let (mut model, settings, mut ctx) = fixture();
        ctx.mesh_exit.as_mut().unwrap().transport_process =
            Some("/opt/xrose/fixture-worker".into());
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let config = super::super::adapter_for(core)
                .compile(&model, &settings, &ctx)
                .unwrap();
            if core == CoreKind::SingBox {
                let rule = &config.json["route"]["rules"][0];
                assert_eq!(rule["inbound"], json!(["mesh-system-tun"]));
                assert_eq!(rule["process_path"], json!(["/opt/xrose/fixture-worker"]));
                assert_eq!(rule["outbound"], "direct");
            } else {
                let rule = &config.json["routing"]["rules"][1];
                assert_eq!(rule["inboundTag"], json!(["mesh-system-tun"]));
                assert_eq!(rule["process"], json!(["/opt/xrose/fixture-worker"]));
                assert_eq!(rule["outboundTag"], "direct");
            }
        }
        model.mesh_mode.target.as_mut().unwrap().provider = MeshProvider::EasyTier;
        ctx.core_version = Some(semver::Version::new(26, 10, 31));
        assert!(
            super::super::adapter_for(CoreKind::Xray)
                .compile(&model, &settings, &ctx)
                .is_ok()
        );
        if cfg!(target_os = "macos") {
            ctx.core_version = Some(semver::Version::new(26, 10, 30));
            assert!(
                super::super::adapter_for(CoreKind::Xray)
                    .compile(&model, &settings, &ctx)
                    .is_err()
            );
        }
    }

    #[test]
    fn mesh_mode_captures_both_families_without_saved_proxy_policy() {
        let (model, settings, ctx) = fixture();
        let saved = model.clone();
        let saved_settings = settings.clone();
        assert_eq!(model.routing.rules_for_mode(WorkMode::Mesh).count(), 0);
        assert!(super::super::required_rule_sets(&model).is_empty());
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let compiled = super::super::adapter_for(core)
                .compile(&model, &settings, &ctx)
                .unwrap();
            assert_eq!(compiled.inbounds.len(), 1);
            assert_eq!(compiled.inbounds[0].kind, "tun");
            assert!(compiled.rule_sets.is_empty());
            assert_eq!(compiled.default_proxy.node.as_deref(), Some("fixture-exit"));
            let text = compiled.to_pretty_string().unwrap();
            assert!(!text.contains("blocked.example"));
            assert!(!text.contains("192.0.2.10"));
            assert!(!text.contains("geoip-cn"));
            assert!(!text.contains("fakeip"));
            if core == CoreKind::SingBox {
                assert!(
                    compiled.json["dns"]["servers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|s| s["type"] != "local")
                );
                let tun = &compiled.json["inbounds"][0];
                assert_eq!(tun["auto_route"], true);
                assert_eq!(tun["address"].as_array().unwrap().len(), 2);
                assert_eq!(compiled.json["dns"]["final"], BOOTSTRAP_DNS);
            } else {
                assert!(!text.contains("localhost"));
                assert!(
                    compiled.json["dns"]["servers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|server| server["address"] == "127.0.0.153" && server["port"] == 53)
                );
                assert!(
                    compiled.json["routing"]["rules"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|rule| {
                            rule["ip"] == json!(["127.0.0.153"]) && rule["outboundTag"] == "direct"
                        })
                );
            }
        }
        assert_eq!(model, saved);
        assert_eq!(settings, saved_settings);
    }

    #[test]
    fn mesh_mode_rejects_ipv6_for_ipv4_only_exits_and_never_falls_back_direct() {
        let (model, settings, mut ctx) = fixture();
        let runtime = ctx.mesh_exit.as_mut().unwrap();
        runtime.ipv6 = false;
        runtime.dns_address = None;
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let compiled = super::super::adapter_for(core)
                .compile(&model, &settings, &ctx)
                .unwrap();
            assert!(
                compiled
                    .warnings
                    .iter()
                    .any(|message| message.contains("IPv6") && message.contains("阻止"))
            );
            match core {
                CoreKind::SingBox => {
                    let rules = compiled.json["route"]["rules"].as_array().unwrap();
                    assert!(rules.iter().any(
                        |rule| rule["ip_cidr"] == json!(["::/0"]) && rule["action"] == "reject"
                    ));
                    assert_ne!(compiled.json["route"]["final"], "direct");
                    let dns = compiled.json["dns"]["servers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|s| s["tag"] == BOOTSTRAP_DNS)
                        .unwrap();
                    assert_eq!(dns["server"], "1.1.1.1");
                    assert_eq!(dns["detour"], "group-xrose-mesh-system");
                }
                CoreKind::Xray => {
                    let rules = compiled.json["routing"]["rules"].as_array().unwrap();
                    assert!(rules.iter().any(
                        |rule| rule["ip"] == json!(["::/0"]) && rule["outboundTag"] == "block"
                    ));
                    assert_eq!(rules.last().unwrap()["outboundTag"], "node-fixture-exit");
                }
            }
        }
        ctx.mesh_exit = None;
        assert!(
            super::super::adapter_for(CoreKind::SingBox)
                .compile(&model, &settings, &ctx)
                .is_err()
        );
    }

    #[test]
    fn custom_dns_uses_exit_for_queries_and_bootstrap_without_mutating_saved_detours() {
        let (mut model, settings, mut ctx) = fixture();
        model.mesh_mode.dns = MeshDnsMode::Custom;
        let mut custom = DnsServer::https("custom", "https://resolver.example/dns-query");
        custom.detour = Some("direct".into());
        model.dns.servers = vec![custom];
        model.dns.final_server = Some("custom".into());
        ctx.mesh_exit.as_mut().unwrap().dns_address = None;
        let compiled = super::super::adapter_for(CoreKind::SingBox)
            .compile(&model, &settings, &ctx)
            .unwrap();
        let servers = compiled.json["dns"]["servers"].as_array().unwrap();
        assert!(servers.iter().all(|server| server["type"] != "local"));
        let custom = servers.iter().find(|s| s["tag"] == "custom").unwrap();
        assert_eq!(custom["detour"], "group-xrose-mesh-system");
        assert_eq!(custom["domain_resolver"], BOOTSTRAP_DNS);
        assert_eq!(model.dns.servers[0].detour.as_deref(), Some("direct"));
        let xray = super::super::adapter_for(CoreKind::Xray)
            .compile(&model, &settings, &ctx)
            .unwrap();
        assert!(
            xray.json["dns"]["servers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|server| {
                    server["address"] == "1.1.1.1"
                        && server["domains"] == json!(["full:resolver.example"])
                        && server["skipFallback"] == true
                })
        );
        model.dns.servers[0] = DnsServer::udp("system", "system");
        assert!(
            super::super::adapter_for(CoreKind::SingBox)
                .compile(&model, &settings, &ctx)
                .is_err()
        );
    }

    #[test]
    fn remote_exit_policy_is_authenticated_and_independent_of_host_mode() {
        let (mut model, settings, ctx) = fixture();
        model.work_mode = WorkMode::Direct;
        model.routing.rules.clear();
        model.routing.bypass_lan = false;
        model.routing.bypass_china_ips = false;
        model.routing.rules.push(RouteRule::new(
            "Remote block",
            Matchers {
                domains: vec!["blocked.example".into()],
                ..Default::default()
            },
            RouteAction::block(),
        ));
        model.routing.rules.push(RouteRule::new(
            "Local process",
            Matchers {
                process_names: vec!["fixture-app".into()],
                ..Default::default()
            },
            RouteAction::direct(),
        ));
        let saved = model.clone();
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let settings = Settings {
                core,
                ..settings.clone()
            };
            let compiled =
                compile_exit_gateway(&model, &settings, &ctx, 19082, "fixture-secret-only")
                    .unwrap();
            assert_eq!(compiled.inbounds.len(), 1);
            assert_eq!(compiled.inbounds[0].listen, "127.0.0.1");
            assert_eq!(compiled.inbounds[0].kind, "socks");
            assert!(
                compiled
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("远端进程"))
            );
            let text = compiled.to_pretty_string().unwrap();
            assert!(text.contains("blocked.example"));
            assert!(!text.contains("fixture-app"));
            let inbound = compiled.json["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|inbound| inbound["tag"] == EXIT_INBOUND)
                .unwrap();
            match core {
                CoreKind::SingBox => {
                    assert!(inbound.get("udp").is_none());
                    assert_eq!(compiled.json["dns"]["reverse_mapping"], true);
                    assert_eq!(
                        compiled.json["experimental"]["cache_file"]["path"],
                        json!(ctx.runtime_dir.join("exit-cache.db"))
                    );
                    assert_eq!(inbound["users"][0]["password"], "fixture-secret-only")
                }
                CoreKind::Xray => assert_eq!(inbound["settings"]["auth"], "password"),
            }
        }
        assert_eq!(model, saved);
        model.routing.final_action = RouteAction::proxy_via("missing-group");
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let settings = Settings {
                core,
                ..settings.clone()
            };
            assert!(
                compile_exit_gateway(&model, &settings, &ctx, 19082, "fixture-secret-only")
                    .is_err()
            );
        }
    }

    #[test]
    fn exit_dns_keeps_its_own_outbound_when_traffic_defaults_to_direct() {
        let (mut model, settings, ctx) = fixture();
        model.work_mode = WorkMode::Direct;
        model.routing.rules.clear();
        model.routing.rule_groups.clear();
        model.routing.final_action = RouteAction::direct();
        let mut server = DnsServer::udp("fixture-dns", "192.0.2.53");
        server.detour = Some("proxy".into());
        model.dns.servers = vec![server];
        model.dns.final_server = Some("fixture-dns".into());
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let settings = Settings {
                core,
                ..settings.clone()
            };
            let compiled =
                compile_exit_gateway(&model, &settings, &ctx, 19082, "fixture-secret-only")
                    .unwrap();
            assert_eq!(compiled.default_outbound, "direct");
            if core == CoreKind::SingBox {
                assert_eq!(
                    compiled.json["dns"]["servers"][0]["detour"],
                    model.groups[0].tag()
                );
            } else {
                let tag = &compiled.json["dns"]["servers"][0]["tag"];
                assert_eq!(
                    compiled.json["dns"]["servers"]
                        .as_array()
                        .unwrap()
                        .last()
                        .unwrap()["tag"],
                    *tag
                );
                assert!(
                    compiled.json["routing"]["rules"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|rule| {
                            rule["inboundTag"] == json!([tag])
                                && rule["outboundTag"] == model.nodes[0].outbound_tag()
                        })
                );
            }
            let mut invalid = model.clone();
            invalid.dns.servers[0].detour = Some("missing-dns-group".into());
            assert!(
                compile_exit_gateway(&invalid, &settings, &ctx, 19082, "fixture-secret-only")
                    .is_err()
            );
            invalid.dns.servers[0].detour = Some("proxy".into());
            invalid.groups.clear();
            invalid.nodes.clear();
            assert!(
                compile_exit_gateway(&invalid, &settings, &ctx, 19082, "fixture-secret-only")
                    .is_err()
            );
        }
    }

    #[test]
    fn direct_exit_policy_does_not_require_a_proxy_subscription() {
        let mut model = AppModel::default();
        model.dns.servers.retain(|server| !server.requires_proxy());
        model.routing.rules.clear();
        model.routing.rule_groups.clear();
        model.routing.bypass_china_ips = false;
        model.routing.final_action = RouteAction::direct();
        model.routing.rules.push(RouteRule::new(
            "Remote block",
            Matchers {
                domains: vec!["blocked.example".into()],
                ..Default::default()
            },
            RouteAction::block(),
        ));
        let settings = Settings::default();
        let ctx = CompileContext::new(&settings, "fixture-data".into(), "fixture-runtime".into());
        for core in [CoreKind::SingBox, CoreKind::Xray] {
            let settings = Settings {
                core,
                ..settings.clone()
            };
            let config =
                compile_exit_gateway(&model, &settings, &ctx, 19082, "fixture-secret-only")
                    .unwrap();
            assert_eq!(config.default_outbound, "direct");
            assert!(
                config
                    .to_pretty_string()
                    .unwrap()
                    .contains("blocked.example")
            );
        }
    }
}
