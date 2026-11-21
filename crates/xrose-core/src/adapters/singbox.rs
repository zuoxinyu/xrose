use std::path::Path;

use serde_json::{Map, Value, json};

use super::{
    CompileContext, CompiledConfig, CompiledInbound, CoreAdapter, action_tag, dns_server_rule,
    network_str, proxy_group_tag, resolve_rule_set, split_dns_domains,
};
use crate::error::{Error, Result};
use crate::model::{
    AppModel, ChainHop, CoreKind, DomainStrategy, GroupKind, Inbound, InboundKind, Node, Protocol,
    RuleSet, Transport,
};
use crate::settings::Settings;

pub(crate) mod vpn;

pub struct SingBoxAdapter;

/// Use the adapter's resolved defaults, including any fallback for unsupported nodes.
pub(crate) fn mesh_selector_defaults(
    config: &CompiledConfig,
) -> impl Iterator<Item = (&str, &str)> {
    config.json["outbounds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|outbound| {
            let members = outbound["outbounds"].as_array()?;
            if outbound["type"] != "selector"
                || !members
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|tag| tag.starts_with("node-mesh-exit-"))
            {
                return None;
            }
            Some((outbound["tag"].as_str()?, outbound["default"].as_str()?))
        })
}

/// Standalone mesh configuration; does not require a proxy profile or nodes.
pub fn compile_mesh(
    profile: &crate::mesh::MeshProfile,
    directory: &Path,
    api_port: u16,
    secret: &str,
    forward_bridge: (u16, &str),
) -> crate::Result<Value> {
    profile.validate()?;
    let mut routes = vec!["100.64.0.0/10".to_string(), "fd7a:115c:a1e0::/48".into()];
    routes.extend(profile.remote_subnets.clone());
    // This TUN only captures mesh prefixes. On Windows, let the OS route
    // control-plane connections through an existing VPN when necessary.
    // Exit sessions must bind their uplink before the host enables a full TUN.
    // Otherwise control-plane traffic can be captured by its own exit route.
    let mut config = json!({
        "log":{"level":"error"},
        "endpoints":[{"type":"tailscale","tag":"mesh-ts","state_directory":directory.join("identity"),"hostname":profile.hostname,
            "control_url":profile.control_url,"system_interface":false,
            "accept_routes":!profile.remote_subnets.is_empty(),"advertise_routes":profile.advertised_subnets,"advertise_exit_node":profile.advertise_exit_node,"exit_node":profile.exit_node,"exit_node_allow_lan_access":false}],
        "services":[{"type":"api","listen":"127.0.0.1","listen_port":api_port,"secret":secret,"dashboard":false}],
        "inbounds":[{"type":"direct","tag":"mesh-dns","listen":crate::mesh::DNS_ADDRESS,"listen_port":53},
            {"type":"tun","tag":"mesh-tun","interface_name":profile.interface_name(),"address":[crate::mesh::TAILSCALE_TUN_ADDRESS,crate::mesh::TAILSCALE_TUN_IPV6_ADDRESS],"mtu":1280,"stack":"gvisor","auto_route":true,"strict_route":false,"route_address":routes}],
        "outbounds":[{"type":"direct","tag":"direct"}],
        "route":{"auto_detect_interface":!cfg!(windows) || !profile.exit_node.is_empty(),"default_domain_resolver":"local","rules":[{"inbound":"mesh-dns","action":"hijack-dns"},{"inbound":"mesh-tun","action":"route","outbound":"mesh-ts"},{"inbound":"mesh-ts","action":"reject"}],"final":"direct"},
        "dns":{"servers":[{"type":"local","tag":"local"},{"type":"tailscale","tag":"mesh-dns","endpoint":"mesh-ts","accept_default_resolvers":!profile.exit_node.is_empty()}],
            "rules":[{"inbound":"mesh-dns","action":"route","server":"mesh-dns"}],"final":"local"}
    });
    let mut incoming = profile.advertised_subnets.clone();
    if profile.advertise_exit_node {
        incoming.extend(["0.0.0.0/0".into(), "::/0".into()]);
    }
    if !incoming.is_empty() {
        config["route"]["rules"].as_array_mut().unwrap().insert(
            2,
            json!({"inbound":"mesh-ts","ip_cidr":incoming,"action":"route","outbound":"direct"}),
        );
    }
    if profile.outbound_port != 0 {
        config["inbounds"].as_array_mut().unwrap().push(json!({"type":"socks","tag":"mesh-exit","listen":"127.0.0.1","listen_port":profile.outbound_port,"users":[{"username":"xrose","password":profile.outbound_secret.0}]}));
        config["route"]["rules"].as_array_mut().unwrap().insert(
            0,
            json!({"inbound":"mesh-exit","action":"route","outbound":"mesh-ts"}),
        );
        if !profile.exit_node.is_empty() {
            config["dns"]["rules"]
                .as_array_mut()
                .unwrap()
                .push(json!({"inbound":"mesh-exit","action":"route","server":"mesh-dns"}));
        }
    }
    config["inbounds"].as_array_mut().unwrap().push(json!({
        "type":"socks", "tag":"mesh-forwards", "listen":"127.0.0.1", "listen_port":forward_bridge.0,
        "users":[{"username":"xrose","password":forward_bridge.1}]
    }));
    config["route"]["rules"].as_array_mut().unwrap().insert(
        0,
        json!({
            "inbound":"mesh-forwards", "action":"route", "outbound":"mesh-ts"
        }),
    );
    Ok(config)
}

/// Forward exit traffic and Tailscale peer DNS to the independent local policy router.
pub(crate) fn route_mesh_exit(config: &mut Value, bridge: &crate::mesh::ExitBridge) {
    config["outbounds"].as_array_mut().unwrap().push(json!({
        "type":"socks", "tag":"mesh-exit-policy", "server":"127.0.0.1",
        "server_port":bridge.port, "version":"5", "username":"xrose", "password":bridge.password.0
    }));
    let rules = config["route"]["rules"].as_array_mut().unwrap();
    for rule in rules
        .iter_mut()
        .filter(|rule| rule["inbound"] == "mesh-ts" && rule["outbound"] == "direct")
    {
        if let Some(prefixes) = rule["ip_cidr"].as_array_mut() {
            prefixes.retain(|prefix| prefix != "0.0.0.0/0" && prefix != "::/0");
        }
    }
    rules.retain(|rule| {
        !(rule["inbound"] == "mesh-ts"
            && rule["outbound"] == "direct"
            && rule["ip_cidr"].as_array().is_some_and(Vec::is_empty))
    });
    let position = rules
        .iter()
        .position(|rule| rule["inbound"] == "mesh-ts" && rule["action"] == "reject")
        .unwrap_or(rules.len());
    rules.insert(position, json!({"inbound":"mesh-ts","network":["tcp","udp"],"action":"route","outbound":"mesh-exit-policy"}));
    config["dns"]["servers"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "type":"udp", "tag":"mesh-exit-policy-dns", "server":"1.1.1.1", "server_port":53,
            "detour":"mesh-exit-policy"
        }));
    config["dns"]["rules"].as_array_mut().unwrap().insert(
        0,
        json!({"inbound":"mesh-ts","action":"route","server":"mesh-exit-policy-dns"}),
    );
}

pub(super) fn authenticate_exit_gateway(
    config: &mut CompiledConfig,
    model: &AppModel,
    password: &str,
    ctx: &CompileContext,
) -> Result<()> {
    for group in super::mesh::policy_groups(model, CoreKind::SingBox)? {
        if !config.json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|outbound| outbound["tag"] == group.tag())
        {
            return Err(Error::config("出口出站组无法编译，请检查节点配置后重试"));
        }
    }
    config.json["dns"]["reverse_mapping"] = json!(true);
    config.json["experimental"]["cache_file"]["path"] =
        json!(ctx.runtime_dir.join("exit-cache.db"));
    for inbound in config.json["inbounds"].as_array_mut().unwrap() {
        if inbound["tag"] == super::mesh::EXIT_INBOUND {
            inbound["users"] = json!([{"username":"xrose","password":password}]);
        }
    }
    Ok(())
}

/// A fixed authenticated bridge applies service rules without restarting the endpoint.
pub(crate) fn allow_mesh_services(
    config: &mut Value,
    address: std::net::SocketAddr,
    password: &str,
) {
    config["outbounds"].as_array_mut().unwrap().push(json!({
        "type":"socks", "tag":"mesh-services", "server":address.ip().to_string(),
        "server_port":address.port(), "version":"5", "username":"xrose", "password":password
    }));
    config["route"]["rules"].as_array_mut().unwrap().insert(
        0,
        json!({
            "inbound":"mesh-ts", "ip_cidr":["127.0.0.1/32","::1/128"],
            "source_ip_cidr":["100.64.0.0/10","fd7a:115c:a1e0::/48"],
            "action":"route", "outbound":"mesh-services"
        }),
    );
}

/// Incoming connections to the endpoint's own addresses are mapped to loopback by sing-box.
pub(crate) fn allow_mesh_discovery(config: &mut Value, (port, password): (u16, &str)) {
    if port >= 1024 && !password.is_empty() {
        config["outbounds"].as_array_mut().unwrap().push(json!({
            "type":"socks", "tag":"mesh-discovery", "server":"127.0.0.1",
            "server_port":port, "version":"5", "username":"xrose", "password":password
        }));
        config["route"]["rules"].as_array_mut().unwrap().insert(
            0,
            json!({
                "inbound":"mesh-ts", "network":"tcp", "ip_cidr":["127.0.0.1/32","::1/128"],
                "source_ip_cidr":["100.64.0.0/10","fd7a:115c:a1e0::/48"],
                "port":crate::mesh::sync::PORT, "action":"route","outbound":"mesh-discovery"
            }),
        );
    }
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
        settings: &Settings,
        ctx: &CompileContext,
    ) -> Result<CompiledConfig> {
        if model.work_mode == crate::model::WorkMode::Mesh {
            return super::mesh::compile_client(CoreKind::SingBox, model, settings, ctx);
        }
        let mut warnings = Vec::new();
        let mut effective = super::compilation_model(model, &mut warnings);
        let model = &effective;
        let direct = model.work_mode == crate::model::WorkMode::Direct;
        let mut inbounds = Vec::new();
        let mut compiled_inbounds = Vec::new();

        let listeners = crate::model::inbound::effective_inbounds(model, settings);
        crate::model::inbound::validate_listeners(&listeners, CoreKind::SingBox, settings)?;
        for inbound in &listeners {
            let account = (inbound.allow_lan && settings.lan_auth).then_some(&settings.lan_account);
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
                            warnings.push("TUN strict_route 已启用，Windows 严格 DNS 过滤可能影响组网 DNS；出现解析失败时可在设置中关闭严格路由".into());
                        }
                    }
                    inbounds.push(tun);
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
                    inbounds.push(json!({
                        "type": inbound.kind.as_str(),
                        "tag": inbound.tag,
                        "listen": inbound.listen_address(),
                        "listen_port": inbound.port,
                        "network": if inbound.udp { "tcp,udp" } else { "tcp" }
                    }));
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
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
                    if let Some(a) = account {
                        value["users"] = json!([{"username":a.username, "password":a.password}]);
                    }
                    inbounds.push(value);
                    compiled_inbounds.push(CompiledInbound {
                        listen: inbound.listen_address().into(),
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
            if !node.enabled || node.is_chain() {
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

        // Compile proxy chains in topological passes using cloned per-chain outbounds
        // so standalone hops remain untouched.
        let mut valid_tags: std::collections::HashSet<String> = outbounds
            .iter()
            .chain(endpoints.iter())
            .filter_map(|o| o["tag"].as_str().map(str::to_owned))
            .collect();
        let filtered_entries = compile_singbox_chains(
            model,
            &mut outbounds,
            &mut endpoints,
            &mut valid_tags,
            &mut warnings,
        );

        if !direct
            && outbounds.is_empty()
            && endpoints.is_empty()
            && super::mesh::requires_proxy(model)
        {
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

        // Resolve group selections against successfully compiled nodes and chains only.
        let nodes = &effective.nodes;
        for group in &mut effective.groups {
            group.members.retain(|id| {
                nodes
                    .iter()
                    .find(|n| &n.id == id)
                    .is_some_and(|n| valid_tags.contains(&n.outbound_tag()))
            });
            group.repair();
        }
        effective.groups.retain(|g| !g.members.is_empty());
        if effective
            .default_group
            .as_ref()
            .is_some_and(|id| effective.group(id).is_none())
        {
            effective.default_group = effective.groups.first().map(|g| g.id.clone());
        }
        let model = &effective;

        let default_group =
            proxy_group_tag(model, &model.routing.final_action).unwrap_or_else(|| "direct".into());

        // Build restricted entry groups after all eligible chain members have compiled.
        let mut entry_groups = Vec::new();
        for chain in model
            .nodes
            .iter()
            .filter(|n| filtered_entries.contains(&n.id))
        {
            if let Some(ChainHop::Group(id)) = chain.chain_hops.first()
                && let Some(group) = model.group(id)
            {
                let mut filtered = group.clone();
                filtered
                    .members
                    .retain(|id| model.chain_entry_allows(&chain.chain_hops, id));
                filtered.repair();
                entry_groups.push((chain_entry_tag(chain), filtered));
            }
        }
        let groups = model
            .groups
            .iter()
            .filter(|g| g.enabled)
            .map(|g| (g.tag(), g, false))
            .chain(entry_groups.iter().map(|(tag, g)| (tag.clone(), g, true)));
        for (tag, group, restricted) in groups {
            let members: Vec<String> = group
                .members
                .iter()
                .filter_map(|id| model.node(id))
                .map(|n| n.outbound_tag())
                .filter(|tag| valid_tags.contains(tag))
                .collect();
            if members.is_empty() {
                if tag == default_group {
                    return Err(Error::config(format!(
                        "默认出站组“{}”没有可用成员",
                        group.name
                    )));
                }
                warnings.push(format!("出站组“{}”没有可用成员，已跳过", group.name));
                continue;
            }
            let interval = format!("{}s", group.interval_secs.max(30));
            let value = if group.managed() || group.manually_selectable() {
                let selected = group
                    .active_node()
                    .and_then(|id| model.node(id))
                    .map(Node::outbound_tag)
                    .unwrap_or_else(|| members[0].clone());
                // A pinned group must not restore a different choice from the Clash cache.
                let choices = if group.managed() || restricted {
                    vec![selected.clone()]
                } else {
                    members
                };
                json!({"type": "selector", "tag": tag, "outbounds": choices,
                           "default": selected, "interrupt_exist_connections": true})
            } else if group.kind == GroupKind::UrlTest {
                json!({
                    "type": "urltest",
                    "tag": tag,
                    "outbounds": members,
                    "url": group.url.clone().unwrap_or_else(|| ctx.urltest_url.clone()),
                    "interval": interval,
                    "tolerance": group.tolerance_ms
                })
            } else {
                warnings.push(format!(
                    "sing-box 没有 loadbalance 出站，“{}”已编译为 selector",
                    group.name
                ));
                json!({
                    "type": "selector",
                    "tag": tag,
                    "outbounds": members,
                    "interrupt_exist_connections": true
                })
            };
            outbounds.push(value);
        }

        // --- rule sets ---------------------------------------------------
        let rule_set_tags = super::rule_set_tags(model, model.work_mode);
        let rule_sets_json =
            build_rule_sets(model, &rule_set_tags, ctx, &default_group, &mut warnings);

        // --- route -------------------------------------------------------
        let mut rules: Vec<Value> = Vec::new();
        if ctx.mesh_transport {
            if let Some(process) = ctx
                .mesh_exit
                .as_ref()
                .and_then(|exit| exit.transport_process.as_ref())
            {
                let process = super::mesh::transport_process_path(CoreKind::SingBox, process);
                rules.push(json!({"inbound":["mesh-system-tun"],"process_path":[process],"action":"route","outbound":"direct"}));
            }
        }
        for inbound in listeners.iter().filter(|inbound| {
            !inbound.udp && matches!(inbound.kind, InboundKind::Socks | InboundKind::Mixed)
        }) {
            rules.push(json!({"inbound":inbound.tag,"network":"udp","action":"reject"}));
        }
        let mesh_dns: Vec<_> = ctx
            .mesh
            .dns
            .iter()
            .enumerate()
            .map(|(index, resolver)| {
                let mut tag = format!("xrose-mesh-dns-{index}");
                while model.dns.servers.iter().any(|s| s.tag == tag) {
                    tag.push('-');
                }
                (resolver, tag)
            })
            .collect();
        if !mesh_dns.is_empty() {
            // Each mesh resolves its own suffix before ordinary fake-IP rules.
            for (resolver, tag) in &mesh_dns {
                rules.push(
                    json!({"domain_suffix":[resolver.suffix],"action":"resolve","server":tag}),
                );
            }
            rules.push(json!({"ip_cidr":["127.0.0.153/32"],"action":"route","outbound":"direct"}));
        }
        for (index, route) in ctx.mesh.routes.iter().enumerate() {
            let tag = format!("xrose-mesh-{index}");
            outbounds.push(json!({"type":"direct","tag":tag,"bind_interface":route.interface}));
            rules.push(json!({"ip_cidr":[route.cidr],"action":"route","outbound":tag}));
        }
        rules.push(json!({"action": "sniff"}));
        if model.dns.enabled {
            rules.push(json!({"protocol": "dns", "action": "hijack-dns"}));
        }
        let mut resolve_direct = direct && model.dns.enabled;
        for rule in model.routing.rules_for_mode(model.work_mode) {
            if let Some(value) = singbox_rule(model, rule, &mut warnings) {
                if resolve_direct {
                    let m = &rule.matchers;
                    if !m.ip_cidrs.is_empty()
                        || !m.geoips.is_empty()
                        || m.rule_sets.iter().any(|tag| !tag.starts_with("geosite-"))
                    {
                        // IP firewall rules need real destination addresses even on Mixed.
                        rules.push(json!({"action": "resolve"}));
                        resolve_direct = false;
                    } else if rule.action.kind != crate::model::RouteActionKind::Block {
                        // Resolve only a flow about to be forwarded. Later process/domain
                        // blocks can still reject other flows before any resolver sees them.
                        let mut resolve = value.clone();
                        resolve.as_object_mut().unwrap().remove("outbound");
                        resolve["action"] = json!("resolve");
                        rules.push(resolve);
                    }
                }
                rules.push(value);
            }
        }

        let mut final_tag = action_tag(CoreKind::SingBox, model, &model.routing.final_action)
            .unwrap_or_else(|| "direct".into());
        if resolve_direct && final_tag != "block" {
            // Outbound bootstrap DNS bypasses the ordinary DNS policy. Resolve
            // client destinations through the configured Hosts and split rules first.
            rules.push(json!({"action": "resolve"}));
        }
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
        if !rule_sets_json.is_empty() {
            route["rule_set"] = json!(rule_sets_json);
        }

        // --- DNS ---------------------------------------------------------
        let mut dns_section = Value::Null;
        if model.dns.enabled {
            let (dns, bootstrap_tag) =
                build_dns(model, &default_group, &mut warnings, ctx.mesh_transport);
            dns_section = dns;
            route["default_domain_resolver"] = json!(bootstrap_tag);
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
            vpn_rules.append(dns_section["rules"].as_array_mut().unwrap());
            dns_section["rules"] = json!(vpn_rules);
        } else if !ctx.mesh.dns.is_empty() {
            route["default_domain_resolver"] = json!("local");
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
            for (resolver, tag) in &mesh_dns {
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
            default_proxy: super::CompiledProxySelection::from_model(model, CoreKind::SingBox),
            managed_groups: model.managed_route_groups(),
            core_version: ctx.core_version.clone(),
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

/// Minimal configuration used only by private per-node or chain probes.
#[cfg(test)]
pub(super) fn compile_probe(node: &Node, port: u16) -> Result<Value> {
    compile_probe_hops(std::slice::from_ref(node), port)
}

pub(super) fn compile_probe_hops(hops: &[Node], port: u16) -> Result<Value> {
    compile_probe_with_inbounds(
        hops,
        vec![json!({"type": "socks", "tag": "probe", "listen": "127.0.0.1", "listen_port": port})],
    )
}

pub(super) fn compile_direct_probe(port: u16) -> Value {
    compile_probe_with_inbounds(
        &[],
        vec![json!({"type": "socks", "tag": "probe", "listen": "127.0.0.1", "listen_port": port})],
    )
    .expect("direct probe compiles")
}

fn compile_probe_with_inbounds(hops: &[Node], inbounds: Vec<Value>) -> Result<Value> {
    let mut endpoints = Vec::new();
    let mut outbounds = Vec::new();
    let is_mesh_exit = hops.last().is_some_and(Node::is_mesh_gateway);
    let final_tag = if hops.is_empty() {
        outbounds.push(json!({"type": "direct", "tag": "direct"}));
        "direct".to_owned()
    } else {
        let last_idx = hops.len() - 1;
        let mut prev_tag: Option<String> = None;
        let mut final_tag = String::new();
        for (idx, node) in hops.iter().enumerate() {
            let mut outbound = node_outbound(node, &mut [])?;
            let tag = if idx == last_idx {
                node.outbound_tag()
            } else {
                format!("probe-hop-{idx}")
            };
            outbound["tag"] = json!(tag);
            if let Some(detour) = &prev_tag {
                outbound["detour"] = json!(detour);
            }
            if node.protocol == Protocol::Wireguard || node.protocol.is_vpn() {
                endpoints.push(outbound);
            } else {
                outbounds.push(outbound);
            }
            if idx == last_idx {
                final_tag = tag.clone();
            }
            prev_tag = Some(tag);
        }
        final_tag
    };
    let (dns, route) = if is_mesh_exit {
        (
            json!({
                "servers": [
                    {"type": "local", "tag": "local"},
                    {"type": "udp", "tag": "exit-dns", "server": "1.1.1.1", "server_port": 53, "detour": final_tag}
                ],
                "final": "exit-dns",
                "strategy": "ipv4_only"
            }),
            json!({
                "auto_detect_interface": true,
                "default_domain_resolver": "local",
                "rules": [{"action": "resolve", "server": "exit-dns", "strategy": "ipv4_only"}],
                "final": final_tag
            }),
        )
    } else {
        (
            json!({"servers": [{"type": "local", "tag": "local"}]}),
            json!({"auto_detect_interface": true, "default_domain_resolver": "local", "final": final_tag}),
        )
    };
    Ok(json!({
        "endpoints": endpoints,
        "log": {"level": "error"},
        "dns": dns,
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": route
    }))
}

pub(crate) fn bind_probe_interface(config: &mut Value, interface: &str, is_direct: bool) {
    // Only bind the entry hop (the one without `detour`); sing-box rejects dial
    // fields combined with `detour`.
    let bound_outbound = config["outbounds"]
        .as_array_mut()
        .and_then(|arr| arr.iter_mut().find(|entry| entry.get("detour").is_none()));
    if let Some(entry) = bound_outbound {
        entry["bind_interface"] = json!(interface);
    } else if let Some(entry) = config["endpoints"]
        .as_array_mut()
        .and_then(|arr| arr.iter_mut().find(|entry| entry.get("detour").is_none()))
    {
        entry["bind_interface"] = json!(interface);
    }
    if is_direct {
        config["dns"] = json!({
            "servers": [{
                "type": "udp",
                "tag": "local",
                "server": "223.5.5.5",
                "server_port": 53,
                "bind_interface": interface
            }],
            "final": "local"
        });
        config["route"]["rules"] = json!([{"action": "resolve", "server": "local"}]);
    } else if let Some(servers) = config["dns"]["servers"].as_array_mut() {
        for server in servers {
            if server["type"] == "local" {
                *server = json!({
                    "type": "udp",
                    "tag": "local",
                    "server": "223.5.5.5",
                    "server_port": 53,
                    "bind_interface": interface
                });
            }
        }
    }
}

fn chain_entry_tag(chain: &Node) -> String {
    format!("chain-entry-{}", chain.id)
}

fn compile_singbox_chains(
    model: &AppModel,
    outbounds: &mut Vec<Value>,
    endpoints: &mut Vec<Value>,
    valid_tags: &mut std::collections::HashSet<String>,
    warnings: &mut Vec<String>,
) -> std::collections::HashSet<String> {
    let mut filtered_entries = std::collections::HashSet::new();
    let chains: Vec<&Node> = model
        .nodes
        .iter()
        .filter(|n| n.enabled && n.is_chain())
        .collect();
    if chains.is_empty() {
        return filtered_entries;
    }
    let mut compiled = std::collections::HashSet::new();
    let mut failed: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    loop {
        let mut progress = false;
        for &chain in &chains {
            if compiled.contains(&chain.id) || failed.contains_key(&chain.id) {
                continue;
            }
            if model.node_in_cycle(&chain.id) {
                failed.insert(
                    chain.id.clone(),
                    format!("代理链“{}”存在循环引用，已跳过", chain.name),
                );
                continue;
            }
            if chain.chain_hops.len() < 2 {
                failed.insert(
                    chain.id.clone(),
                    format!("代理链“{}”跳点不足，已跳过", chain.name),
                );
                continue;
            }
            if let Err(err) = model.validate_chain_transports(&chain.chain_hops) {
                failed.insert(
                    chain.id.clone(),
                    format!("代理链“{}”已跳过：{err}", chain.name),
                );
                continue;
            }
            let entry_tag = match &chain.chain_hops[0] {
                ChainHop::Node(id) => model
                    .node(id)
                    .filter(|n| !n.is_chain() && valid_tags.contains(&n.outbound_tag()))
                    .map(Node::outbound_tag),
                ChainHop::Group(gid) => model
                    .group(gid)
                    .filter(|g| {
                        g.enabled
                            && g.members.iter().any(|mid| {
                                model.chain_entry_allows(&chain.chain_hops, mid)
                                    && model
                                        .node(mid)
                                        .is_some_and(|n| valid_tags.contains(&n.outbound_tag()))
                            })
                    })
                    .map(|g| {
                        if model.needs_filtered_chain_entry(chain, g) {
                            chain_entry_tag(chain)
                        } else {
                            g.tag()
                        }
                    }),
            };
            let Some(mut prev_tag) = entry_tag else {
                continue;
            };
            let filtered_entry = prev_tag == chain_entry_tag(chain);
            let mut trailing_nodes = Vec::with_capacity(chain.chain_hops.len() - 1);
            let mut trailing_ok = true;
            for hop in &chain.chain_hops[1..] {
                match hop {
                    ChainHop::Node(id) => {
                        if let Some(node) = model
                            .node(id)
                            .filter(|n| !n.is_chain() && valid_tags.contains(&n.outbound_tag()))
                        {
                            if node.is_mesh_gateway() {
                                failed.insert(
                                    chain.id.clone(),
                                    format!(
                                        "代理链“{}”的后续跳点包含组网出口网关，已跳过",
                                        chain.name
                                    ),
                                );
                                trailing_ok = false;
                                break;
                            }
                            trailing_nodes.push(node);
                        } else {
                            trailing_ok = false;
                            break;
                        }
                    }
                    ChainHop::Group(_) => {
                        failed.insert(
                            chain.id.clone(),
                            format!("代理链“{}”仅第 1 跳支持出站组，已跳过", chain.name),
                        );
                        trailing_ok = false;
                        break;
                    }
                }
            }
            if !trailing_ok {
                continue;
            }
            let mut staged_outbounds = Vec::new();
            let mut staged_endpoints = Vec::new();
            let mut compile_err = None;
            let last_idx = trailing_nodes.len() - 1;
            for (idx, hop_node) in trailing_nodes.iter().enumerate() {
                match node_outbound(hop_node, warnings) {
                    Ok(mut cloned) => {
                        let tag = if idx == last_idx {
                            chain.outbound_tag()
                        } else {
                            format!("chain-hop-{}-{}", chain.id, idx + 1)
                        };
                        cloned["tag"] = json!(tag);
                        cloned["detour"] = json!(prev_tag);
                        if hop_node.protocol == Protocol::Wireguard || hop_node.protocol.is_vpn() {
                            staged_endpoints.push(cloned);
                        } else {
                            staged_outbounds.push(cloned);
                        }
                        prev_tag = tag;
                    }
                    Err(err) => {
                        compile_err = Some(format!(
                            "代理链“{}”跳点“{}”编译失败：{err}",
                            chain.name, hop_node.name
                        ));
                        break;
                    }
                }
            }
            if let Some(message) = compile_err {
                failed.insert(chain.id.clone(), message);
                continue;
            }
            outbounds.extend(staged_outbounds);
            endpoints.extend(staged_endpoints);
            if filtered_entry {
                filtered_entries.insert(chain.id.clone());
            }
            valid_tags.insert(chain.outbound_tag());
            compiled.insert(chain.id.clone());
            progress = true;
        }
        if !progress {
            break;
        }
    }
    for chain in chains {
        if !compiled.contains(&chain.id) {
            warnings.push(failed.remove(&chain.id).unwrap_or_else(|| {
                format!(
                    "代理链“{}”包含不可用或当前内核不支持的跳点，已跳过",
                    chain.name
                )
            }));
        }
    }
    filtered_entries
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
                "security": node.credentials.method.as_deref().unwrap_or("auto"),
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
        Protocol::Chain => return Err(Error::config("代理链不能直接作为单跳出站编译")),
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
    tags: &std::collections::BTreeSet<String>,
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
        object.insert(
            "network".into(),
            json!(network.split(',').collect::<Vec<_>>()),
        );
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
    match raw.trim() {
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
    if let Some(rest) = address.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .unwrap_or(default_port);
        return (host.to_string(), port);
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
    default_group: &str,
    warnings: &mut Vec<String>,
    mesh_transport: bool,
) -> (Value, String) {
    let mut servers: Vec<Value> = Vec::new();
    let mut rules: Vec<Value> = Vec::new();
    let fake_enabled = model.dns.fake_ip;

    // Bootstrap proxy endpoints and encrypted resolver hostnames without cycles.
    let local = model.dns.servers.iter().find(|s| {
        if mesh_transport {
            return s.tag == super::mesh::BOOTSTRAP_DNS;
        }
        !s.requires_proxy()
            && matches!(
                s.server_type,
                crate::model::DnsServerType::Udp | crate::model::DnsServerType::Tcp
            )
            && (matches!(s.address.trim(), "system" | "local")
                || s.tag == "local"
                    && split_server_address(&s.address, 53)
                        .0
                        .parse::<std::net::IpAddr>()
                        .is_ok())
    });
    let local_tag = local.map(|s| s.tag.clone()).unwrap_or_else(|| {
        let mut tag = String::from("local");
        while model.dns.servers.iter().any(|s| s.tag == tag) {
            tag.push('-');
        }
        tag
    });

    let mut fake_tag = String::from("fakeip");
    if fake_enabled {
        while model
            .dns
            .servers
            .iter()
            .any(|s| !s.is_fakeip() && s.tag == fake_tag)
        {
            fake_tag.push('-');
        }
        let range = if model.dns.fake_ip_range.trim().is_empty() {
            "198.18.0.0/15"
        } else {
            model.dns.fake_ip_range.trim()
        };
        servers.push(json!({
            "type": "fakeip",
            "tag": fake_tag,
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

    if local.is_none() {
        servers.push(json!({"type": "local", "tag": local_tag}));
    }

    // Node server domains must resolve without the proxy, otherwise a
    // proxy-detoured remote DNS creates a bootstrap loop.
    for node in model
        .nodes
        .iter()
        .filter(|n| n.enabled && !n.is_chain() && !n.server.trim().is_empty())
    {
        if node.server.parse::<std::net::IpAddr>().is_ok() {
            continue;
        }
        rules.push(json!({
            "domain": [node.server.clone()],
            "server": local_tag
        }));
    }

    // A hosts resolver handles A and AAAA independently, including NODATA.
    if !model.dns.hosts.is_empty() {
        let mut tag = "xrose-hosts".to_string();
        while servers.iter().any(|server| server["tag"] == tag) {
            tag.push('-');
        }
        let mut hosts = std::collections::BTreeMap::new();
        for (domain, addresses) in &model.dns.hosts {
            let valid: Vec<std::net::IpAddr> =
                addresses.iter().filter_map(|v| v.parse().ok()).collect();
            if valid.len() != addresses.len() {
                warnings.push(format!(
                    "Hosts 中「{domain}」的无效 IP 地址已跳过，请检查地址"
                ));
            }
            if !valid.is_empty() && !domain.is_empty() {
                hosts.insert(domain, valid);
            }
        }
        if !hosts.is_empty() {
            rules.insert(0, json!({"domain": hosts.keys().collect::<Vec<_>>(), "action":"route", "server": tag}));
            servers.push(json!({"type": "hosts", "tag": tag, "predefined": hosts}));
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
            .map(|d| d != "direct" && d == default_group)
            .unwrap_or(false);
        let target = if fake_enabled && goes_through_proxy {
            &fake_tag
        } else {
            server.tag.as_str()
        };
        rules.push(dns_server_rule(&sets, target));
    }

    if fake_enabled && model.work_mode == crate::model::WorkMode::Direct {
        rules.push(json!({"query_type": ["A", "AAAA"], "action": "route", "server": fake_tag}));
    }

    let final_tag = model
        .dns
        .final_server
        .as_ref()
        .filter(|tag| !tag.is_empty())
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

    let dns = json!({
        "servers": servers,
        "rules": rules,
        "final": final_tag,
        "strategy": strategy
    });
    (dns, local_tag)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mesh_exit_policy_preserves_services_and_subnets_and_handles_peer_dns() {
        use crate::mesh::{ExitBridge, MeshProfile, MeshProvider, MeshSecret};
        let profile = MeshProfile {
            provider: MeshProvider::Tailscale,
            advertise_exit_node: true,
            advertised_subnets: vec!["192.0.2.0/24".into()],
            ..Default::default()
        };
        let mut config = super::compile_mesh(
            &profile,
            std::path::Path::new("fixture"),
            19090,
            "fixture",
            (19091, "fixture"),
        )
        .unwrap();
        super::allow_mesh_services(&mut config, "127.0.0.1:19092".parse().unwrap(), "fixture");
        super::route_mesh_exit(
            &mut config,
            &ExitBridge {
                port: 19093,
                password: MeshSecret("fixture-secret-only".into()),
            },
        );
        let rules = config["route"]["rules"].as_array().unwrap();
        let service = rules
            .iter()
            .position(|rule| rule["outbound"] == "mesh-services")
            .unwrap();
        let subnet = rules
            .iter()
            .position(|rule| {
                rule["inbound"] == "mesh-ts"
                    && rule["ip_cidr"] == serde_json::json!(["192.0.2.0/24"])
            })
            .unwrap();
        let policy = rules
            .iter()
            .position(|rule| rule["outbound"] == "mesh-exit-policy")
            .unwrap();
        let reject = rules
            .iter()
            .position(|rule| rule["inbound"] == "mesh-ts" && rule["action"] == "reject")
            .unwrap();
        assert!(service < policy && subnet < policy && policy < reject);
        assert!(!rules.iter().any(|rule| {
            rule["outbound"] == "direct"
                && rule["ip_cidr"]
                    .as_array()
                    .is_some_and(|nets| nets.contains(&serde_json::json!("0.0.0.0/0")))
        }));
        assert_eq!(config["dns"]["rules"][0]["inbound"], "mesh-ts");
        assert_eq!(config["dns"]["rules"][0]["server"], "mesh-exit-policy-dns");
        let dns = config["dns"]["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["tag"] == "mesh-exit-policy-dns")
            .unwrap();
        assert_eq!(dns["detour"], "mesh-exit-policy");
        let outbound = config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|outbound| outbound["tag"] == "mesh-exit-policy")
            .unwrap();
        assert_eq!(outbound["server"], "127.0.0.1");
        assert_eq!(outbound["password"], "fixture-secret-only");
    }

    #[test]
    fn mesh_service_forward_matches_only_own_endpoint_and_selected_port() {
        use crate::mesh::{ForwardProtocol, MeshProfile, MeshProvider, ServiceForward};
        let profile = MeshProfile {
            provider: MeshProvider::Tailscale,
            service_forwards: vec![
                ServiceForward {
                    protocol: ForwardProtocol::Tcp,
                    port: 18080,
                    target: "192.0.2.10:80".parse().unwrap(),
                },
                ServiceForward {
                    protocol: ForwardProtocol::Udp,
                    port: 15353,
                    target: "[::1]:5353".parse().unwrap(),
                },
            ],
            ..Default::default()
        };
        let mut config = super::compile_mesh(
            &profile,
            std::path::Path::new("fixture"),
            19090,
            "fixture",
            (19091, "fixture"),
        )
        .unwrap();
        assert_eq!(
            config["endpoints"][0]["advertise_routes"],
            serde_json::json!([])
        );
        super::allow_mesh_services(&mut config, "127.0.0.1:19092".parse().unwrap(), "fixture");
        let rules = config["route"]["rules"].as_array().unwrap();
        let rule = &rules[0];
        assert_eq!(rule["inbound"], "mesh-ts");
        assert_eq!(
            rule["ip_cidr"],
            serde_json::json!(["127.0.0.1/32", "::1/128"])
        );
        assert_eq!(rule["outbound"], "mesh-services");
        assert!(rule.get("override_address").is_none());
        let mut empty = profile.clone();
        empty.service_forwards.clear();
        let mut unchanged = super::compile_mesh(
            &empty,
            std::path::Path::new("fixture"),
            19090,
            "fixture",
            (19091, "fixture"),
        )
        .unwrap();
        super::allow_mesh_services(
            &mut unchanged,
            "127.0.0.1:19092".parse().unwrap(),
            "fixture",
        );
        assert_eq!(config, unchanged);
        assert!(
            rules
                .iter()
                .any(|r| r["inbound"] == "mesh-ts" && r["action"] == "reject")
        );
    }
    #[test]
    fn managed_route_pins_an_eligible_node_and_keeps_other_groups_idle() {
        let (mut model, settings, ctx) = sample();
        let unavailable = model.nodes[0].id.clone();
        model.nodes[0].enabled = false;
        let valid = Node::new("可用节点", Protocol::Socks, "candidate.example", 1080);
        model.add_nodes([valid.clone()]);
        for g in &mut model.groups {
            g.kind = GroupKind::Fallback;
            g.switch_policy = Default::default();
            g.selected = Some(unavailable.clone());
        }
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert_eq!(compiled.managed_groups.len(), 1);
        assert_eq!(compiled.managed_groups[0].id, "proxy");
        assert_eq!(compiled.managed_groups[0].members, [valid.id.clone()]);
        let outbound = compiled.json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "group-proxy")
            .unwrap();
        assert_eq!(outbound["type"], "selector");
        assert_eq!(outbound["outbounds"], json!([valid.outbound_tag()]));
        assert_eq!(outbound["default"], valid.outbound_tag());
    }

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
        assert_eq!(degraded.default_proxy.node.as_ref(), Some(&existing));
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
        assert_eq!(xray.default_proxy.node.as_ref(), Some(&existing));
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
        let mut model = AppModel {
            work_mode: crate::model::WorkMode::Proxy,
            dns: crate::model::DnsConfig::default(),
            ..Default::default()
        };
        model.apply_preset(crate::model::RoutePreset::BypassMainland);
        let mut node = Node::new("n", Protocol::Vless, "example.com", 443);
        node.credentials.uuid = Some("11111111-2222-3333-4444-555555555555".into());
        node.tls.enabled = true;
        model.add_nodes(vec![node]);
        let settings = Settings::default();
        let ctx = CompileContext::new(
            &settings,
            std::path::PathBuf::from("/tmp/xrose-test-data"),
            std::path::PathBuf::from("/tmp/xrose-test-run"),
        );
        (model, settings, ctx)
    }

    #[test]
    fn legacy_front_node_and_update_proxy_do_not_chain_vless_outbounds() {
        let (mut model, mut settings, ctx) = sample();
        let front = Node::new("Fixture", Protocol::Socks, "front.example.invalid", 1080);
        let front_id = front.id.clone();
        model.add_nodes([front]);
        let mut old_profile = serde_json::to_value(&model).unwrap();
        old_profile["front_proxy"] = json!(front_id);
        let mut model: AppModel = serde_json::from_value(old_profile).unwrap();
        settings.update_proxy =
            crate::updates::Socks5Proxy::parse("socks5://localhost:1080").unwrap();
        settings.tun_enabled = true;
        settings.sync_inbounds(&mut model);
        for core in CoreKind::all() {
            let config = super::super::adapter_for(core)
                .compile(&model, &settings, &ctx)
                .unwrap();
            let outbounds = config.json["outbounds"].as_array().unwrap();
            let target = outbounds
                .iter()
                .find(|o| o["type"] == "vless" || o["protocol"] == "vless")
                .unwrap();
            assert!(target.get("detour").is_none());
            assert!(
                target["streamSettings"]["sockopt"]
                    .get("dialerProxy")
                    .is_none()
            );
        }
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
                interface: "xrose-mesh-ts-test".into(),
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
        assert_eq!(tun["strict_route"], json!(true));
        assert_eq!(
            compiled.json["dns"]["rules"][0]["server"],
            "xrose-mesh-dns-0"
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
        let json = compile_mesh(
            &p,
            Path::new("mesh-test"),
            19090,
            "fake-key",
            (19091, "fixture-forward"),
        )
        .unwrap();
        assert_eq!(json["endpoints"][0]["system_interface"], false);
        assert_eq!(json["endpoints"][0]["advertise_exit_node"], false);
        assert_eq!(json["endpoints"][0]["advertise_routes"], json!([]));
        assert_eq!(json["route"]["auto_detect_interface"], !cfg!(windows));
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
    fn mesh_exit_gateway_forwarding_and_subnet_publication() {
        use crate::mesh::{MeshProfile, MeshProvider, MeshSecret, PortForward};
        let mut p = MeshProfile {
            provider: MeshProvider::Tailscale,
            exit_node: "100.64.1.1".into(),
            outbound_port: 19081,
            outbound_secret: MeshSecret("fixture-secret".into()),
            advertised_subnets: vec!["192.168.50.0/24".into()],
            port_forwards: vec![PortForward::parse("udp://127.0.0.1:5533/100.64.1.2:53").unwrap()],
            ..Default::default()
        };
        let config = compile_mesh(
            &p,
            Path::new("fixture"),
            19090,
            "fixture-api",
            (19091, "fixture-forward"),
        )
        .unwrap();
        assert_eq!(config["endpoints"][0]["exit_node"], "100.64.1.1");
        assert_eq!(config["route"]["auto_detect_interface"], true);
        assert_eq!(
            config["dns"]["servers"][1]["accept_default_resolvers"],
            true
        );
        assert_eq!(config["endpoints"][0]["exit_node_allow_lan_access"], false);
        assert_eq!(
            config["endpoints"][0]["advertise_routes"],
            json!(["192.168.50.0/24"])
        );
        assert_eq!(
            config["inbounds"][2]["users"][0]["password"],
            "fixture-secret"
        );
        assert_eq!(config["inbounds"][3]["listen"], "127.0.0.1");
        assert_eq!(
            config["inbounds"][3]["users"][0]["password"],
            "fixture-forward"
        );
        assert_eq!(config["route"]["rules"][0]["outbound"], "mesh-ts");
        assert!(
            !config["inbounds"][1]["route_address"]
                .as_array()
                .unwrap()
                .contains(&json!("0.0.0.0/0"))
        );
        p.exit_node.clear();
        p.outbound_port = 0;
        p.advertise_exit_node = true;
        let config = compile_mesh(
            &p,
            Path::new("fixture"),
            19090,
            "fixture-api",
            (19091, "fixture-forward"),
        )
        .unwrap();
        assert_eq!(config["endpoints"][0]["advertise_exit_node"], true);
        assert!(
            config["route"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["inbound"] == "mesh-ts" && r["outbound"] == "direct")
        );
        assert!(
            !config["inbounds"][1]["route_address"]
                .as_array()
                .unwrap()
                .contains(&json!("0.0.0.0/0"))
        );
    }

    #[test]
    fn rejects_missing_nodes() {
        let mut model = AppModel {
            work_mode: crate::model::WorkMode::Proxy,
            ..Default::default()
        };
        model.nodes.clear();
        let settings = Settings::default();
        let ctx = CompileContext::new(
            &settings,
            std::path::PathBuf::from("/tmp/xrose-test-data"),
            std::path::PathBuf::from("/tmp/xrose-test-run"),
        );
        assert!(SingBoxAdapter.compile(&model, &settings, &ctx).is_err());
    }

    #[test]
    fn selector_uses_singbox_native_type() {
        let (mut model, settings, ctx) = sample();
        let mut group = crate::model::OutboundGroup::new("测速组", GroupKind::UrlTest);
        group.members = model.groups[0].members.clone();
        let group_id = group.id.clone();
        model.groups.push(group);
        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let outbounds = compiled.json["outbounds"].as_array().unwrap();
        assert!(outbounds.iter().any(|o| o["type"] == "selector"));
        assert!(outbounds.iter().any(|o| o["type"] == "urltest"));

        let second = Node::new("备用", Protocol::Socks, "backup.example", 1080);
        model.add_nodes([second.clone()]);
        let group = model.group_mut(&group_id).unwrap();
        group.members.push(second.id.clone());
        group.selected = Some(second.id.clone());
        group.switch_policy.mode = crate::model::SwitchMode::Off;
        let tag = group.tag();
        model.default_group = Some(group_id);
        let manual = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(manual.managed_groups.is_empty());
        let outbound = manual.json["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == tag)
            .unwrap();
        assert_eq!(outbound["type"], "selector");
        assert_eq!(outbound["default"], second.outbound_tag());
        assert_eq!(outbound["outbounds"].as_array().unwrap().len(), 2);
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
    #[test]
    fn compiles_separate_authenticated_lan() {
        let (mut model, mut settings, ctx) = sample();
        let password = uuid::Uuid::new_v4().to_string();
        settings.allow_lan = true;
        settings.separate_lan_port = true;
        settings.lan_auth = true;
        settings.lan_account = crate::settings::ProxyAccount {
            username: "fixture".into(),
            password: password.clone(),
        };
        settings.sync_inbounds(&mut model);
        let config = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let items = config.json["inbounds"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["listen"], "127.0.0.1");
        assert!(items[0]["users"].is_null());
        assert_eq!(items[1]["listen"], "0.0.0.0");
        assert_eq!(items[1]["listen_port"], settings.lan_port);
        assert_eq!(items[1]["users"][0]["password"], password);
        settings.separate_lan_port = false;
        settings.sync_inbounds(&mut model);
        let shared = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert_eq!(shared.json["inbounds"][0]["users"][0]["password"], password);
    }

    #[test]
    fn bind_probe_interface_routes_dns_and_keeps_mesh_exit_dns() {
        use crate::adapters::singbox::bind_probe_interface;

        let mut direct = super::super::compile_direct_probe(CoreKind::SingBox, 18080);
        bind_probe_interface(&mut direct, "en0", true);
        assert_eq!(direct["outbounds"][0]["bind_interface"], "en0");
        assert_eq!(direct["dns"]["servers"][0]["type"], "udp");
        assert_eq!(direct["dns"]["servers"][0]["server"], "223.5.5.5");
        assert_eq!(direct["dns"]["servers"][0]["server_port"], 53);
        assert_eq!(direct["dns"]["servers"][0]["bind_interface"], "en0");
        assert_eq!(direct["route"]["rules"][0]["action"], "resolve");

        let mut gateway = Node::new("mesh-exit", Protocol::Socks, "127.0.0.1", 19080);
        gateway.id = "mesh-exit-test".into();
        gateway.tags.push("mesh-exit".into());
        let mut mesh_probe =
            super::super::compile_probe(CoreKind::SingBox, &gateway, 18081).unwrap();
        bind_probe_interface(&mut mesh_probe, "en0", false);
        assert_eq!(mesh_probe["outbounds"][0]["bind_interface"], "en0");
        assert_eq!(mesh_probe["dns"]["final"], "exit-dns");
        let servers = mesh_probe["dns"]["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);
        let local = servers.iter().find(|s| s["tag"] == "local").unwrap();
        assert_eq!(local["type"], "udp");
        assert_eq!(local["server"], "223.5.5.5");
        assert_eq!(local["bind_interface"], "en0");
        let exit_dns = servers.iter().find(|s| s["tag"] == "exit-dns").unwrap();
        assert_eq!(exit_dns["detour"], gateway.outbound_tag());
    }

    #[test]
    fn tuic_exit_uses_vision_for_live_traffic_and_isolated_probes() {
        use crate::model::{OutboundGroup, RealityOptions};
        let (mut model, settings, ctx) = sample();
        let entry = &mut model.nodes[0];
        entry.credentials.flow = Some("xtls-rprx-vision".into());
        entry.tls.reality = Some(RealityOptions {
            public_key: crate::util::encode_base64(&[1; 32]),
            short_id: "0011".into(),
        });
        let entry = entry.clone();
        let mut exit = Node::new("TUIC", Protocol::Tuic, "exit.test", 8443);
        exit.credentials.uuid = entry.credentials.uuid.clone();
        exit.credentials.password = Some("fixture".into());
        exit.tls.enabled = true;
        exit.tls.server_name = Some("tls.exit.test".into());
        let http = Node::new("HTTP", Protocol::Http, "proxy.test", 8080);
        let mut group = OutboundGroup::new("Entry", GroupKind::UrlTest);
        group.members = vec![exit.id.clone(), http.id.clone(), entry.id.clone()];
        group.selected = Some(exit.id.clone());
        let ordinary = Node::new_chain(
            "fixed",
            vec![
                ChainHop::Node(entry.id.clone()),
                ChainHop::Node(exit.id.clone()),
            ],
        );
        let grouped = Node::new_chain(
            "grouped",
            vec![
                ChainHop::Group(group.id.clone()),
                ChainHop::Node(exit.id.clone()),
            ],
        );
        model.add_nodes([
            exit.clone(),
            http.clone(),
            ordinary.clone(),
            grouped.clone(),
        ]);
        model.groups.push(group.clone());

        for kind in [GroupKind::UrlTest, GroupKind::Selector] {
            model.group_mut(&group.id).unwrap().kind = kind;
            let config = SingBoxAdapter
                .compile(&model, &settings, &ctx)
                .unwrap()
                .json;
            let outs = config["outbounds"].as_array().unwrap();
            let get = |tag: String| outs.iter().find(|out| out["tag"] == tag).unwrap();
            let filtered_tag = chain_entry_tag(&grouped);
            let restricted = get(filtered_tag.clone());
            assert_eq!(
                restricted["type"],
                if kind == GroupKind::UrlTest {
                    "urltest"
                } else {
                    "selector"
                }
            );
            assert_eq!(restricted["outbounds"], json!([entry.outbound_tag()]));
            assert_eq!(get(group.tag())["outbounds"].as_array().unwrap().len(), 3);
            assert_eq!(get(grouped.outbound_tag())["detour"], filtered_tag);
            let landing = get(ordinary.outbound_tag());
            assert_eq!(landing["detour"], entry.outbound_tag());
            assert_eq!(landing["server_port"], 8443);
            assert_eq!(landing["tls"]["server_name"], "tls.exit.test");
            assert!(get(exit.outbound_tag())["detour"].is_null());

            let hops = model
                .resolve_probe_hops(&grouped, CoreKind::SingBox)
                .unwrap();
            let probe = compile_probe_hops(&hops, 1080).unwrap();
            let probe_entry = &probe["outbounds"][0];
            assert_eq!(probe_entry["flow"], "xtls-rprx-vision");
            assert_eq!(probe_entry["tls"]["reality"]["enabled"], true);
            assert_eq!(probe_entry["packet_encoding"], "xudp");
            let probe_exit = &probe["outbounds"][1];
            assert_eq!(probe_exit["type"], "tuic");
            assert_eq!(probe_exit["detour"], probe_entry["tag"]);
            assert_eq!(probe_exit["tls"], landing["tls"]);
            assert_eq!(probe["route"]["final"], probe_exit["tag"]);
        }

        model.node_mut(&entry.id).unwrap().udp = false;
        let invalid = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(invalid.warnings.iter().any(|w| w.contains("UDP/8443")));
        assert!(
            invalid.json["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|out| out["tag"] != ordinary.outbound_tag()
                    && out["tag"] != grouped.outbound_tag())
        );
    }

    #[test]
    fn compiles_proxy_chains_with_cloned_detours_group_entry_and_probe() {
        use crate::adapters::singbox::bind_probe_interface;
        use crate::model::{ChainHop, OutboundGroup};

        let (mut model, settings, ctx) = sample();
        let entry = model.nodes[0].clone();
        let mut mid = Node::new("Mid-SOCKS", Protocol::Socks, "203.0.113.20", 1080);
        mid.id = "node-mid".into();
        let mut exit = Node::new("US-Home", Protocol::Socks, "203.0.113.30", 1080);
        exit.id = "node-exit".into();
        model.add_nodes([mid.clone(), exit.clone()]);

        let mut relay = OutboundGroup::new("HK Relay", GroupKind::UrlTest);
        relay.members = vec![entry.id.clone()];
        relay.selected = Some(entry.id.clone());
        model.groups.push(relay.clone());

        let chain_3 = Node::new_chain(
            "3-Hop",
            vec![
                ChainHop::Node(entry.id.clone()),
                ChainHop::Node(mid.id.clone()),
                ChainHop::Node(exit.id.clone()),
            ],
        );
        let chain_grp = Node::new_chain(
            "Group-Entry",
            vec![
                ChainHop::Group(relay.id.clone()),
                ChainHop::Node(exit.id.clone()),
            ],
        );
        model.add_nodes([chain_3.clone(), chain_grp.clone()]);

        let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        let outbounds = compiled.json["outbounds"].as_array().unwrap();

        // Original single-hop nodes must remain untouched without `detour`.
        let orig_mid = outbounds
            .iter()
            .find(|o| o["tag"] == mid.outbound_tag())
            .unwrap();
        assert!(orig_mid.get("detour").is_none());
        let orig_exit = outbounds
            .iter()
            .find(|o| o["tag"] == exit.outbound_tag())
            .unwrap();
        assert!(orig_exit.get("detour").is_none());

        // 3-hop chain clones hop 1 and final hop with sequential detours.
        let mid_clone_tag = format!("chain-hop-{}-1", chain_3.id);
        let mid_clone = outbounds
            .iter()
            .find(|o| o["tag"] == mid_clone_tag)
            .unwrap();
        assert_eq!(mid_clone["detour"], entry.outbound_tag());
        let exit_clone = outbounds
            .iter()
            .find(|o| o["tag"] == chain_3.outbound_tag())
            .unwrap();
        assert_eq!(exit_clone["detour"], mid_clone_tag);

        // Group-entry chain detours directly through the sing-box group tag.
        let grp_exit = outbounds
            .iter()
            .find(|o| o["tag"] == chain_grp.outbound_tag())
            .unwrap();
        assert_eq!(grp_exit["detour"], relay.tag());

        // DNS bootstrap rules must never contain empty chain server hostnames.
        if let Some(rules) = compiled.json["dns"]["rules"].as_array() {
            for rule in rules {
                if let Some(domains) = rule["domain"].as_array() {
                    assert!(domains.iter().all(|d| !d.as_str().unwrap_or("").is_empty()));
                }
            }
        }

        // Multi-hop isolated probe binds interface ONLY on the entry hop (not detoured hops).
        let mut probe = super::super::compile_probe_hops(
            CoreKind::SingBox,
            &[entry.clone(), exit.clone()],
            18082,
        )
        .unwrap();
        bind_probe_interface(&mut probe, "en0", false);
        let probe_outs = probe["outbounds"].as_array().unwrap();
        assert_eq!(probe_outs.len(), 2);
        assert_eq!(probe_outs[0]["tag"], "probe-hop-0");
        assert!(probe_outs[0].get("detour").is_none());
        assert_eq!(probe_outs[0]["bind_interface"], "en0");
        assert_eq!(probe_outs[1]["tag"], exit.outbound_tag());
        assert_eq!(probe_outs[1]["detour"], "probe-hop-0");
        assert!(probe_outs[1].get("bind_interface").is_none());
        assert_eq!(probe["route"]["final"], exit.outbound_tag());
    }
}
