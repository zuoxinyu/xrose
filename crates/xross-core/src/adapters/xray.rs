use std::path::Path;

use serde_json::{json, Map, Value};

use super::{
    action_tag, network_str, xray_dns_address, CompileContext, CompiledConfig, CompiledInbound,
    CoreAdapter,
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

        for inbound in model.inbounds.iter().filter(|i| i.enabled) {
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
                    inbounds.push(socks_inbound(inbound)?);
                    inbounds.push(http_inbound(inbound, http_port)?);
                    compiled_inbounds.push(CompiledInbound {
                        tag: inbound.tag.clone(),
                        kind: "socks".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                    compiled_inbounds.push(CompiledInbound {
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
                    inbounds.push(socks_inbound(inbound)?);
                    compiled_inbounds.push(CompiledInbound {
                        tag: inbound.tag.clone(),
                        kind: "socks".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                }
                InboundKind::Http => {
                    inbounds.push(http_inbound(inbound, inbound.port)?);
                    compiled_inbounds.push(CompiledInbound {
                        tag: inbound.tag.clone(),
                        kind: "http".into(),
                        port: inbound.port,
                        allow_lan: inbound.allow_lan,
                    });
                }
                InboundKind::Tun => {
                    warnings.push(
                        "Xray 不内建 TUN，已跳过 TUN 入站；请使用 sing-box 或配合 tun2socks".into(),
                    );
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
                "没有可用的入站。Xray 需要至少一个 SOCKS/HTTP/透明代理入站（不支持 TUN）",
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
            match node_outbound(node, &mut warnings) {
                Ok(value) => outbounds.push(value),
                Err(err) => warnings.push(format!("节点“{}”编译失败：{err}", node.name)),
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
            if group.kind == GroupKind::Selector {
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
        for rule in model.routing.rules.iter().filter(|r| r.enabled) {
            if let Some(value) = xray_rule(model, rule, &mut warnings) {
                rules.push(value);
            }
        }
        let final_tag = action_tag(CoreKind::Xray, model, &model.routing.final_action)
            .unwrap_or_else(|| "direct".into());
        // Xray has no "final" field: append a catch-all rule.
        rules.push(json!({"type": "field", "network": "tcp,udp", "outboundTag": final_tag}));

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
                    let mut entry = json!({"address": address});
                    if !server.domains.is_empty() {
                        entry["domains"] = json!(server
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
                            .collect::<Vec<_>>());
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
                .or_else(|| Some("localhost".into()));
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

        let default_outbound = final_tag;
        Ok(CompiledConfig {
            core: CoreKind::Xray,
            json: Value::Object(root),
            warnings,
            inbounds: compiled_inbounds,
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

fn socks_inbound(inbound: &Inbound) -> Result<Value> {
    if inbound.port == 0 {
        return Err(Error::config(format!("入站 {} 端口不能为 0", inbound.tag)));
    }
    Ok(json!({
        "tag": inbound.tag,
        "listen": inbound.listen_address(),
        "port": inbound.port,
        "protocol": "socks",
        "settings": {"auth": "noauth", "udp": inbound.udp, "userLevel": 0},
        "sniffing": sniffing(inbound)
    }))
}

fn http_inbound(inbound: &Inbound, port: u16) -> Result<Value> {
    if port == 0 {
        return Err(Error::config(format!("入站 {} 端口不能为 0", inbound.tag)));
    }
    Ok(json!({
        "tag": if port == inbound.port { inbound.tag.clone() } else { format!("{}-http", inbound.tag) },
        "listen": inbound.listen_address(),
        "port": port,
        "protocol": "http",
        "settings": {"timeout": 300},
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
            return Err(Error::config("WireGuard 出站尚未支持（模型缺少密钥字段）"));
        }
        Protocol::Hysteria2 | Protocol::Tuic => {
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
    object.insert("outboundTag".into(), json!(target));
    Some(Value::Object(object))
}

#[cfg(test)]
mod tests {
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
        let (model, settings, ctx) = sample();
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        let balancers = compiled.json["routing"]["balancers"].as_array().unwrap();
        assert!(balancers.iter().any(|b| b["tag"] == "group-auto"));
        assert!(compiled.json["observatory"].is_object());
    }

    #[test]
    fn skips_fakeip_with_warning() {
        let (model, settings, ctx) = sample();
        let compiled = XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        assert!(compiled.warnings.iter().any(|w| w.contains("fake-ip")));
        assert!(compiled.json["dns"]["servers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["address"] != "fakeip"));
    }
}
