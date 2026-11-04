use std::path::Path;

use serde_json::{Map, Value, json};

use super::{
    CompileContext, CompiledConfig, CompiledInbound, CoreAdapter, action_tag, dns_server_rule,
    network_str, proxy_group_tag, resolve_rule_set, split_dns_domains,
};
use crate::error::{Error, Result};
use crate::model::{
    AppModel, CoreKind, DomainStrategy, GroupKind, Inbound, InboundKind, Node, Protocol, RuleSet,
    Transport,
};
use crate::settings::Settings;

pub(crate) mod vpn;

pub struct SingBoxAdapter;

/// Standalone mesh configuration; does not require a proxy profile or nodes.
pub fn compile_mesh(
    profile: &crate::mesh::MeshProfile,
    directory: &Path,
    api_port: u16,
    secret: &str,
) -> crate::Result<Value> {
    profile.validate()?;
    let mut routes = vec!["100.64.0.0/10".to_string(), "fd7a:115c:a1e0::/48".into()];
    routes.extend(profile.remote_subnets.clone());
    Ok(json!({
        "log":{"level":"error"},
        "endpoints":[{"type":"tailscale","tag":"mesh-ts","state_directory":directory.join("identity"),"hostname":profile.hostname,
            "control_url":profile.control_url,"system_interface":false,
            "accept_routes":!profile.remote_subnets.is_empty(),"advertise_routes":[],"advertise_exit_node":false}],
        "services":[{"type":"api","listen":"127.0.0.1","listen_port":api_port,"secret":secret,"dashboard":false}],
        "inbounds":[{"type":"direct","tag":"mesh-dns","listen":"127.0.0.153","listen_port":53},
            {"type":"tun","tag":"mesh-tun","interface_name":format!("xross-mesh-ts-{}",&profile.id[..8]),"address":[crate::mesh::TAILSCALE_TUN_ADDRESS,crate::mesh::TAILSCALE_TUN_IPV6_ADDRESS],"mtu":1280,"stack":"gvisor","auto_route":true,"strict_route":false,"route_address":routes}],
        "outbounds":[{"type":"direct","tag":"direct"}],
        "route":{"auto_detect_interface":true,"default_domain_resolver":"local","rules":[{"inbound":"mesh-dns","action":"hijack-dns"},{"inbound":"mesh-tun","action":"route","outbound":"mesh-ts"},{"inbound":"mesh-ts","action":"reject"}],"final":"direct"},
        "dns":{"servers":[{"type":"local","tag":"local"},{"type":"tailscale","tag":"mesh-dns","endpoint":"mesh-ts","accept_default_resolvers":false}],
            "rules":[{"inbound":"mesh-dns","action":"route","server":"mesh-dns"}],"final":"local"}
    }))
}

impl CoreAdapter for SingBoxAdapter {
    fn kind(&self) -> CoreKind {
        CoreKind::SingBox
    }

    fn check_args(&self, config_path: &Path) -> Vec<String> {
        vec![
            "check".into(),
            "-c".into(),
            config_path.display().to_string(),
        ]
    }

    fn compile(
        &self,
        model: &AppModel,
        _settings: &Settings,
        ctx: &CompileContext,
    ) -> Result<CompiledConfig> {
        let mut warnings = Vec::new();
        let mut inbounds = Vec::new();
        let mut compiled_inbounds = Vec::new();

        for inbound in model.inbounds.iter().filter(|i| i.enabled) {
            match inbound.kind {
                InboundKind::Tun => {
                    let mut tun = tun_inbound(inbound);
                    if ctx.mesh.active {
                        let mut excludes = inbound.tun.route_exclude_addresses.clone();
                        excludes.extend(ctx.mesh.routes.iter().map(|r| r.cidr.clone()));
                        excludes.push("127.0.0.153/32".into());
                        excludes.sort();
                        excludes.dedup();
                        tun["route_exclude_address"] = json!(excludes);
                        if cfg!(windows) && inbound.tun.strict_route {
                            tun["strict_route"] = json!(false);
                            warnings.push("异地组网运行期间暂时关闭 TUN strict_route，严格 DNS 防泄漏过滤已放宽；断开全部组网后恢复".into());
                        }
                    }
                    inbounds.push(tun);
                    compiled_inbounds.push(CompiledInbound {
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
                    inbounds.push(json!({
                        "type": inbound.kind.as_str(),
                        "tag": inbound.tag,
                        "listen": inbound.listen_address(),
                        "listen_port": inbound.port,
                        "network": if inbound.udp { "tcp,udp" } else { "tcp" }
                    }));
                    compiled_inbounds.push(CompiledInbound {
                        tag: inbound.tag.clone(),
                        kind: inbound.kind.as_str().into(),
                        port: inbound.port,
                        allow_lan: false,
                    });
                }
                kind => {
                    if inbound.port == 0 {
                        return Err(Error::config(format!("入站 {} 端口不能为 0", inbound.tag)));
                    }
                    let inbound_type = match kind {
                        InboundKind::Mixed => "mixed",
                        InboundKind::Socks => "socks",
                        InboundKind::Http => "http",
                        _ => unreachable!(),
                    };
                    let mut value = json!({
                        "type": inbound_type,
                        "tag": inbound.tag,
                        "listen": inbound.listen_address(),
                        "listen_port": inbound.port
                    });
                    // sing-box 1.11 removed `udp` from mixed/http; SOCKS keeps it.
                    if inbound.kind == InboundKind::Socks {
                        value["udp"] = json!(inbound.udp);
                    }
                    inbounds.push(value);
                    compiled_inbounds.push(CompiledInbound {
                        tag: inbound.tag.clone(),
                        kind: inbound_type.into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                }
            }
        }
        if compiled_inbounds.is_empty() {
            return Err(Error::config(
                "没有可用的入站（至少启用一个 mixed/socks/http/tun）",
            ));
        }

        // --- outbounds ---------------------------------------------------
        let mut outbounds = Vec::new();
        let mut endpoints = Vec::new();
        for node in &model.nodes {
            if !node.enabled {
                continue;
            }
            if !node.protocol.supported_by(CoreKind::SingBox) {
                warnings.push(format!(
                    "节点“{}”使用 {}，sing-box 编译器暂不支持，已跳过",
                    node.name,
                    node.protocol.display_name()
                ));
                continue;
            }
            if node.protocol.is_vpn()
                && ctx
                    .core_version
                    .as_ref()
                    .is_some_and(|v| v < &semver::Version::new(1, 14, 0))
            {
                warnings.push(format!(
                    "节点“{}”需要 sing-box 1.14.0 或更新版本，已跳过，请更新内核",
                    node.name
                ));
                continue;
            }
            match node_outbound(node, &mut warnings) {
                Ok(value) => {
                    if node.protocol == Protocol::Wireguard || node.protocol.is_vpn() {
                        endpoints.push(value);
                    } else {
                        outbounds.push(value);
                    }
                }
                Err(err) => warnings.push(format!("节点“{}”编译失败：{err}", node.name)),
            }
        }
        if outbounds.is_empty() && endpoints.is_empty() {
            return Err(Error::config(
                if model.nodes.iter().any(|n| n.enabled && n.protocol.is_vpn()) {
                    "没有可编译的出站节点；请检查 VPN 凭据并使用 sing-box 1.14.0 或更新版本"
                } else {
                    "没有可编译的出站节点"
                },
            ));
        }
        if model.nodes.iter().any(|n| n.enabled && n.protocol.is_vpn()) {
            warnings.push("OpenVPN / OpenConnect 需要 sing-box 1.14.0 或更新版本".into());
        }
        // `block` is a legacy special outbound; reject is expressed as a rule action.
        outbounds.push(json!({"type": "direct", "tag": "direct"}));

        // Resolve group selections against successfully compiled nodes only.
        let valid_tags: std::collections::HashSet<String> = outbounds
            .iter()
            .chain(endpoints.iter())
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

        let default_group =
            proxy_group_tag(model, &model.routing.final_action).unwrap_or_else(|| "direct".into());

        for group in model.groups.iter().filter(|g| g.enabled) {
            let members: Vec<String> = group
                .members
                .iter()
                .filter_map(|id| model.node(id))
                .filter(|n| n.usable_with(CoreKind::SingBox))
                .map(|n| n.outbound_tag())
                .filter(|tag| valid_tags.contains(tag))
                .collect();
            if members.is_empty() {
                if group.tag() == default_group {
                    return Err(Error::config(format!(
                        "默认出站组“{}”没有可用成员",
                        group.name
                    )));
                }
                warnings.push(format!("出站组“{}”没有可用成员，已跳过", group.name));
                continue;
            }
            let interval = format!("{}s", group.interval_secs.max(30));
            let mut value = match group.kind {
                GroupKind::Selector => json!({
                    "type": "selector",
                    "tag": group.tag(),
                    "outbounds": members,
                    "interrupt_exist_connections": true
                }),
                GroupKind::UrlTest => json!({
                    "type": "urltest",
                    "tag": group.tag(),
                    "outbounds": members,
                    "url": group.url.clone().unwrap_or_else(|| ctx.urltest_url.clone()),
                    "interval": interval,
                    "tolerance": group.tolerance_ms
                }),
                GroupKind::Fallback => {
                    warnings.push(format!(
                        "sing-box 没有 fallback 出站，“{}”已编译为 tolerance=0 的 urltest",
                        group.name
                    ));
                    json!({
                        "type": "urltest",
                        "tag": group.tag(),
                        "outbounds": members,
                        "url": group.url.clone().unwrap_or_else(|| ctx.urltest_url.clone()),
                        "interval": interval,
                        "tolerance": 0
                    })
                }
                GroupKind::LoadBalance => {
                    warnings.push(format!(
                        "sing-box 没有 loadbalance 出站，“{}”已编译为 selector",
                        group.name
                    ));
                    json!({
                        "type": "selector",
                        "tag": group.tag(),
                        "outbounds": members,
                        "interrupt_exist_connections": true
                    })
                }
            };
            if group.kind == GroupKind::Selector {
                if let Some(node_id) = group.active_node() {
                    if let Some(node) = model.node(node_id) {
                        value["default"] = json!(node.outbound_tag());
                    }
                }
            }
            outbounds.push(value);
        }

        // --- rule sets ---------------------------------------------------
        let mut rule_set_tags: Vec<String> = Vec::new();
        for rule in model.routing.effective_rules() {
            rule_set_tags.extend(rule.matchers.rule_sets.iter().cloned());
            for geosite in &rule.matchers.geosites {
                rule_set_tags.push(format!("geosite-{geosite}"));
            }
            for geoip in &rule.matchers.geoips {
                rule_set_tags.push(format!("geoip-{geoip}"));
            }
        }
        if model.dns.enabled {
            for server in &model.dns.servers {
                let sets = split_dns_domains(&server.domains);
                for geosite in sets.geosites {
                    rule_set_tags.push(format!("geosite-{geosite}"));
                }
                for tag in sets.rule_sets {
                    rule_set_tags.push(tag);
                }
            }
        }
        rule_set_tags.push("geosite-cn".into());
        rule_set_tags.push("geoip-cn".into());
        rule_set_tags.sort();
        rule_set_tags.dedup();

        let rule_sets_json =
            build_rule_sets(model, &rule_set_tags, ctx, &default_group, &mut warnings);

        // --- route -------------------------------------------------------
        let mut rules: Vec<Value> = Vec::new();
        if !ctx.mesh.dns.is_empty() {
            // MagicDNS returns real addresses before the ordinary fake-IP rules.
            rules.push(json!({"domain_suffix":ctx.mesh.dns.iter().map(|d| &d.suffix).collect::<Vec<_>>(),"action":"resolve","server":"xross-mesh-dns-0"}));
            rules.push(json!({"ip_cidr":["127.0.0.153/32"],"action":"route","outbound":"direct"}));
        }
        for (index, route) in ctx.mesh.routes.iter().enumerate() {
            let tag = format!("xross-mesh-{index}");
            outbounds.push(json!({"type":"direct","tag":tag,"bind_interface":route.interface}));
            rules.push(json!({"ip_cidr":[route.cidr],"action":"route","outbound":tag}));
        }
        if model.dns.enabled {
            rules.push(json!({"protocol": "dns", "action": "hijack-dns"}));
        }
        rules.push(json!({"action": "sniff"}));
        if model.routing.bypass_lan {
            rules.push(json!({"ip_is_private": true, "outbound": "direct"}));
        }
        if model.routing.bypass_china_ips {
            rules.push(json!({"rule_set": ["geoip-cn"], "outbound": "direct"}));
        }
        for rule in model.routing.effective_rules() {
            if let Some(value) = singbox_rule(model, rule, &mut warnings) {
                rules.push(value);
            }
        }

        let mut final_tag = default_group.clone();
        if final_tag == "block" {
            rules.push(json!({"action": "reject"}));
            final_tag = "direct".into();
        }
        let mut route = json!({
            "rules": rules,
            "final": final_tag,
            "auto_detect_interface": true,
            // Populate connection process metadata even without process rules.
            "find_process": cfg!(any(target_os = "linux", target_os = "windows", target_os = "macos"))
        });
        if model.dns.enabled {
            // sing-box 1.12+ requires an explicit resolver for outbound server
            // domains; otherwise startup stops with a deprecation error.
            let local_tag = model
                .dns
                .servers
                .iter()
                .find(|s| {
                    matches!(s.address.trim(), "system" | "local")
                        || (s.tag == "local" && s.server_type == crate::model::DnsServerType::Udp)
                })
                .map(|s| s.tag.clone())
                .unwrap_or_else(|| "local".to_string());
            route["default_domain_resolver"] = json!(local_tag);
        }
        if !rule_sets_json.is_empty() {
            route["rule_set"] = json!(rule_sets_json);
        }

        // --- DNS ---------------------------------------------------------
        let mut dns_section = Value::Null;
        if model.dns.enabled {
            let (dns, dns_warnings) = build_dns(model, ctx, &default_group)?;
            dns_section = dns;
            warnings.extend(dns_warnings);
            let mut vpn_rules = Vec::new();
            for node in model.nodes.iter().filter(|n| {
                n.enabled && n.protocol.is_vpn() && valid_tags.contains(&n.outbound_tag())
            }) {
                let use_dns = match node.protocol {
                    Protocol::Openvpn => node
                        .credentials
                        .openvpn
                        .as_ref()
                        .is_some_and(|o| o.use_pushed_dns),
                    Protocol::Openconnect => node
                        .credentials
                        .openconnect
                        .as_ref()
                        .is_some_and(|o| o.use_pushed_dns),
                    _ => false,
                };
                if !use_dns {
                    continue;
                }
                let tag = format!("vpn-dns-{}", node.id);
                dns_section["servers"].as_array_mut().unwrap().push(json!({
                    "type":node.protocol.as_str(),"tag":tag,"endpoint":node.outbound_tag(),
                    "accept_default_resolvers":false,"accept_search_domain":true
                }));
                vpn_rules.push(json!({"preferred_by":tag,"action":"route","server":tag}));
            }
            vpn_rules.extend(dns_section["rules"].as_array_mut().unwrap().drain(..));
            dns_section["rules"] = json!(vpn_rules);
        }

        // --- assemble ----------------------------------------------------
        let mut root = Map::new();
        root.insert(
            "log".into(),
            json!({"level": ctx.log_level.singbox(), "timestamp": true}),
        );
        if model.dns.enabled {
            root.insert("dns".into(), dns_section);
        }
        if !ctx.mesh.dns.is_empty() {
            let dns = root.entry("dns").or_insert_with(
                || json!({"servers":[{"type":"local","tag":"local"}],"final":"local"}),
            );
            let mut mesh_rules = Vec::new();
            for (index, resolver) in ctx.mesh.dns.iter().enumerate() {
                let tag = format!("xross-mesh-dns-{index}");
                dns["servers"].as_array_mut().unwrap().push(
                    json!({"type":"udp","tag":tag,"server":resolver.address,"server_port":53}),
                );
                mesh_rules
                    .push(json!({"domain_suffix":[resolver.suffix],"action":"route","server":tag}));
            }
            if let Some(existing) = dns["rules"].as_array() {
                mesh_rules.extend(existing.iter().cloned());
            }
            dns["rules"] = json!(mesh_rules);
        }
        root.insert("inbounds".into(), json!(inbounds));
        root.insert("outbounds".into(), json!(outbounds));
        if !endpoints.is_empty() {
            root.insert("endpoints".into(), json!(endpoints));
        }
        root.insert("route".into(), route);
        root.insert(
            "experimental".into(),
            json!({
                "clash_api": {
                    "external_controller": format!("127.0.0.1:{}", ctx.api_port),
                    "default_mode": "rule"
                },
                "cache_file": {
                    "enabled": true,
                    "path": ctx.data_dir.join("cache.db").to_string_lossy()
                }
            }),
        );

        Ok(CompiledConfig {
            core: CoreKind::SingBox,
            json: Value::Object(root),
            warnings,
            inbounds: compiled_inbounds,
            rule_sets: model.routing.rule_sets.clone(),
            default_outbound: final_tag,
        })
    }
}

fn tun_inbound(inbound: &Inbound) -> Value {
    let mut value = json!({
        "type": "tun",
        "tag": inbound.tag,
        "address": [inbound.tun.inet4_address, inbound.tun.inet6_address],
        "mtu": inbound.tun.mtu,
        "stack": inbound.tun.stack.as_str(),
        "auto_route": inbound.tun.auto_route,
        "strict_route": inbound.tun.strict_route
    });
    if let Some(name) = &inbound.tun.interface_name {
        if !name.is_empty() {
            value["interface_name"] = json!(name);
        }
    }
    if !inbound.tun.route_exclude_addresses.is_empty() {
        value["route_exclude_address"] = json!(inbound.tun.route_exclude_addresses);
    }
    value
}

/// Minimal configuration used only by private per-node probes.
pub(super) fn compile_probe(node: &Node, port: u16) -> Result<Value> {
    let endpoint = node.protocol == Protocol::Wireguard || node.protocol.is_vpn();
    let value = node_outbound(node, &mut [])?;
    Ok(json!({
        "endpoints": if endpoint { vec![value.clone()] } else { vec![] },
        "log": {"disabled": true},
        "dns": {"servers": [{"type": "local", "tag": "local"}]},
        "inbounds": [{"type": "http", "tag": "probe", "listen": "127.0.0.1", "listen_port": port}],
        "outbounds": if endpoint { vec![] } else { vec![value] },
        "route": {"auto_detect_interface": true, "default_domain_resolver": "local", "final": node.outbound_tag()}
    }))
}

fn node_outbound(node: &Node, warnings: &mut [String]) -> Result<Value> {
    let _ = warnings;
    let mut outbound = match node.protocol {
        Protocol::Vmess => {
            let uuid = node
                .credentials
                .uuid
                .as_ref()
                .ok_or_else(|| Error::config("缺少 uuid"))?;
            json!({
                "type": "vmess",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "security": "auto",
                "alter_id": node.credentials.alter_id,
                "packet_encoding": "xudp"
            })
        }
        Protocol::Vless => {
            let uuid = node
                .credentials
                .uuid
                .as_ref()
                .ok_or_else(|| Error::config("缺少 uuid"))?;
            let mut value = json!({
                "type": "vless",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "packet_encoding": "xudp"
            });
            if let Some(flow) = &node.credentials.flow {
                if !flow.is_empty() {
                    value["flow"] = json!(flow);
                }
            }
            value
        }
        Protocol::Trojan | Protocol::Anytls => {
            let password = node
                .credentials
                .password
                .as_ref()
                .filter(|password| !password.is_empty())
                .ok_or_else(|| Error::config("缺少密码"))?;
            json!({
                "type": node.protocol.as_str(),
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "password": password
            })
        }
        Protocol::Shadowsocks => {
            let method = node
                .credentials
                .method
                .as_ref()
                .ok_or_else(|| Error::config("缺少加密方式"))?;
            let password = node
                .credentials
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            let mut value = json!({
                "type": "shadowsocks",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "method": method,
                "password": password
            });
            if let Some(plugin) = &node.credentials.plugin {
                if !plugin.is_empty() {
                    value["plugin"] = json!(plugin);
                    if let Some(opts) = &node.credentials.plugin_opts {
                        value["plugin_opts"] = json!(opts);
                    }
                }
            }
            value
        }
        Protocol::Hysteria2 => {
            let password = node
                .credentials
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            let mut value = json!({
                "type": "hysteria2",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "password": password
            });
            if let Some(obfs) = &node.credentials.obfs {
                if !obfs.is_empty() {
                    value["obfs"] = json!({
                        "type": obfs,
                        "password": node.credentials.obfs_password.clone().unwrap_or_default()
                    });
                }
            }
            value
        }
        Protocol::Tuic => {
            let uuid = node
                .credentials
                .uuid
                .as_ref()
                .ok_or_else(|| Error::config("缺少 uuid"))?;
            let password = node
                .credentials
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            json!({
                "type": "tuic",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "uuid": uuid,
                "password": password,
                "congestion_control": node.credentials.congestion_control.clone().unwrap_or_else(|| "bbr".into()),
                "udp_relay_mode": node.credentials.udp_relay_mode.clone().unwrap_or_else(|| "native".into())
            })
        }
        Protocol::Socks => {
            let mut value = json!({
                "type": "socks",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port,
                "version": "5"
            });
            if let Some(user) = &node.credentials.username {
                value["username"] = json!(user);
            }
            if let Some(pass) = &node.credentials.password {
                value["password"] = json!(pass);
            }
            value
        }
        Protocol::Http => {
            let mut value = json!({
                "type": "http",
                "tag": node.outbound_tag(),
                "server": node.server,
                "server_port": node.port
            });
            if let Some(user) = &node.credentials.username {
                value["username"] = json!(user);
            }
            if let Some(pass) = &node.credentials.password {
                value["password"] = json!(pass);
            }
            value
        }
        Protocol::Openvpn | Protocol::Openconnect => return vpn::compile(node),
        Protocol::Wireguard => {
            let wg = node
                .credentials
                .wireguard
                .as_ref()
                .ok_or_else(|| Error::config("WireGuard 缺少密钥和地址配置"))?;
            wg.validate()?;
            return Ok(
                json!({"type":"wireguard", "tag":node.outbound_tag(), "system":false,
                "address":wg.addresses,"private_key":wg.private_key,"mtu":wg.mtu,
                "peers":[{"address":node.server,"port":node.port,"public_key":wg.public_key,
                    "pre_shared_key":wg.pre_shared_key,"allowed_ips":wg.allowed_ips,"reserved":wg.reserved}]}),
            );
        }
    };

    if node.protocol == Protocol::Anytls {
        if let Some(options) = &node.credentials.anytls {
            if let Some(value) = &options.idle_session_check_interval {
                outbound["idle_session_check_interval"] = json!(value);
            }
            if let Some(value) = &options.idle_session_timeout {
                outbound["idle_session_timeout"] = json!(value);
            }
            if let Some(value) = options.min_idle_session {
                outbound["min_idle_session"] = json!(value);
            }
        }
    }
    if let Some(tls) = tls_json(node) {
        outbound["tls"] = tls;
    }
    if node.protocol == Protocol::Anytls && !matches!(node.transport, Transport::Tcp) {
        return Err(Error::config(
            "AnyTLS 不支持叠加 WebSocket / gRPC 等传输，请使用 TCP",
        ));
    }
    if let Some(transport) = transport_json(node) {
        outbound["transport"] = transport;
    }
    Ok(outbound)
}

fn tls_json(node: &Node) -> Option<Value> {
    let tls = &node.tls;
    let required = matches!(
        node.protocol,
        Protocol::Hysteria2 | Protocol::Tuic | Protocol::Anytls
    );
    if !tls.enabled && !required && tls.reality.is_none() {
        return None;
    }
    let mut value = json!({"enabled": true, "insecure": tls.insecure});
    if let Some(sni) = &tls.server_name {
        value["server_name"] = json!(sni);
    }
    if !tls.alpn.is_empty() {
        value["alpn"] = json!(tls.alpn);
    }
    if let Some(fp) = &tls.fingerprint {
        value["utls"] = json!({"enabled": true, "fingerprint": fp});
    }
    if let Some(reality) = &tls.reality {
        if value.get("utls").is_none() {
            value["utls"] = json!({"enabled": true, "fingerprint": "chrome"});
        }
        value["reality"] = json!({
            "enabled": true,
            "public_key": reality.public_key,
            "short_id": reality.short_id
        });
    }
    Some(value)
}

fn transport_json(node: &Node) -> Option<Value> {
    match &node.transport {
        Transport::Tcp => None,
        Transport::WebSocket {
            path,
            host,
            headers,
        } => {
            let mut value = json!({"type": "ws", "path": path});
            let mut map = Map::new();
            if let Some(host) = host {
                if !host.is_empty() {
                    map.insert("Host".into(), json!(host));
                }
            }
            for (k, v) in headers {
                map.insert(k.clone(), json!(v));
            }
            if !map.is_empty() {
                value["headers"] = Value::Object(map);
            }
            Some(value)
        }
        Transport::Grpc { service_name } => {
            Some(json!({"type": "grpc", "service_name": service_name}))
        }
        Transport::Http2 { path, host } => {
            let mut value = json!({"type": "http", "path": path});
            if let Some(host) = host {
                value["host"] = json!([host]);
            }
            Some(value)
        }
        Transport::HttpUpgrade { path, host } => {
            let mut value = json!({"type": "httpupgrade", "path": path});
            if let Some(host) = host {
                value["host"] = json!(host);
            }
            Some(value)
        }
        Transport::Quic => Some(json!({"type": "quic"})),
        Transport::Kcp { .. } => None,
    }
}

fn build_rule_sets(
    model: &AppModel,
    tags: &[String],
    ctx: &CompileContext,
    default_group: &str,
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let mut result = Vec::new();
    for tag in tags {
        if let Some(set) = resolve_rule_set(model, tag) {
            result.push(rule_set_json(&set, ctx, default_group));
        } else {
            warnings.push(format!(
                "未知 rule-set“{tag}”，已忽略（请先在规则集中添加）"
            ));
        }
    }
    result
}

fn rule_set_json(set: &RuleSet, ctx: &CompileContext, default_group: &str) -> Value {
    let local_path = ctx.rule_set_path(&set.tag);
    if local_path.exists() {
        return json!({
            "type": "local",
            "tag": set.tag,
            "format": set.format.as_str(),
            "path": local_path.to_string_lossy()
        });
    }
    json!({
        "type": "remote",
        "tag": set.tag,
        "format": set.format.as_str(),
        "url": set.url,
        "download_detour": if set.download_detour { default_group } else { "direct" },
        "update_interval": format!("{}h", set.update_interval_hours.max(1))
    })
}

fn singbox_rule(
    model: &AppModel,
    rule: &crate::model::RouteRule,
    _warnings: &mut [String],
) -> Option<Value> {
    let m = &rule.matchers;
    let mut object = Map::new();
    if !m.domains.is_empty() {
        object.insert("domain".into(), json!(m.domains));
    }
    if !m.domain_suffixes.is_empty() {
        object.insert("domain_suffix".into(), json!(m.domain_suffixes));
    }
    if !m.domain_keywords.is_empty() {
        object.insert("domain_keyword".into(), json!(m.domain_keywords));
    }
    if !m.domain_regexes.is_empty() {
        object.insert("domain_regex".into(), json!(m.domain_regexes));
    }
    if !m.ip_cidrs.is_empty() {
        object.insert("ip_cidr".into(), json!(m.ip_cidrs));
    }
    if !m.source_ip_cidrs.is_empty() {
        object.insert("source_ip_cidr".into(), json!(m.source_ip_cidrs));
    }
    let (ports, ranges) = super::parse_port_specs(&m.ports);
    if !ports.is_empty() {
        object.insert("port".into(), json!(ports));
    }
    if !ranges.is_empty() {
        object.insert("port_range".into(), json!(ranges));
    }
    let (source_ports, source_ranges) = super::parse_port_specs(&m.source_ports);
    if !source_ports.is_empty() {
        object.insert("source_port".into(), json!(source_ports));
    }
    if !source_ranges.is_empty() {
        object.insert("source_port_range".into(), json!(source_ranges));
    }
    if let Some(network) = network_str(m) {
        object.insert("network".into(), json!(network));
    }
    if !m.process_names.is_empty() {
        object.insert("process_name".into(), json!(m.process_names));
    }
    if !m.process_paths.is_empty() {
        object.insert("process_path".into(), json!(m.process_paths));
    }
    if !m.inbound_tags.is_empty() {
        object.insert("inbound".into(), json!(m.inbound_tags));
    }
    if !m.protocols.is_empty() {
        object.insert("protocol".into(), json!(m.protocols));
    }
    let mut rule_sets: Vec<String> = m.rule_sets.clone();
    for geosite in &m.geosites {
        rule_sets.push(format!("geosite-{geosite}"));
    }
    for geoip in &m.geoips {
        rule_sets.push(format!("geoip-{geoip}"));
    }
    if !rule_sets.is_empty() {
        object.insert("rule_set".into(), json!(rule_sets));
    }
    if object.is_empty() {
        return None;
    }
    let target = action_tag(CoreKind::SingBox, model, &rule.action)?;
    if target == "block" {
        object.insert("action".into(), json!("reject"));
    } else {
        object.insert("outbound".into(), json!(target));
    }
    Some(Value::Object(object))
}

fn resolve_detour(model: &AppModel, raw: &str, default_group: &str) -> Option<String> {
    match raw {
        "proxy" | "default" => Some(default_group.to_string()),
        "direct" => Some("direct".into()),
        other => {
            if let Some(group) = model.group(other) {
                Some(group.tag())
            } else if let Some(node) = model.node(other) {
                Some(node.outbound_tag())
            } else if other.is_empty() {
                None
            } else {
                Some(other.to_string())
            }
        }
    }
}

fn split_server_address(address: &str, default_port: u16) -> (String, u16) {
    let address = address.trim();
    if address.starts_with('[') {
        if let Some((host, tail)) = address[1..].split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host.to_string(), port);
        }
    }
    if address.matches(':').count() == 1 {
        if let Some((host, port)) = address.rsplit_once(':') {
            if let Ok(port) = port.parse() {
                return (host.to_string(), port);
            }
        }
    }
    (address.to_string(), default_port)
}

fn dns_server_object(server: &crate::model::DnsServer, local_tag: &str) -> Option<Value> {
    use crate::model::DnsServerType;
    let mut object = Map::new();
    object.insert("tag".into(), json!(server.tag));
    match server.server_type {
        DnsServerType::FakeIp => return None,
        DnsServerType::Udp | DnsServerType::Tcp => {
            let address = server.address.trim();
            if address.is_empty() || matches!(address, "system" | "local") {
                object.insert("type".into(), json!("local"));
            } else {
                object.insert(
                    "type".into(),
                    json!(if server.server_type == DnsServerType::Tcp {
                        "tcp"
                    } else {
                        "udp"
                    }),
                );
                let (host, port) = split_server_address(address, 53);
                object.insert("server".into(), json!(host));
                if port != 53 {
                    object.insert("server_port".into(), json!(port));
                }
            }
        }
        DnsServerType::Tls => {
            let (host, port) = split_server_address(&server.address, 853);
            object.insert("type".into(), json!("tls"));
            object.insert("server".into(), json!(host));
            if port != 853 {
                object.insert("server_port".into(), json!(port));
            }
        }
        DnsServerType::Https | DnsServerType::Quic | DnsServerType::H3 => {
            let raw = server.display_address();
            let parsed = url::Url::parse(&raw).ok();
            let (host, port, path) = if let Some(url) = &parsed {
                (
                    url.host_str().unwrap_or_default().to_string(),
                    url.port_or_known_default()
                        .unwrap_or(match server.server_type {
                            DnsServerType::Https => 443,
                            _ => 853,
                        }),
                    url.path().to_string(),
                )
            } else {
                let (host, port) = split_server_address(&server.address, 443);
                (host, port, "/dns-query".to_string())
            };
            let kind = match server.server_type {
                DnsServerType::Https => "https",
                DnsServerType::Quic => "quic",
                _ => "h3",
            };
            let default_port = if server.server_type == DnsServerType::Https {
                443
            } else {
                853
            };
            object.insert("type".into(), json!(kind));
            object.insert("server".into(), json!(host));
            if port != default_port {
                object.insert("server_port".into(), json!(port));
            }
            if server.server_type == DnsServerType::Https && !path.is_empty() {
                object.insert("path".into(), json!(path));
            }
        }
    }
    if !object.contains_key("server") {
        // local server has no address; nothing else to add
    } else if let Some(host) = object.get("server").and_then(|v| v.as_str()) {
        if host.parse::<std::net::IpAddr>().is_err() {
            object.insert("domain_resolver".into(), json!(local_tag));
        }
    }
    Some(Value::Object(object))
}

fn build_dns(
    model: &AppModel,
    _ctx: &CompileContext,
    default_group: &str,
) -> Result<(Value, Vec<String>)> {
    let mut warnings = Vec::new();
    let mut servers: Vec<Value> = Vec::new();
    let mut rules: Vec<Value> = Vec::new();
    let fake_enabled = model.dns.fake_ip;

    let local_tag = model
        .dns
        .servers
        .iter()
        .find(|s| {
            matches!(s.address.trim(), "system" | "local")
                || s.server_type == crate::model::DnsServerType::Udp && s.tag == "local"
        })
        .map(|s| s.tag.clone())
        .unwrap_or_else(|| "local".to_string());
    let local_present = model
        .dns
        .servers
        .iter()
        .any(|s| s.tag == local_tag && !s.is_fakeip());

    if fake_enabled {
        let range = if model.dns.fake_ip_range.trim().is_empty() {
            "198.18.0.0/15"
        } else {
            model.dns.fake_ip_range.trim()
        };
        servers.push(json!({
            "type": "fakeip",
            "tag": "fakeip",
            "inet4_range": range,
            "inet6_range": "fc00::/18"
        }));
    }

    let strategy = match model.routing.domain_strategy {
        DomainStrategy::PreferIpv4 | DomainStrategy::IpIfNonMatch | DomainStrategy::IpOnDemand => {
            "prefer_ipv4"
        }
        DomainStrategy::PreferIpv6 => "prefer_ipv6",
        DomainStrategy::AsIs => "prefer_ipv4",
    };

    for server in &model.dns.servers {
        if server.is_fakeip() {
            continue;
        }
        let Some(mut entry) = dns_server_object(server, &local_tag) else {
            continue;
        };
        if let Some(target) = &server.detour {
            if let Some(detour) = resolve_detour(model, target, default_group) {
                if detour != "direct" {
                    entry["detour"] = json!(detour);
                }
            }
        }
        if let Some(subnet) = &server.client_subnet {
            entry["client_subnet"] = json!(subnet);
        }
        servers.push(entry);
    }

    if !local_present {
        servers.push(json!({"type": "local", "tag": local_tag}));
    }

    // Node server domains must resolve without the proxy, otherwise a
    // proxy-detoured remote DNS creates a bootstrap loop.
    for node in model.nodes.iter().filter(|n| n.enabled) {
        if node.server.parse::<std::net::IpAddr>().is_ok() {
            continue;
        }
        rules.push(json!({
            "domain": [node.server.clone()],
            "server": local_tag
        }));
    }

    // Hosts / predefined answers.
    if !model.dns.hosts.is_empty() {
        warnings.push("hosts 使用 sing-box 1.12+ 的 dns predefined 动作".into());
        for (domain, addresses) in &model.dns.hosts {
            if addresses.is_empty() {
                continue;
            }
            rules.push(json!({
                "domain": [domain],
                "action": "predefined",
                "answer": addresses
            }));
        }
    }

    // fake-ip filters must win over the fakeip server rules.
    if fake_enabled {
        for filter in &model.dns.fake_ip_filter {
            let suffix = filter
                .trim()
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_string();
            if suffix.is_empty() {
                continue;
            }
            rules.push(json!({"domain_suffix": [suffix], "server": local_tag}));
        }
    }

    for server in &model.dns.servers {
        if server.is_fakeip() || server.domains.is_empty() {
            continue;
        }
        let sets = split_dns_domains(&server.domains);
        let goes_through_proxy = server
            .detour
            .as_deref()
            .and_then(|d| resolve_detour(model, d, default_group))
            .map(|d| d == default_group)
            .unwrap_or(false);
        let target = if fake_enabled && goes_through_proxy {
            "fakeip"
        } else {
            server.tag.as_str()
        };
        rules.push(dns_server_rule(&sets, target));
    }

    let mut final_tag = model
        .dns
        .final_server
        .as_ref()
        .filter(|tag| !tag.is_empty() && tag.as_str() != "fakeip")
        .filter(|tag| {
            model
                .dns
                .servers
                .iter()
                .any(|s| &s.tag == *tag && !s.is_fakeip())
                || **tag == local_tag
        })
        .cloned()
        .unwrap_or_else(|| local_tag.clone());
    if final_tag == "fakeip" {
        final_tag = local_tag.clone();
    }

    let dns = json!({
        "servers": servers,
        "rules": rules,
        "final": final_tag,
        "strategy": strategy
    });
    Ok((dns, warnings))
}

#[cfg(test)]
mod tests {
    #[test]
    fn vpn_endpoints_groups_dns_and_version_degradation() {
        let (mut model, settings, mut ctx) = sample();
        let existing = model.groups[0].members[0].clone();
        let mut vpn = crate::model::NodeDraft {
            protocol: Protocol::Openconnect,
            server: "vpn.example".into(),
            username: "fixture".into(),
            password: "fake-password".into(),
            ..Default::default()
        }
        .into_node(CoreKind::SingBox)
        .unwrap();
        vpn.name = "office".into();
        model.groups[0].members.push(vpn.id.clone());
        model.groups[0].selected = Some(vpn.id.clone());
        model.nodes.push(vpn.clone());
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert_eq!(compiled.json["endpoints"][0]["type"], "openconnect");
        assert!(
            compiled.json["dns"]["servers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["type"] == "openconnect" && s["endpoint"] == vpn.outbound_tag())
        );
        assert_eq!(
            compiled.json["dns"]["rules"][0]["preferred_by"],
            format!("vpn-dns-{}", vpn.id)
        );
        let probe = compile_probe(&vpn, 18080).unwrap();
        assert_eq!(probe["endpoints"][0]["type"], "openconnect");
        assert!(probe["outbounds"].as_array().unwrap().is_empty());
        ctx.core_version = Some(semver::Version::new(1, 13, 0));
        let degraded = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(
            degraded
                .warnings
                .iter()
                .any(|s| s.contains("已跳过") && s.contains("1.14"))
        );
        assert!(
            !degraded
                .to_pretty_string()
                .unwrap()
                .contains(&vpn.outbound_tag())
        );
        assert!(
            degraded
                .to_pretty_string()
                .unwrap()
                .contains(&model.node(&existing).unwrap().outbound_tag())
        );
        let xray = crate::adapters::XrayAdapter
            .compile(&model, &settings, &ctx)
            .unwrap();
        assert!(xray.warnings.iter().any(|s| s.contains("OpenConnect")));
        assert!(
            !xray
                .to_pretty_string()
                .unwrap()
                .contains(&vpn.outbound_tag())
        );
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
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let probe = super::super::compile_probe(CoreKind::SingBox, &wg, 12345).unwrap();
        let ep = &compiled.json["endpoints"][0];
        assert_eq!(
            ep["private_key"],
            wg.credentials.wireguard.as_ref().unwrap().private_key
        );
        assert_eq!(ep["peers"][0]["reserved"], json!([1, 2, 3]));
        assert_eq!(ep["peers"][0]["address"], "2001:db8::1");
        assert_eq!(probe["endpoints"][0], *ep);
        assert!(probe["outbounds"].as_array().unwrap().is_empty());
        let imported = crate::subscription::parse::parse_singbox_json(&compiled.json);
        assert_eq!(
            imported.nodes[0].credentials.wireguard,
            wg.credentials.wireguard
        );
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
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let rules = compiled.json["route"]["rules"].as_array().unwrap();
        let domains: Vec<_> = rules
            .iter()
            .filter_map(|r| r["domain"][0].as_str())
            .collect();
        assert_eq!(domains, ["first.example", "last.example"]);
    }

    #[test]
    fn anytls_emits_required_tls_and_private_probe_route() {
        let mut node = Node::new("test", Protocol::Anytls, "example.com", 443);
        node.credentials.password = Some("fake".into());
        node.credentials.anytls = Some(crate::model::AnytlsOptions {
            idle_session_timeout: Some("40s".into()),
            min_idle_session: Some(3),
            ..Default::default()
        });
        node.tls.server_name = Some("tls.example.com".into());
        let (mut model, settings, ctx) = sample();
        model.add_nodes(vec![node.clone()]);
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let outbound = compiled.json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["type"] == "anytls")
            .unwrap();
        assert_eq!(outbound["password"], "fake");
        assert_eq!(outbound["idle_session_timeout"], "40s");
        assert_eq!(outbound["min_idle_session"], 3);
        assert_eq!(outbound["tls"]["enabled"], true);
        assert_eq!(outbound["tls"]["server_name"], "tls.example.com");
        assert!(outbound.get("transport").is_none());
        let probe = super::super::compile_probe(CoreKind::SingBox, &node, 12345).unwrap();
        assert_eq!(probe["outbounds"].as_array().unwrap().len(), 1);
        assert_eq!(probe["route"]["final"], node.outbound_tag());
        assert_eq!(probe["route"]["auto_detect_interface"], true);
        assert_eq!(probe["inbounds"][0]["listen"], "127.0.0.1");
        assert!(probe.get("experimental").is_none());
        assert!(probe["route"].get("rules").is_none());
        node.credentials.password = None;
        assert!(super::super::compile_probe(CoreKind::SingBox, &node, 12345).is_err());
    }

    use super::*;
    use crate::model::{AppModel, Node, Protocol};
    use crate::settings::Settings;

    fn sample() -> (AppModel, Settings, CompileContext) {
        let mut model = AppModel::default();
        let mut node = Node::new("n", Protocol::Vless, "example.com", 443);
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
    fn compiles_modern_dns_and_route() {
        let (model, settings, ctx) = sample();
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let json = &compiled.json;
        let servers = json["dns"]["servers"].as_array().unwrap();
        assert!(servers.iter().any(|s| s["type"] == "fakeip"));
        assert!(servers.iter().all(|s| s.get("address").is_none()));
        assert_eq!(json["route"]["default_domain_resolver"], "local");
        assert!(json["route"].get("rule_set").is_some());
        assert_eq!(
            json["route"]["find_process"],
            cfg!(any(
                target_os = "linux",
                target_os = "windows",
                target_os = "macos"
            ))
        );
        let outbounds = json["outbounds"].as_array().unwrap();
        assert!(outbounds.iter().all(|o| o["tag"] != "block"));
        assert!(!json.to_string().contains("geoip\""));
        assert!(!json.to_string().contains("geosite\""));
    }

    #[test]
    fn mesh_precedes_fake_ip_and_preserves_original_tun_intent() {
        use crate::mesh::coexist::{CoexistencePlan, MeshDns, MeshRoute};
        let (mut model, settings, mut ctx) = sample();
        let tun = model
            .inbounds
            .iter_mut()
            .find(|i| i.kind == InboundKind::Tun)
            .unwrap();
        tun.enabled = true;
        tun.tun.strict_route = true;
        tun.tun.route_exclude_addresses.push("192.0.2.0/24".into());
        let original = serde_json::to_value(&model).unwrap();
        ctx.mesh = CoexistencePlan {
            active: true,
            routes: vec![MeshRoute {
                cidr: "100.64.0.0/10".into(),
                interface: "xross-mesh-ts-test".into(),
            }],
            dns: vec![MeshDns {
                suffix: "example.ts.net".into(),
                address: "127.0.0.153".into(),
            }],
        };
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let tun = compiled.json["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "tun")
            .unwrap();
        assert!(
            tun["route_exclude_address"]
                .as_array()
                .unwrap()
                .contains(&json!("100.64.0.0/10"))
        );
        assert!(
            tun["route_exclude_address"]
                .as_array()
                .unwrap()
                .contains(&json!("192.0.2.0/24"))
        );
        assert_eq!(tun["strict_route"], json!(!cfg!(windows)));
        assert_eq!(
            compiled.json["dns"]["rules"][0]["server"],
            "xross-mesh-dns-0"
        );
        assert_eq!(compiled.json["route"]["rules"][0]["action"], "resolve");
        assert_eq!(serde_json::to_value(&model).unwrap(), original);
        ctx.mesh = Default::default();
        let restored = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let tun = restored.json["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "tun")
            .unwrap();
        assert_eq!(tun["strict_route"], true);
    }

    #[test]
    fn tailscale_scopes_routes_before_starting_and_has_no_exit() {
        let p = crate::mesh::MeshProfile {
            provider: crate::mesh::MeshProvider::Tailscale,
            remote_subnets: vec!["192.168.50.0/24".into()],
            ..Default::default()
        };
        let json = compile_mesh(&p, Path::new("mesh-test"), 19090, "fake-key").unwrap();
        assert_eq!(json["endpoints"][0]["system_interface"], false);
        assert_eq!(json["endpoints"][0]["advertise_exit_node"], false);
        assert_eq!(json["endpoints"][0]["advertise_routes"], json!([]));
        let tun = &json["inbounds"][1];
        assert_eq!(tun["strict_route"], false);
        assert_eq!(
            tun["route_address"],
            json!(["100.64.0.0/10", "fd7a:115c:a1e0::/48", "192.168.50.0/24"])
        );
        assert_eq!(json["services"][0]["listen"], "127.0.0.1");
        assert_eq!(json["services"][0]["secret"], "fake-key");
    }

    #[test]
    fn rejects_missing_nodes() {
        let mut model = AppModel::default();
        model.nodes.clear();
        let settings = Settings::default();
        let ctx = CompileContext::new(
            &settings,
            std::path::PathBuf::from("/tmp/xross-test-data"),
            std::path::PathBuf::from("/tmp/xross-test-run"),
        );
        assert!(SingBoxAdapter.compile(&model, &settings, &ctx).is_err());
    }

    #[test]
    fn selector_uses_singbox_native_type() {
        let (model, settings, ctx) = sample();
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let outbounds = compiled.json["outbounds"].as_array().unwrap();
        assert!(outbounds.iter().any(|o| o["type"] == "selector"));
        assert!(outbounds.iter().any(|o| o["type"] == "urltest"));
    }

    #[test]
    fn gfwlist_uses_a_published_rule_set_and_shares_the_download_source() {
        let (mut model, settings, ctx) = sample();
        model.apply_preset(crate::model::RoutePreset::GfwList);
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let set = compiled.json["route"]["rule_set"]
            .as_array()
            .unwrap()
            .iter()
            .find(|set| set["tag"] == "geosite-gfw")
            .unwrap();
        assert_eq!(
            set["url"],
            "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geosite/gfw.srs"
        );
        let downloads = super::super::required_rule_sets(&model);
        assert_eq!(
            set["url"],
            downloads
                .iter()
                .find(|set| set.tag == "geosite-gfw")
                .unwrap()
                .url
        );
        assert!(
            compiled.json["route"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| rule["rule_set"] == json!(["geosite-gfw"]))
        );
        let mut legacy = super::super::default_rule_set("geosite-gfw").unwrap();
        legacy.url =
            "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-gfw.srs"
                .into();
        model.routing.rule_sets.push(legacy);
        assert_eq!(
            resolve_rule_set(&model, "geosite-gfw").unwrap().url,
            set["url"].as_str().unwrap()
        );
        model.routing.rule_sets.last_mut().unwrap().url =
            "https://example.com/custom-gfw.srs".into();
        assert_eq!(
            resolve_rule_set(&model, "geosite-gfw").unwrap().url,
            "https://example.com/custom-gfw.srs"
        );
    }
}
