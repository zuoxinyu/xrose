use std::path::Path;

use serde_json::{json, Map, Value};

use super::{
    action_tag, dns_server_rule, network_str, proxy_group_tag, split_dns_domains, CompileContext,
    CompiledConfig, CompiledInbound, CoreAdapter,
};
use crate::error::{Error, Result};
use crate::model::{
    AppModel, CoreKind, DomainStrategy, GroupKind, Inbound, InboundKind, Node, Protocol, RuleSet,
    RuleSetFormat, Transport,
};
use crate::settings::Settings;

pub struct SingBoxAdapter;

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
                    inbounds.push(tun_inbound(inbound));
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
            match node_outbound(node, &mut warnings) {
                Ok(value) => outbounds.push(value),
                Err(err) => warnings.push(format!("节点“{}”编译失败：{err}", node.name)),
            }
        }
        if outbounds.is_empty() {
            return Err(Error::config("没有可编译的出站节点"));
        }
        // `block` is a legacy special outbound; reject is expressed as a rule action.
        outbounds.push(json!({"type": "direct", "tag": "direct"}));

        let default_group =
            proxy_group_tag(model, &model.routing.final_action).unwrap_or_else(|| "direct".into());

        for group in model.groups.iter().filter(|g| g.enabled) {
            let members: Vec<String> = group
                .members
                .iter()
                .filter_map(|id| model.node(id))
                .filter(|n| n.usable_with(CoreKind::SingBox))
                .map(|n| n.outbound_tag())
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
        for rule in model.routing.rules.iter().filter(|r| r.enabled) {
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
        for rule in model.routing.rules.iter().filter(|r| r.enabled) {
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
            "auto_detect_interface": true
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
        root.insert("inbounds".into(), json!(inbounds));
        root.insert("outbounds".into(), json!(outbounds));
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
        Protocol::Trojan => {
            let password = node
                .credentials
                .password
                .as_ref()
                .ok_or_else(|| Error::config("缺少密码"))?;
            json!({
                "type": "trojan",
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
        Protocol::Wireguard => {
            return Err(Error::config("WireGuard 出站尚未支持（模型缺少密钥字段）"));
        }
    };

    if let Some(tls) = tls_json(node) {
        outbound["tls"] = tls;
    }
    if let Some(transport) = transport_json(node) {
        outbound["transport"] = transport;
    }
    Ok(outbound)
}

fn tls_json(node: &Node) -> Option<Value> {
    let tls = &node.tls;
    let required = matches!(node.protocol, Protocol::Hysteria2 | Protocol::Tuic);
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

fn default_rule_set_url(tag: &str) -> Option<String> {
    if let Some(name) = tag.strip_prefix("geosite-") {
        Some(format!(
            "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-{name}.srs"
        ))
    } else if let Some(code) = tag.strip_prefix("geoip-") {
        Some(format!(
            "https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-{code}.srs"
        ))
    } else {
        None
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
        if let Some(configured) = model.routing.rule_sets.iter().find(|r| &r.tag == tag) {
            result.push(rule_set_json(configured, ctx, default_group));
            continue;
        }
        if let Some(url) = default_rule_set_url(tag) {
            let set = RuleSet {
                tag: tag.clone(),
                url,
                format: RuleSetFormat::Binary,
                download_detour: false,
                update_interval_hours: 168,
            };
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
        let outbounds = json["outbounds"].as_array().unwrap();
        assert!(outbounds.iter().all(|o| o["tag"] != "block"));
        assert!(!json.to_string().contains("geoip\""));
        assert!(!json.to_string().contains("geosite\""));
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
}
