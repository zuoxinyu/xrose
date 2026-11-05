use std::path::Path;

use serde_json::{Map, Value, json};

use super::{
    CompileContext, CompiledConfig, CompiledInbound, CoreAdapter, action_tag, network_str,
    xray_dns_address,
};
use crate::error::{Error, Result};
use crate::model::{
    AppModel, CoreKind, DomainStrategy, GroupKind, Inbound, InboundKind, Node, Protocol, RouteRule,
    Transport,
};
use crate::settings::Settings;

pub struct XrayAdapter;

impl CoreAdapter for XrayAdapter {
    fn kind(&self) -> CoreKind {
        CoreKind::Xray
    }

    fn check_args(&self, config_path: &Path) -> Vec<String> {
        vec![
            "run".into(),
            "-test".into(),
            "-config".into(),
            config_path.display().to_string(),
        ]
    }

    fn compile(
        &self,
        model: &AppModel,
        settings: &Settings,
        ctx: &CompileContext,
    ) -> Result<CompiledConfig> {
        let mut warnings = Vec::new();
        let mut compiled_inbounds = Vec::new();
        let mut inbounds = Vec::new();

        let listeners = crate::model::inbound::effective_inbounds(model, settings);
        crate::model::inbound::validate_listeners(&listeners, CoreKind::Xray, settings)?;
        for inbound in &listeners {
            let account = (inbound.allow_lan && settings.lan_auth).then_some(&settings.lan_account);
            match inbound.kind {
                InboundKind::Mixed => {
                    // Xray has no "mixed" inbound. SOCKS and HTTP must use
                    // different ports, so we allocate port+1 for HTTP.
                    let http_port = inbound.port.saturating_add(1);
                    if inbound.port >= 65535 {
                        return Err(Error::config(
                            "mixed 入站端口过大，无法为 HTTP 分配相邻端口",
                        ));
                    }
                    inbounds.push(socks_inbound(inbound, account)?);
                    inbounds.push(http_inbound(inbound, http_port, account)?);
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: inbound.tag.clone(),
                        kind: "socks".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: format!("{}-http", inbound.tag),
                        kind: "http".into(),
                        port: http_port,
                        allow_lan: inbound.allow_lan,
                    });
                    warnings.push(format!(
                        "Xray 没有 mixed 入站：{} 使用 SOCKS:{} + HTTP:{}",
                        inbound.tag, inbound.port, http_port
                    ));
                }
                InboundKind::Socks => {
                    inbounds.push(socks_inbound(inbound, account)?);
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: inbound.tag.clone(),
                        kind: "socks".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                }
                InboundKind::Http => {
                    inbounds.push(http_inbound(inbound, inbound.port, account)?);
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: inbound.tag.clone(),
                        kind: "http".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                }
                InboundKind::Tun => {
                    inbounds.push(tun_inbound(inbound, ctx, model.dns.enabled, &mut warnings)?);
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: inbound.tag.clone(),
                        kind: "tun".into(),
                        port: 0,
                        allow_lan: false,
                    });
                }
                InboundKind::Tproxy | InboundKind::Redirect => {
                    if !cfg!(target_os = "linux") {
                        warnings.push(format!(
                            "{} 入站仅支持 Linux，已跳过",
                            inbound.kind.as_str()
                        ));
                        continue;
                    }
                    inbounds.push(transparent_inbound(inbound));
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
                        tag: inbound.tag.clone(),
                        kind: inbound.kind.as_str().into(),
                        port: inbound.port,
                        allow_lan: false,
                    });
                }
            }
        }

        if compiled_inbounds.is_empty() {
            return Err(Error::config(
                "没有可用的入站，请启用至少一个代理或 TUN 入站",
            ));
        }

        // --- API inbound -------------------------------------------------
        inbounds.push(json!({
            "tag": "api-in",
            "listen": "127.0.0.1",
            "port": ctx.stats_port,
            "protocol": "dokodemo-door",
            "settings": { "address": "127.0.0.1", "network": "tcp" }
        }));

        // --- Outbounds ---------------------------------------------------
        let mut outbounds = Vec::new();
        let front = model.front_node(CoreKind::Xray)?;
        for node in &model.nodes {
            if !node.enabled {
                continue;
            }
            if !node.protocol.supported_by(CoreKind::Xray) {
                warnings.push(format!(
                    "节点“{}”使用 {}，Xray 不支持，已跳过",
                    node.name,
                    node.protocol.display_name()
                ));
                continue;
            }
            match chained_outbound(node, front, &mut warnings) {
                Ok(value) => outbounds.push(value),
                Err(err) => warnings.push(format!("节点“{}”编译失败：{err}", node.name)),
            }
        }
        if let Some(front) = front {
            let tag = front.outbound_tag();
            if !outbounds.iter().any(|v| v["tag"] == tag) {
                return Err(Error::config(
                    "前置节点无法编译，请检查节点配置或重新选择前置代理",
                ));
            }
        }
        if !outbounds
            .iter()
            .any(|o| o["protocol"] != "blackhole" && o["protocol"] != "dns")
        {
            return Err(Error::config("没有可编译的出站节点"));
        }
        outbounds.push(json!({"tag": "direct", "protocol": "freedom", "settings": {"domainStrategy": "UseIP"}}));
        outbounds.push(json!({"tag": "block", "protocol": "blackhole", "settings": {}}));
        if model.dns.enabled {
            outbounds.push(
                json!({"tag": "dns-out", "protocol": "dns", "settings": {"address": "8.8.8.8"}}),
            );
        }

        // Resolve group selections against successfully compiled nodes only.
        let valid_tags: std::collections::HashSet<String> = outbounds
            .iter()
            .filter_map(|o| o["tag"].as_str().map(str::to_owned))
            .collect();
        let mut resolved_model = model.clone();
        for group in &mut resolved_model.groups {
            group.members.retain(|id| {
                model
                    .node(id)
                    .is_some_and(|n| valid_tags.contains(&n.outbound_tag()))
            });
            group.repair();
        }
        resolved_model.groups.retain(|g| !g.members.is_empty());
        if resolved_model
            .default_group
            .as_ref()
            .is_some_and(|id| resolved_model.group(id).is_none())
        {
            resolved_model.default_group = resolved_model.groups.first().map(|g| g.id.clone());
        }
        let model = &resolved_model;

        // --- Groups / balancers -----------------------------------------
        let mut balancers = Vec::new();
        let mut observatory_needed = false;
        for group in model.groups.iter().filter(|g| g.enabled) {
            let member_tags: Vec<String> = group
                .members
                .iter()
                .filter_map(|id| model.node(id))
                .filter(|n| n.usable_with(CoreKind::Xray))
                .map(|n| n.outbound_tag())
                .collect();
            if group.kind == GroupKind::Selector || group.managed() {
                // Selector is compiled via routing target resolution; no
                // balancer is necessary and Xray cannot switch it at runtime.
                if group.active_node().is_none() {
                    warnings.push(format!("选择组“{}”没有可用成员", group.name));
                }
                continue;
            }
            if member_tags.is_empty() {
                warnings.push(format!("出站组“{}”没有可用成员，已跳过", group.name));
                continue;
            }
            let strategy = match group.kind {
                GroupKind::LoadBalance => json!({"type": "random"}),
                _ => {
                    observatory_needed = true;
                    json!({"type": "leastPing"})
                }
            };
            balancers.push(json!({
                "tag": group.tag(),
                "selector": member_tags,
                "strategy": strategy
            }));
        }

        // --- Routing -----------------------------------------------------
        let mut rules: Vec<Value> = Vec::new();
        rules.push(json!({"type": "field", "inboundTag": ["api-in"], "outboundTag": "api"}));
        if !ctx.mesh.routes.is_empty() {
            rules.push(json!({"type":"field","ip":ctx.mesh.routes.iter().map(|r| &r.cidr).collect::<Vec<_>>(),"outboundTag":"direct"}));
        }
        if !ctx.mesh.dns.is_empty() {
            rules.push(json!({"type":"field","domain":ctx.mesh.dns.iter().map(|d|format!("domain:{}",d.suffix)).collect::<Vec<_>>(),"outboundTag":"direct"}));
            rules.push(json!({"type":"field","ip":["127.0.0.153/32"],"outboundTag":"direct"}));
        }
        let tun_dns_tags: Vec<_> = model
            .inbounds
            .iter()
            .filter(|i| i.enabled && i.kind == InboundKind::Tun && i.tun.dns_hijack)
            .map(|i| &i.tag)
            .collect();
        if !tun_dns_tags.is_empty() && model.dns.enabled {
            rules.push(json!({"type":"field", "inboundTag":tun_dns_tags, "network":"tcp,udp", "port":"53", "outboundTag":"dns-out"}));
        }
        if model.dns.enabled {
            rules.push(
                json!({"type": "field", "network": "udp", "port": "53", "outboundTag": "dns-out"}),
            );
        }
        if model.routing.bypass_lan {
            rules.push(json!({
                "type": "field",
                "ip": ["geoip:private"],
                "outboundTag": "direct"
            }));
        }
        if model.routing.bypass_china_ips {
            rules.push(json!({
                "type": "field",
                "ip": ["geoip:cn"],
                "outboundTag": "direct"
            }));
        }
        for rule in model.routing.effective_rules() {
            if let Some(value) = xray_rule(model, rule, &mut warnings) {
                rules.push(value);
            }
        }
        let final_tag = action_tag(CoreKind::Xray, model, &model.routing.final_action)
            .unwrap_or_else(|| "direct".into());
        // Xray has no "final" field: append a catch-all rule.
        rules.push(
            json!({"type": "field", "network": "tcp,udp", (target_field(&final_tag)): final_tag}),
        );

        let mut routing = json!({
            "domainStrategy": xray_domain_strategy(model.routing.domain_strategy),
            "rules": rules
        });
        if !balancers.is_empty() {
            routing["balancers"] = json!(balancers);
        }

        let mut root = Map::new();
        root.insert(
            "log".into(),
            json!({
                "loglevel": ctx.log_level.xray(),
                "access": "none",
                "error": ""
            }),
        );
        root.insert(
            "api".into(),
            json!({"tag": "api", "services": ["StatsService"]}),
        );
        root.insert("stats".into(), json!({}));
        root.insert(
            "policy".into(),
            json!({
                "levels": {"0": {"statsUserUplink": true, "statsUserDownlink": true}},
                "system": {
                    "statsInboundUplink": true,
                    "statsInboundDownlink": true,
                    "statsOutboundUplink": true,
                    "statsOutboundDownlink": true
                }
            }),
        );
        root.insert("inbounds".into(), json!(inbounds));
        root.insert("outbounds".into(), json!(outbounds));
        root.insert("routing".into(), routing);

        if observatory_needed {
            root.insert(
                "observatory".into(),
                json!({
                    "subjectSelector": ["node-"],
                    "probeUrl": ctx.urltest_url,
                    "probeInterval": format!("{}s", settings.urltest_interval_secs.max(30)),
                    "enableConcurrency": true
                }),
            );
        }

        if model.dns.enabled {
            let mut dns_servers = Vec::new();
            for server in &model.dns.servers {
                if let Some(address) = xray_dns_address(server, &mut warnings) {
                    let address = if !tun_dns_tags.is_empty()
                        && matches!(address.as_str(), "localhost" | "system" | "local")
                    {
                        warnings.push("TUN DNS 已将系统解析器替换为 1.1.1.1，避免 DNS 回环".into());
                        "1.1.1.1".to_string()
                    } else {
                        address
                    };
                    let mut entry = json!({"address": address});
                    if !server.domains.is_empty() {
                        entry["domains"] = json!(
                            server
                                .domains
                                .iter()
                                .map(|d| {
                                    if d.starts_with("geosite:")
                                        || d.starts_with("domain:")
                                        || d.starts_with("full:")
                                    {
                                        d.clone()
                                    } else if d.starts_with('.') {
                                        format!("domain:{}", d.trim_start_matches('.'))
                                    } else {
                                        d.clone()
                                    }
                                })
                                .collect::<Vec<_>>()
                        );
                    }
                    dns_servers.push(entry);
                }
            }
            // Xray uses the last server as the fallback.
            let final_address = model
                .dns
                .final_server
                .as_ref()
                .and_then(|tag| model.dns.servers.iter().find(|s| &s.tag == tag))
                .and_then(|s| xray_dns_address(s, &mut warnings))
                .or_else(|| Some("localhost".into()))
                .map(|address| {
                    if !tun_dns_tags.is_empty()
                        && matches!(address.as_str(), "localhost" | "system" | "local")
                    {
                        "1.1.1.1".into()
                    } else {
                        address
                    }
                });
            if let Some(address) = final_address {
                dns_servers.push(json!({"address": address}));
            }
            let mut dns = json!({
                "servers": dns_servers,
                "queryStrategy": xray_query_strategy(model.routing.domain_strategy)
            });
            if !model.dns.hosts.is_empty() {
                dns["hosts"] = json!(model.dns.hosts);
            }
            root.insert("dns".into(), dns);
        }

        if !ctx.mesh.dns.is_empty() {
            let dns = root
                .entry("dns")
                .or_insert_with(|| json!({"servers":["localhost"]}));
            let mut servers: Vec<Value> = ctx.mesh.dns.iter().map(|d|json!({"address":d.address,"port":53,"domains":[format!("domain:{}",d.suffix)],"skipFallback":true})).collect();
            if let Some(existing) = dns["servers"].as_array() {
                servers.extend(existing.iter().cloned());
            }
            dns["servers"] = json!(servers);
        }
        let default_outbound = final_tag;
        Ok(CompiledConfig {
            core: CoreKind::Xray,
            json: Value::Object(root),
            warnings,
            inbounds: compiled_inbounds,
            managed_groups: model.managed_route_groups(),
            core_version: ctx.core_version.clone(),
            rule_sets: model.routing.rule_sets.clone(),
            default_outbound,
        })
    }
}

fn xray_domain_strategy(strategy: DomainStrategy) -> &'static str {
    match strategy {
        DomainStrategy::IpIfNonMatch => "IPIfNonMatch",
        DomainStrategy::IpOnDemand => "IPOnDemand",
        _ => "AsIs",
    }
}

fn xray_query_strategy(strategy: DomainStrategy) -> &'static str {
    match strategy {
        DomainStrategy::PreferIpv4 => "UseIPv4",
        DomainStrategy::PreferIpv6 => "UseIPv6",
        DomainStrategy::AsIs => "AsIs",
        _ => "UseIP",
    }
}

fn socks_inbound(
    inbound: &Inbound,
    account: Option<&crate::settings::ProxyAccount>,
) -> Result<Value> {
    if inbound.port == 0 {
        return Err(Error::config(format!("入站 {} 端口不能为 0", inbound.tag)));
    }
    let mut options = json!({"auth": "noauth", "udp": inbound.udp, "userLevel": 0});
    if let Some(a) = account {
        options["auth"] = json!("password");
        options["accounts"] = json!([{"user":a.username, "pass":a.password}]);
    }
    Ok(json!({
        "tag": inbound.tag,
        "listen": inbound.listen_address(),
        "port": inbound.port,
        "protocol": "socks",
        "settings": options,
        "sniffing": sniffing(inbound)
    }))
}

fn http_inbound(
    inbound: &Inbound,
    port: u16,
    account: Option<&crate::settings::ProxyAccount>,
) -> Result<Value> {
    if port == 0 {
        return Err(Error::config(format!("入站 {} 端口不能为 0", inbound.tag)));
    }
    let mut options = json!({"timeout":300});
    if let Some(a) = account {
        options["accounts"] = json!([{"user":a.username, "pass":a.password}]);
    }
    Ok(json!({
        "tag": if port == inbound.port { inbound.tag.clone() } else { format!("{}-http", inbound.tag) },
        "listen": inbound.listen_address(),
        "port": port,
        "protocol": "http",
        "settings": options,
        "sniffing": sniffing(inbound)
    }))
}

fn sniffing(inbound: &Inbound) -> Value {
    if inbound.sniff {
        json!({"enabled": true, "destOverride": ["http", "tls", "quic"], "routeOnly": false})
    } else {
        json!({"enabled": false})
    }
}

fn transparent_inbound(inbound: &Inbound) -> Value {
    let follow = inbound.kind == InboundKind::Tproxy;
    let mut value = json!({
        "tag": inbound.tag,
        "listen": inbound.listen_address(),
        "port": inbound.port,
        "protocol": "dokodemo-door",
        "settings": {
            "network": if follow { "tcp,udp" } else { "tcp" },
            "followRedirect": true
        },
        "sniffing": sniffing(inbound),
        "streamSettings": {"sockopt": {"tproxy": if follow { "tproxy" } else { "redirect" }}}
    });
    if !follow {
        value["settings"]["network"] = json!("tcp");
    }
    value
}

/// Subtract excluded prefixes from the captured routes, including mesh networks.
fn subtract_prefix(network: ipnet::IpNet, excluded: ipnet::IpNet, result: &mut Vec<ipnet::IpNet>) {
    if excluded.contains(&network.network()) && excluded.prefix_len() <= network.prefix_len() {
        return;
    }
    if !network.contains(&excluded.network()) {
        result.push(network);
        return;
    }
    let prefix = network.prefix_len() + 1;
    let first = network.network();
    let second = match first {
        std::net::IpAddr::V4(ip) => {
            std::net::IpAddr::V4((u32::from(ip) | (1 << (32 - prefix))).into())
        }
        std::net::IpAddr::V6(ip) => {
            std::net::IpAddr::V6((u128::from(ip) | (1 << (128 - prefix))).into())
        }
    };
    for address in [first, second] {
        subtract_prefix(
            ipnet::IpNet::new(address, prefix).expect("valid child prefix"),
            excluded,
            result,
        );
    }
}

fn tun_inbound(
    inbound: &Inbound,
    ctx: &CompileContext,
    dns_enabled: bool,
    warnings: &mut Vec<String>,
) -> Result<Value> {
    let tun = &inbound.tun;
    let mut gateways = Vec::new();
    let mut dns = Vec::new();
    for address in [&tun.inet4_address, &tun.inet6_address]
        .into_iter()
        .filter(|s| !s.is_empty())
    {
        let net: ipnet::IpNet = address
            .parse()
            .map_err(|_| Error::config("TUN 地址必须为有效 IP/CIDR"))?;
        gateways.push(address.clone());
        let candidate = match net.network() {
            std::net::IpAddr::V4(ip) => {
                let host = u32::from(ip).checked_add(
                    if u32::from(ip)
                        .checked_add(1)
                        .map(std::net::Ipv4Addr::from)
                        .map(std::net::IpAddr::V4)
                        == Some(net.addr())
                    {
                        2
                    } else {
                        1
                    },
                );
                host.map(|v| std::net::IpAddr::V4(v.into()))
            }
            std::net::IpAddr::V6(ip) => {
                let host = u128::from(ip).checked_add(
                    if u128::from(ip)
                        .checked_add(1)
                        .map(std::net::Ipv6Addr::from)
                        .map(std::net::IpAddr::V6)
                        == Some(net.addr())
                    {
                        2
                    } else {
                        1
                    },
                );
                host.map(|v| std::net::IpAddr::V6(v.into()))
            }
        }
        .filter(|ip| *ip != net.addr() && net.contains(ip));
        if tun.dns_hijack && dns_enabled {
            dns.push(
                candidate
                    .ok_or_else(|| Error::config("TUN 地址段需要保留一个 DNS 地址，请扩大子网"))?
                    .to_string(),
            );
        }
    }
    if gateways.is_empty() {
        return Err(Error::config("TUN 至少需要一个 IPv4 或 IPv6 地址段"));
    }
    let mut settings = json!({"mtu":tun.mtu, "gateway":gateways, "autoOutboundsInterface":"auto"});
    // Let Xray choose an available utun name on macOS; other platforms use a stable name.
    if let Some(name) = tun.interface_name.as_deref().filter(|s| !s.is_empty()) {
        settings["name"] = json!(name);
    } else if !cfg!(target_os = "macos") {
        settings["name"] = json!("xross-tun");
    }
    if tun.auto_route {
        let mut routes: Vec<ipnet::IpNet> = Vec::new();
        if !tun.inet4_address.is_empty() {
            routes.push("0.0.0.0/0".parse().unwrap());
        }
        if !tun.inet6_address.is_empty() {
            routes.push("::/0".parse().unwrap());
        }
        let mut excludes = tun.route_exclude_addresses.clone();
        excludes.extend(ctx.mesh.routes.iter().map(|r| r.cidr.clone()));
        if ctx.mesh.active {
            excludes.push("127.0.0.153/32".into());
        }
        for cidr in excludes {
            let excluded = cidr
                .parse()
                .map_err(|_| Error::config(format!("TUN 排除路由无效：{cidr}")))?;
            let mut next = Vec::new();
            for network in routes {
                subtract_prefix(network, excluded, &mut next);
            }
            routes = next;
        }
        settings["autoSystemRoutingTable"] =
            json!(routes.iter().map(ToString::to_string).collect::<Vec<_>>());
    }
    if tun.dns_hijack && dns_enabled {
        settings["dns"] = json!(dns);
    }
    if tun.dns_hijack && !dns_enabled {
        warnings.push("未启用 DNS 配置，TUN DNS 劫持已停用".into());
    }
    if tun.strict_route {
        warnings.push("Xray TUN 已使用自动路由；不支持 sing-box 严格路由选项".into());
    }
    if tun.stack != crate::model::TunStack::Mixed {
        warnings.push("Xray TUN 使用内置网络栈，已忽略网络栈选择".into());
    }
    if tun.dns_hijack && !cfg!(windows) {
        warnings.push("Xray TUN 在当前平台不自动修改系统 DNS，请确认系统 DNS 设置".into());
    }
    Ok(
        json!({"tag":inbound.tag, "protocol":"tun", "settings":settings, "sniffing":sniffing(inbound)}),
    )
}

/// Minimal configuration used only by private per-node probes.
pub(super) fn compile_probe(node: &Node, port: u16, front: Option<&Node>) -> Result<Value> {
    let mut outbounds = vec![chained_outbound(node, front, &mut Vec::new())?];
    if let Some(front) = front.filter(|f| node.needs_front(f)) {
        outbounds.push(node_outbound(front, &mut Vec::new())?);
    }
    Ok(json!({
        "log": {"loglevel": "none"},
        "inbounds": [{"protocol": "http", "tag": "probe", "listen": "127.0.0.1", "port": port, "settings": {}}],
        "outbounds": outbounds
    }))
}

/// Bind probe sockets before they can be captured by a running TUN.
pub(crate) fn bind_probe_interface(config: &mut Value, interface: &str) {
    let first_hop = config["outbounds"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    first_hop["streamSettings"]["sockopt"]["interface"] = json!(interface);
}

fn chained_outbound(
    node: &Node,
    front: Option<&Node>,
    warnings: &mut Vec<String>,
) -> Result<Value> {
    let mut outbound = node_outbound(node, warnings)?;
    if let Some(front) = front.filter(|f| node.needs_front(f)) {
        outbound["streamSettings"]["sockopt"]["dialerProxy"] = json!(front.outbound_tag());
    }
    Ok(outbound)
}

fn node_outbound(node: &Node, warnings: &mut Vec<String>) -> Result<Value> {
    let address = &node.server;
    let port = node.port;
    let cred = &node.credentials;
    let mut outbound = match node.protocol {
        Protocol::Vmess => {
            let uuid = cred
                .uuid
                .as_ref()
                .ok_or_else(|| Error::config("缺少 uuid"))?;
            json!({
                "tag": node.outbound_tag(),
                "protocol": "vmess",
                "settings": {
                    "vnext": [{
                        "address": address,
                        "port": port,
                        "users": [{
                            "id": uuid,
                            "alterId": cred.alter_id,
                            "security": "auto",
                            "level": 0
                        }]
                    }]
                }
            })
        }
        Protocol::Vless => {
            let uuid = cred
                .uuid
                .as_ref()
                .ok_or_else(|| Error::config("缺少 uuid"))?;
            let mut user = json!({
                "id": uuid,
                "encryption": "none",
                "level": 0
            });
            if let Some(flow) = &cred.flow {
                if !flow.is_empty() {
                    user["flow"] = json!(flow);
                }
            }
            json!({
                "tag": node.outbound_tag(),
                "protocol": "vless",
                "settings": {"vnext": [{"address": address, "port": port, "users": [user]}]}
            })
        }
        Protocol::Trojan => {
            let password = cred
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            json!({
                "tag": node.outbound_tag(),
                "protocol": "trojan",
                "settings": {"servers": [{"address": address, "port": port, "password": password, "level": 0}]}
            })
        }
        Protocol::Shadowsocks => {
            let method = cred
                .method
                .as_ref()
                .ok_or_else(|| Error::config("缺少加密方式"))?;
            let password = cred
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            json!({
                "tag": node.outbound_tag(),
                "protocol": "shadowsocks",
                "settings": {"servers": [{
                    "address": address,
                    "port": port,
                    "method": method,
                    "password": password,
                    "uot": true,
                    "level": 0
                }]}
            })
        }
        Protocol::Socks => json!({
            "tag": node.outbound_tag(),
            "protocol": "socks",
            "settings": {"servers": [{
                "address": address,
                "port": port,
                "users": auth_users(cred)
            }]}
        }),
        Protocol::Http => json!({
            "tag": node.outbound_tag(),
            "protocol": "http",
            "settings": {"servers": [{
                "address": address,
                "port": port,
                "users": auth_users(cred)
            }]}
        }),
        Protocol::Wireguard => {
            let wg = cred
                .wireguard
                .as_ref()
                .ok_or_else(|| Error::config("WireGuard 缺少密钥和地址配置"))?;
            wg.validate()?;
            return Ok(
                json!({"tag":node.outbound_tag(),"protocol":"wireguard","settings":{
                "secretKey":wg.private_key,"address":wg.addresses,"mtu":wg.mtu,"reserved":wg.reserved,"noKernelTun":true,
                "peers":[{"endpoint":format!("{}:{}",crate::subscription::links::format_host(address),port),
                    "publicKey":wg.public_key,"preSharedKey":wg.pre_shared_key,"allowedIPs":wg.allowed_ips}]}}),
            );
        }
        Protocol::Hysteria2
        | Protocol::Tuic
        | Protocol::Anytls
        | Protocol::Openvpn
        | Protocol::Openconnect => {
            return Err(Error::other(format!(
                "{} 仅 sing-box 支持",
                node.protocol.display_name()
            )));
        }
    };

    // Stream settings (TLS / transport).
    if node.protocol != Protocol::Shadowsocks
        && node.protocol != Protocol::Socks
        && node.protocol != Protocol::Http
    {
        let stream = stream_settings(node, warnings)?;
        if !stream.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            outbound["streamSettings"] = stream;
        }
    }
    Ok(outbound)
}

fn auth_users(cred: &crate::model::Credentials) -> Value {
    match (&cred.username, &cred.password) {
        (Some(user), Some(pass)) => json!([{"user": user, "pass": pass}]),
        (None, Some(pass)) => json!([{"user": "", "pass": pass}]),
        _ => json!([]),
    }
}

fn stream_settings(node: &Node, warnings: &mut Vec<String>) -> Result<Value> {
    let mut stream = Map::new();
    let (network, settings) = match &node.transport {
        Transport::Tcp => ("tcp", Value::Null),
        Transport::WebSocket {
            path,
            host,
            headers,
        } => {
            let mut value = json!({"path": path});
            if let Some(host) = host {
                if !host.is_empty() {
                    value["headers"] = json!({"Host": host});
                }
            }
            if !headers.is_empty() {
                let mut map = value["headers"].as_object().cloned().unwrap_or_default();
                for (k, v) in headers {
                    map.insert(k.clone(), json!(v));
                }
                value["headers"] = Value::Object(map);
            }
            ("ws", value)
        }
        Transport::Grpc { service_name } => ("grpc", json!({"serviceName": service_name})),
        Transport::Http2 { path, host } => {
            let mut value = json!({"path": path});
            if let Some(host) = host {
                value["host"] = json!([host]);
            }
            ("http", value)
        }
        Transport::HttpUpgrade { path, host } => {
            let mut value = json!({"path": path});
            if let Some(host) = host {
                value["host"] = json!(host);
            }
            ("httpupgrade", value)
        }
        Transport::Quic => ("quic", json!({"security": "none", "key": ""})),
        Transport::Kcp { seed } => {
            let mut value = json!({"header": {"type": "none"}});
            if let Some(seed) = seed {
                value["seed"] = json!(seed);
            }
            ("kcp", value)
        }
    };
    stream.insert("network".into(), json!(network));
    if !settings.is_null() {
        stream.insert(format!("{network}Settings"), settings);
    }

    if let Some(reality) = &node.tls.reality {
        stream.insert("security".into(), json!("reality"));
        let mut reality_settings = json!({
            "fingerprint": node.tls.fingerprint.clone().unwrap_or_else(|| "chrome".into()),
            "publicKey": reality.public_key,
            "shortId": reality.short_id,
            "spiderX": "/"
        });
        if let Some(sni) = &node.tls.server_name {
            reality_settings["serverName"] = json!(sni);
        }
        stream.insert("realitySettings".into(), reality_settings);
    } else if node.tls.enabled {
        stream.insert("security".into(), json!("tls"));
        let mut tls = json!({"allowInsecure": node.tls.insecure});
        if let Some(sni) = &node.tls.server_name {
            tls["serverName"] = json!(sni);
        }
        if !node.tls.alpn.is_empty() {
            tls["alpn"] = json!(node.tls.alpn);
        }
        if let Some(fp) = &node.tls.fingerprint {
            tls["fingerprint"] = json!(fp);
        }
        stream.insert("tlsSettings".into(), tls);
    }
    if let Transport::Kcp { .. } = node.transport {
        warnings.push(format!("节点“{}”使用 KCP，Xray 支持有限", node.name));
    }
    Ok(Value::Object(stream))
}

/// Only native balancers resolve to group tags; managed and manual groups resolve to node tags.
fn target_field(tag: &str) -> &'static str {
    if tag.starts_with("group-") {
        "balancerTag"
    } else {
        "outboundTag"
    }
}

fn xray_rule(model: &AppModel, rule: &RouteRule, warnings: &mut Vec<String>) -> Option<Value> {
    let mut object = Map::new();
    object.insert("type".into(), json!("field"));
    let m = &rule.matchers;

    let mut domains: Vec<String> = Vec::new();
    domains.extend(m.domains.iter().map(|d| format!("full:{d}")));
    domains.extend(m.domain_suffixes.iter().map(|d| format!("domain:{d}")));
    domains.extend(m.domain_keywords.iter().map(|d| format!("keyword:{d}")));
    domains.extend(m.domain_regexes.iter().map(|d| format!("regexp:{d}")));
    domains.extend(m.geosites.iter().map(|g| format!("geosite:{g}")));
    for tag in &m.rule_sets {
        if let Some(geosite) = tag.strip_prefix("geosite-") {
            domains.push(format!("geosite:{geosite}"));
        } else if tag.starts_with("geoip-") {
            // handled as ip below
        } else {
            warnings.push(format!(
                "路由规则“{}”引用了自定义 rule-set“{tag}”，Xray 不支持，已忽略",
                rule.name
            ));
        }
    }
    if !domains.is_empty() {
        object.insert("domain".into(), json!(domains));
    }

    let mut ips: Vec<String> = m.ip_cidrs.clone();
    ips.extend(m.geoips.iter().map(|g| format!("geoip:{g}")));
    for tag in &m.rule_sets {
        if let Some(geoip) = tag.strip_prefix("geoip-") {
            ips.push(format!("geoip:{geoip}"));
        }
    }
    if !ips.is_empty() {
        object.insert("ip".into(), json!(ips));
    }
    if !m.source_ip_cidrs.is_empty() {
        object.insert("sourceIP".into(), json!(m.source_ip_cidrs));
    }
    if !m.ports.is_empty() {
        object.insert("port".into(), json!(m.ports.join(",")));
    }
    if !m.source_ports.is_empty() {
        object.insert("sourcePort".into(), json!(m.source_ports.join(",")));
    }
    if let Some(network) = network_str(m) {
        object.insert("network".into(), json!(network));
    }
    if !m.process_names.is_empty() {
        object.insert("process".into(), json!(m.process_names));
    }
    if !m.inbound_tags.is_empty() {
        object.insert("inboundTag".into(), json!(m.inbound_tags));
    }
    if !m.protocols.is_empty() {
        object.insert("protocol".into(), json!(m.protocols));
    }
    if object.len() == 1 {
        return None;
    }
    let target = action_tag(CoreKind::Xray, model, &rule.action)?;
    object.insert(target_field(&target).into(), json!(target));
    Some(Value::Object(object))
}

#[cfg(test)]
mod tests {
    #[test]
    fn managed_route_pins_an_eligible_node_and_keeps_other_groups_idle() {
        let (mut model, settings, ctx) = sample();
        let unavailable = model.nodes[0].id.clone();
        model.nodes[0].enabled = false;
        let valid = Node::new("可用节点", Protocol::Socks, "candidate.example", 1080);
        model.add_nodes([valid.clone()]);
        for g in &mut model.groups {
            g.kind = GroupKind::Fallback;
            g.selected = Some(unavailable.clone());
        }
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert_eq!(compiled.managed_groups.len(), 1);
        assert_eq!(compiled.managed_groups[0].id, "proxy");
        assert_eq!(compiled.managed_groups[0].members, [valid.id.clone()]);
        assert_eq!(
            compiled.json["routing"]["rules"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["outboundTag"],
            valid.outbound_tag()
        );
        assert!(compiled.json["routing"].get("balancers").is_none());
    }

    #[test]
    fn wireguard_compiles_with_credentials_and_probe() {
        let (mut model, settings, ctx) = sample();
        let mut wg = Node::new("fixture wg", Protocol::Wireguard, "2001:db8::1", 51820);
        wg.credentials.wireguard = Some(crate::model::WireguardOptions {
            private_key: crate::util::encode_base64(&[1; 32]),
            public_key: crate::util::encode_base64(&[2; 32]),
            pre_shared_key: crate::util::encode_base64(&[3; 32]),
            addresses: vec!["10.0.0.2/32".into(), "fd00::2/128".into()],
            reserved: [1, 2, 3],
            ..Default::default()
        });
        model.nodes.clear();
        for group in &mut model.groups {
            group.members.clear();
            group.selected = None;
        }
        model.add_nodes([wg.clone()]);
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let probe = super::super::compile_probe(CoreKind::Xray, &wg, 12345, None).unwrap();
        let out = compiled.json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["protocol"] == "wireguard")
            .unwrap();
        assert_eq!(
            out["settings"]["secretKey"],
            wg.credentials.wireguard.as_ref().unwrap().private_key
        );
        assert_eq!(
            out["settings"]["peers"][0]["endpoint"],
            "[2001:db8::1]:51820"
        );
        assert_eq!(
            out["settings"]["peers"][0]["preSharedKey"],
            wg.credentials.wireguard.as_ref().unwrap().pre_shared_key
        );
        assert_eq!(probe["outbounds"][0], *out);
    }
    #[test]
    fn custom_group_order_and_enabled_state_reach_compiler() {
        let (mut model, settings, ctx) = sample();
        model.routing.rules.clear();
        for (name, enabled) in [
            ("first.example", true),
            ("disabled.example", false),
            ("last.example", true),
        ] {
            model
                .routing
                .rule_groups
                .push(crate::model::RouteRuleGroup {
                    name: name.into(),
                    enabled,
                    rules: vec![crate::model::RouteRule::new(
                        name,
                        crate::model::Matchers {
                            domains: vec![name.into()],
                            ..Default::default()
                        },
                        crate::model::RouteAction::direct(),
                    )],
                    ..Default::default()
                });
        }
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let rules = compiled.json["routing"]["rules"].as_array().unwrap();
        let domains: Vec<_> = rules
            .iter()
            .filter_map(|r| r["domain"][0].as_str())
            .collect();
        assert_eq!(domains, ["full:first.example", "full:last.example"]);
    }

    #[test]
    fn native_tun_compiles_without_proxy_ports_and_routes_dns_before_lan() {
        let (mut model, settings, ctx) = sample();
        let mut tun = Inbound::new("tun-fixture", InboundKind::Tun, 0);
        tun.tun.interface_name = Some("xross-test".into());
        model.inbounds = vec![tun];
        assert!(model.validate(CoreKind::Xray).is_empty());
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let inbound = &compiled.json["inbounds"][0];
        assert_eq!(inbound["protocol"], "tun");
        assert!(inbound.get("port").is_none());
        assert_eq!(inbound["settings"]["name"], "xross-test");
        assert_eq!(
            inbound["settings"]["gateway"],
            json!(["172.19.0.1/30", "fdfe:dcba:9876::1/126"])
        );
        assert_eq!(
            inbound["settings"]["dns"],
            json!(["172.19.0.2", "fdfe:dcba:9876::2"])
        );
        assert_eq!(
            inbound["settings"]["autoSystemRoutingTable"],
            json!(["0.0.0.0/0", "::/0"])
        );
        assert_eq!(inbound["settings"]["autoOutboundsInterface"], "auto");
        assert_eq!(compiled.inbounds[0].kind, "tun");
        let rules = compiled.json["routing"]["rules"].as_array().unwrap();
        let dns = rules
            .iter()
            .position(|r| {
                r["inboundTag"] == json!(["tun-fixture"]) && r["outboundTag"] == "dns-out"
            })
            .unwrap();
        let lan = rules
            .iter()
            .position(|r| r["ip"] == json!(["geoip:private"]))
            .unwrap();
        assert!(dns < lan);
        assert_eq!(rules[dns]["network"], "tcp,udp");
        assert!(
            compiled.json["dns"]["servers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| !matches!(
                    r["address"].as_str(),
                    Some("localhost" | "system" | "local")
                ))
        );
    }

    #[test]
    fn tun_subtracts_exclusions_and_mesh_routes_in_both_ip_families() {
        let (mut model, settings, mut ctx) = sample();
        let mut tun = Inbound::new("tun", InboundKind::Tun, 0);
        tun.tun.route_exclude_addresses = vec!["192.0.2.0/24".into(), "2001:db8::/32".into()];
        ctx.mesh.active = true;
        ctx.mesh.routes.push(crate::mesh::coexist::MeshRoute {
            cidr: "100.64.0.0/10".into(),
            interface: "mesh-test".into(),
        });
        model.inbounds = vec![tun];
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let routes: Vec<ipnet::IpNet> =
            compiled.json["inbounds"][0]["settings"]["autoSystemRoutingTable"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().parse().unwrap())
                .collect();
        for address in ["192.0.2.5", "2001:db8::1", "100.64.0.2", "127.0.0.153"] {
            let ip: std::net::IpAddr = address.parse().unwrap();
            assert!(
                !routes.iter().any(|r| r.contains(&ip)),
                "excluded: {address}"
            );
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let ip: std::net::IpAddr = address.parse().unwrap();
            assert!(routes.iter().any(|r| r.contains(&ip)));
        }
    }

    #[test]
    fn tun_manual_routing_disabled_dns_and_invalid_prefixes() {
        let (mut model, settings, ctx) = sample();
        let mut tun = Inbound::new("tun", InboundKind::Tun, 0);
        tun.tun.auto_route = false;
        model.inbounds = vec![tun];
        model.dns.enabled = false;
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(
            compiled.json["inbounds"][0]["settings"]
                .get("autoSystemRoutingTable")
                .is_none()
        );
        assert!(
            compiled.json["inbounds"][0]["settings"]
                .get("dns")
                .is_none()
        );
        assert!(
            compiled
                .warnings
                .iter()
                .any(|w| w.contains("DNS 劫持已停用"))
        );
        model.inbounds[0].tun.auto_route = true;
        model.inbounds[0]
            .tun
            .route_exclude_addresses
            .push("invalid".into());
        assert!(XrayAdapter.compile(&model, &settings, &ctx).is_err());
        model.inbounds[0].tun.route_exclude_addresses.clear();
        model.inbounds[0].tun.inet4_address = "255.255.255.255/32".into();
        model.dns.enabled = true;
        assert!(XrayAdapter.compile(&model, &settings, &ctx).is_err());
    }

    #[test]
    fn xray_probe_binds_only_the_requested_node_to_the_uplink() {
        let (model, _, _) = sample();
        let mut config =
            super::super::compile_probe(CoreKind::Xray, &model.nodes[0], 12345, None).unwrap();
        bind_probe_interface(&mut config, "test-uplink");
        assert_eq!(config["outbounds"].as_array().unwrap().len(), 1);
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["sockopt"]["interface"],
            "test-uplink"
        );
        assert!(
            config["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["protocol"] != "tun")
        );
    }

    #[test]
    fn anytls_is_skipped_with_warning_and_probe_never_falls_back() {
        let (mut model, settings, ctx) = sample();
        let mut node = Node::new("AnyTLS fixture", Protocol::Anytls, "example.com", 443);
        node.credentials.password = Some("fake".into());
        model.add_nodes(vec![node.clone()]);
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(compiled.warnings.iter().any(|w| w.contains("AnyTLS")));
        assert!(
            !compiled.json["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["tag"] == node.outbound_tag())
        );
        assert!(super::super::compile_probe(CoreKind::Xray, &node, 12345, None).is_err());
        let probe =
            super::super::compile_probe(CoreKind::Xray, &model.nodes[0], 12345, None).unwrap();
        assert_eq!(probe["outbounds"].as_array().unwrap().len(), 1);
        assert_eq!(probe["outbounds"][0]["tag"], model.nodes[0].outbound_tag());
        assert_eq!(probe["inbounds"][0]["listen"], "127.0.0.1");
        assert!(probe.get("routing").is_none());
        assert!(probe.get("api").is_none());
    }

    use super::*;
    use crate::model::AppModel;
    use crate::settings::Settings;

    fn sample() -> (AppModel, Settings, CompileContext) {
        let mut model = AppModel::default();
        let mut node = crate::model::Node::new("n", Protocol::Vless, "example.com", 443);
        node.credentials.uuid = Some("11111111-2222-3333-4444-555555555555".into());
        node.tls.enabled = true;
        model.add_nodes(vec![node]);
        let settings = Settings::default();
        let ctx = CompileContext::new(
            &settings,
            std::path::PathBuf::from("/tmp/xross-test-data"),
            std::path::PathBuf::from("/tmp/xross-test-run"),
        );
        (model, settings, ctx)
    }

    #[test]
    fn mesh_domains_and_addresses_precede_proxy_rules() {
        let (model, settings, mut ctx) = sample();
        ctx.mesh.active = true;
        ctx.mesh.routes.push(crate::mesh::coexist::MeshRoute {
            cidr: "100.64.0.0/10".into(),
            interface: "mesh-test".into(),
        });
        ctx.mesh.dns.push(crate::mesh::coexist::MeshDns {
            suffix: "example.ts.net".into(),
            address: "127.0.0.153".into(),
        });
        let config = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert_eq!(
            config.json["routing"]["rules"][1]["ip"],
            json!(["100.64.0.0/10"])
        );
        assert_eq!(
            config.json["routing"]["rules"][2]["domain"],
            json!(["domain:example.ts.net"])
        );
        assert_eq!(
            config.json["dns"]["servers"][0]["domains"],
            json!(["domain:example.ts.net"])
        );
    }

    #[test]
    fn splits_mixed_and_adds_api() {
        let (model, settings, ctx) = sample();
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let tags: Vec<&str> = compiled.json["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["tag"].as_str())
            .collect();
        assert!(tags.contains(&"mixed-in"));
        assert!(tags.contains(&"mixed-in-http"));
        assert!(tags.contains(&"api-in"));
        assert_eq!(compiled.http_port(), 7891);
        assert!(compiled.warnings.iter().any(|w| w.contains("mixed")));
    }

    #[test]
    fn uses_balancer_for_urltest() {
        let (mut model, settings, ctx) = sample();
        model.default_group = Some("auto".into());
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let balancers = compiled.json["routing"]["balancers"].as_array().unwrap();
        assert!(balancers.iter().any(|b| b["tag"] == "group-auto"));
        let rules = compiled.json["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules.last().unwrap()["balancerTag"], "group-auto");
        assert!(rules.iter().all(|r| r["outboundTag"] != "group-auto"));
        assert!(compiled.json["observatory"].is_object());
    }

    #[test]
    fn skips_fakeip_with_warning() {
        let (model, settings, ctx) = sample();
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(compiled.warnings.iter().any(|w| w.contains("fake-ip")));
        assert!(
            compiled.json["dns"]["servers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["address"] != "fakeip")
        );
    }
    #[test]
    fn compiles_separate_authenticated_lan_and_chains_only_other_nodes() {
        let (mut model, mut settings, ctx) = sample();
        let front = Node::new("fixture", Protocol::Socks, "proxy.invalid", 1080);
        model.add_nodes([front.clone()]);
        model.front_proxy = Some(front.id.clone());
        let password = uuid::Uuid::new_v4().to_string();
        settings.allow_lan = true;
        settings.separate_lan_port = true;
        settings.lan_auth = true;
        settings.lan_account = crate::settings::ProxyAccount {
            username: "fixture".into(),
            password: password.clone(),
        };
        settings.sync_inbounds(&mut model);
        let config = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let items = config.json["inbounds"].as_array().unwrap();
        for i in &items[..2] {
            assert_eq!(i["listen"], "127.0.0.1");
            assert!(i["settings"]["accounts"].is_null());
        }
        for i in &items[2..4] {
            assert_eq!(i["listen"], "0.0.0.0");
            assert_eq!(i["settings"]["accounts"][0]["pass"], password);
        }
        assert_eq!(items[2]["settings"]["auth"], "password");
        assert_eq!(items[3]["port"], settings.lan_port + 1);
        assert_eq!(
            config.json["outbounds"][0]["streamSettings"]["sockopt"]["dialerProxy"],
            front.outbound_tag()
        );
        assert!(config.json["outbounds"][1]["streamSettings"]["sockopt"]["dialerProxy"].is_null());
        let probe =
            super::super::compile_probe(CoreKind::Xray, &model.nodes[0], 12345, Some(&front))
                .unwrap();
        assert_eq!(probe["outbounds"].as_array().unwrap().len(), 2);
        settings.separate_lan_port = false;
        settings.sync_inbounds(&mut model);
        let shared = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        for i in &shared.json["inbounds"].as_array().unwrap()[..2] {
            assert_eq!(i["settings"]["accounts"][0]["pass"], password);
        }
        model.remove_node(&front.id);
        assert!(XrayAdapter.compile(&model, &settings, &ctx).is_err());
    }
}
