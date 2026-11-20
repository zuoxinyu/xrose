//! Xray JSON full configuration parser.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::Result;
use crate::model::{
    DnsConfig, DnsServer, DnsServerType, DomainStrategy, GroupKind, Matchers, Network, Node,
    NodeSource, OutboundGroup, Protocol, RealityOptions, RouteAction, RoutePreset, RouteRule,
    Transport, WireguardOptions,
};
use crate::route::transfer::safe_tag;

use super::{
    ConfigFormat, ParsedConfig, ParsedProfile, ParsedRouting, compact_rules_if_large,
    dedup_warnings, normalize_ip_cidr, parse_dns_address,
};

pub(crate) fn parse(root: &Value) -> Result<ParsedConfig> {
    let mut warnings = Vec::new();
    let mut nodes = Vec::new();
    let mut node_tags: BTreeMap<String, String> = BTreeMap::new();
    let mut direct_tags: BTreeSet<String> = BTreeSet::from(["direct".to_string()]);
    let mut block_tags: BTreeSet<String> = BTreeSet::from(["block".to_string()]);
    let mut dns_tags: BTreeSet<String> = BTreeSet::from(["dns-out".to_string()]);

    if let Some(outbounds) = root.get("outbounds").and_then(Value::as_array) {
        for (index, outbound) in outbounds.iter().enumerate() {
            let Some(proto) = outbound.get("protocol").and_then(Value::as_str) else {
                continue;
            };
            let tag = outbound
                .get("tag")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("{proto}-{}", index + 1));
            match proto.to_ascii_lowercase().as_str() {
                "freedom" => {
                    direct_tags.insert(tag);
                }
                "blackhole" => {
                    block_tags.insert(tag);
                }
                "dns" => {
                    dns_tags.insert(tag);
                }
                "loopback" => {}
                _ => match parse_xray_outbound(outbound, &tag, &mut warnings) {
                    Ok(mut node) => {
                        node.source = NodeSource::Import;
                        node_tags.entry(tag).or_insert_with(|| node.id.clone());
                        nodes.push(node);
                    }
                    Err(reason) => warnings.push(reason),
                },
            }
        }
    }

    // Parse routing.balancers into outbound groups.
    let mut groups = Vec::new();
    let mut balancer_tag_to_id: BTreeMap<String, String> = BTreeMap::new();
    if let Some(balancers) = root
        .get("routing")
        .and_then(|r| r.get("balancers"))
        .and_then(Value::as_array)
    {
        for balancer in balancers {
            let Some(tag) = balancer
                .get("tag")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            else {
                continue;
            };
            let selectors = json_string_list(balancer.get("selector"));
            let mut members = Vec::new();
            for (node_tag, node_id) in &node_tags {
                if selectors.iter().any(|prefix| node_tag.starts_with(prefix))
                    && !members.contains(node_id)
                {
                    members.push(node_id.clone());
                }
            }
            if members.is_empty() {
                continue;
            }
            let strategy_type = balancer
                .get("strategy")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("random")
                .to_ascii_lowercase();
            let kind = match strategy_type.as_str() {
                "leastping" | "leastload" => GroupKind::UrlTest,
                "random" => GroupKind::LoadBalance,
                _ => GroupKind::Selector,
            };
            let mut group = OutboundGroup::new(tag, kind);
            group.members = members;
            group.repair();
            balancer_tag_to_id.insert(tag.to_string(), group.id.clone());
            groups.push(group);
        }
    }

    // Collect DNS tag detours encoded as routing rules (`inboundTag: [dns_tag] -> outboundTag`).
    let mut dns_inbound_detours: BTreeMap<String, String> = BTreeMap::new();
    if let Some(rules) = root
        .get("routing")
        .and_then(|r| r.get("rules"))
        .and_then(Value::as_array)
    {
        for rule in rules {
            let inbounds = json_string_list(rule.get("inboundTag"));
            if inbounds.len() == 1
                && rule.get("domain").is_none()
                && rule.get("ip").is_none()
                && rule.get("port").is_none()
            {
                let target = rule
                    .get("balancerTag")
                    .and_then(Value::as_str)
                    .or_else(|| rule.get("outboundTag").and_then(Value::as_str));
                if let Some(target) = target {
                    let detour = if direct_tags.contains(target) {
                        "direct".to_string()
                    } else if let Some(gid) = balancer_tag_to_id.get(target) {
                        gid.clone()
                    } else {
                        "proxy".to_string()
                    };
                    dns_inbound_detours.insert(inbounds[0].clone(), detour);
                }
            }
        }
    }

    let dns = root
        .get("dns")
        .and_then(|dns| parse_xray_dns(dns, &dns_inbound_detours, &mut warnings));

    let routing = root.get("routing").and_then(|routing| {
        parse_xray_routing(
            routing,
            root.get("dns"),
            &direct_tags,
            &block_tags,
            &dns_tags,
            &balancer_tag_to_id,
            &node_tags,
            &dns_inbound_detours,
            &mut warnings,
        )
    });

    let default_group = routing
        .as_ref()
        .and_then(|r| r.final_action.as_ref())
        .and_then(|a| a.group.clone())
        .or_else(|| groups.first().map(|g| g.id.clone()));

    dedup_warnings(&mut warnings);

    let profile = ParsedProfile {
        name: "Xray 配置".into(),
        work_mode: None,
        nodes,
        groups,
        default_group,
        routing,
        dns,
        warnings: Vec::new(),
    };
    let has_profile_data = !profile.counts(0).available_sections().is_empty();

    Ok(ParsedConfig {
        format: ConfigFormat::XrayJson,
        exported_at: None,
        profiles: if has_profile_data {
            vec![profile]
        } else {
            Vec::new()
        },
        subscriptions: Vec::new(),
        warnings,
    })
}

fn parse_xray_outbound(
    outbound: &Value,
    tag: &str,
    warnings: &mut Vec<String>,
) -> std::result::Result<Node, String> {
    let proto_str = outbound
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(protocol) = Protocol::parse(proto_str) else {
        return Err(format!("出站“{tag}”的协议 {proto_str} 暂不支持，已跳过"));
    };
    let settings = outbound.get("settings");
    let mut node = match protocol {
        Protocol::Vmess | Protocol::Vless => {
            let vnext = settings
                .and_then(|s| s.get("vnext"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .ok_or_else(|| format!("出站“{tag}”缺少 settings.vnext"))?;
            let host = vnext
                .get("address")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("出站“{tag}”缺少服务器地址"))?;
            let port = json_u16(vnext.get("port"))
                .filter(|p| *p > 0)
                .ok_or_else(|| format!("出站“{tag}”缺少有效端口"))?;
            let user = vnext
                .get("users")
                .and_then(Value::as_array)
                .and_then(|a| a.first());
            let mut node = Node::new(tag, protocol, host, port);
            if let Some(user) = user {
                node.credentials.uuid = user.get("id").and_then(Value::as_str).map(str::to_string);
                node.credentials.flow = user
                    .get("flow")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                node.credentials.alter_id =
                    user.get("alterId").and_then(Value::as_u64).unwrap_or(0) as u32;
                if protocol == Protocol::Vmess {
                    node.credentials.method = user
                        .get("security")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
            node
        }
        Protocol::Trojan | Protocol::Shadowsocks | Protocol::Socks | Protocol::Http => {
            let server = settings
                .and_then(|s| s.get("servers"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .ok_or_else(|| format!("出站“{tag}”缺少 settings.servers"))?;
            let host = server
                .get("address")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("出站“{tag}”缺少服务器地址"))?;
            let port = json_u16(server.get("port"))
                .filter(|p| *p > 0)
                .ok_or_else(|| format!("出站“{tag}”缺少有效端口"))?;
            let mut node = Node::new(tag, protocol, host, port);
            match protocol {
                Protocol::Trojan => {
                    node.credentials.password = server
                        .get("password")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    node.tls.enabled = true;
                }
                Protocol::Shadowsocks => {
                    node.credentials.method = server
                        .get("method")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    node.credentials.password = server
                        .get("password")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                Protocol::Socks | Protocol::Http => {
                    if let Some(user) = server
                        .get("users")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                    {
                        node.credentials.username = user
                            .get("user")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        node.credentials.password = user
                            .get("pass")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                }
                _ => {}
            }
            node
        }
        Protocol::Wireguard => return parse_xray_wireguard(settings, tag),
        _ => {
            return Err(format!(
                "出站“{tag}”的协议 {} 无法从 Xray 配置导入",
                protocol.display_name()
            ));
        }
    };

    if let Some(stream) = outbound.get("streamSettings") {
        parse_xray_stream(stream, tag, &mut node, warnings);
    }

    Ok(node)
}

fn parse_xray_wireguard(settings: Option<&Value>, tag: &str) -> std::result::Result<Node, String> {
    let settings = settings.ok_or_else(|| format!("WireGuard 出站“{tag}”缺少 settings"))?;
    let peers = settings
        .get("peers")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("WireGuard 出站“{tag}”缺少 peers"))?;
    if peers.len() > 1 {
        return Err(format!(
            "WireGuard 出站“{tag}”包含多个对端，Xrose 仅支持单对端，请拆分后导入"
        ));
    }
    let peer = &peers[0];
    let endpoint = peer
        .get("endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("WireGuard 出站“{tag}”缺少 endpoint"))?;
    let (host, port) = split_host_port(endpoint)
        .ok_or_else(|| format!("WireGuard 出站“{tag}”的 endpoint 格式无效"))?;

    let defaults = WireguardOptions::default();
    let allowed_ips = json_string_list(peer.get("allowedIPs"));
    let reserved: [u8; 3] = match settings.get("reserved") {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|_| format!("WireGuard 出站“{tag}”的 reserved 必须为 3 个字节"))?,
        None => [0; 3],
    };
    let wg = WireguardOptions {
        private_key: settings
            .get("secretKey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        public_key: peer
            .get("publicKey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        pre_shared_key: peer
            .get("preSharedKey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        addresses: json_string_list(settings.get("address")),
        allowed_ips: if allowed_ips.is_empty() {
            defaults.allowed_ips
        } else {
            allowed_ips
        },
        mtu: json_u16(settings.get("mtu")).unwrap_or(defaults.mtu),
        reserved,
    };
    wg.validate().map_err(|e| e.to_string())?;
    let mut node = Node::new(tag, Protocol::Wireguard, host, port);
    node.credentials.wireguard = Some(wg);
    Ok(node)
}

fn parse_xray_stream(stream: &Value, tag: &str, node: &mut Node, warnings: &mut Vec<String>) {
    let network = stream
        .get("network")
        .and_then(Value::as_str)
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    node.transport = match network.as_str() {
        "tcp" | "raw" => Transport::Tcp,
        "ws" => {
            let ws = stream.get("wsSettings");
            let path = ws
                .and_then(|s| s.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("/")
                .to_string();
            let mut headers = BTreeMap::new();
            if let Some(map) = ws.and_then(|s| s.get("headers")).and_then(Value::as_object) {
                for (k, v) in map {
                    if let Some(v) = v.as_str() {
                        headers.insert(k.clone(), v.to_string());
                    }
                }
            }
            let host = ws
                .and_then(|s| s.get("host"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| headers.get("Host").cloned())
                .or_else(|| headers.get("host").cloned());
            Transport::WebSocket {
                path,
                host,
                headers,
            }
        }
        "grpc" => Transport::Grpc {
            service_name: stream
                .get("grpcSettings")
                .and_then(|s| s.get("serviceName"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "http" | "h2" => {
            let http = stream.get("httpSettings");
            let path = http
                .and_then(|s| s.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("/")
                .to_string();
            let host = http
                .and_then(|s| s.get("host"))
                .and_then(|h| {
                    h.as_str()
                        .or_else(|| h.as_array().and_then(|a| a.first()?.as_str()))
                })
                .map(str::to_string);
            Transport::Http2 { path, host }
        }
        "httpupgrade" => {
            let hu = stream.get("httpupgradeSettings");
            Transport::HttpUpgrade {
                path: hu
                    .and_then(|s| s.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or("/")
                    .to_string(),
                host: hu
                    .and_then(|s| s.get("host"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }
        }
        "quic" => Transport::Quic,
        "kcp" => Transport::Kcp {
            seed: stream
                .get("kcpSettings")
                .and_then(|s| s.get("seed"))
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        other => {
            warnings.push(format!(
                "出站“{tag}”的传输方式 {other} 暂不支持，已降级为 TCP"
            ));
            Transport::Tcp
        }
    };

    let security = stream
        .get("security")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_ascii_lowercase();
    match security.as_str() {
        "tls" => {
            node.tls.enabled = true;
            if let Some(tls) = stream.get("tlsSettings") {
                node.tls.server_name = tls
                    .get("serverName")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                node.tls.insecure = tls
                    .get("allowInsecure")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                node.tls.alpn = json_string_list(tls.get("alpn"));
                node.tls.fingerprint = tls
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
        }
        "reality" => {
            node.tls.enabled = true;
            if let Some(reality) = stream.get("realitySettings") {
                node.tls.server_name = reality
                    .get("serverName")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                node.tls.fingerprint = reality
                    .get("fingerprint")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                if let Some(public_key) = reality
                    .get("publicKey")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    node.tls.reality = Some(RealityOptions {
                        public_key: public_key.to_string(),
                        short_id: reality
                            .get("shortId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_xray_routing(
    routing: &Value,
    dns_root: Option<&Value>,
    direct_tags: &BTreeSet<String>,
    block_tags: &BTreeSet<String>,
    dns_tags: &BTreeSet<String>,
    balancer_tags: &BTreeMap<String, String>,
    node_tags: &BTreeMap<String, String>,
    dns_inbound_detours: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Option<ParsedRouting> {
    let mut rules = Vec::new();
    let mut final_action = None;
    let mut skipped_rules = 0usize;
    let mut fallback_targets = 0usize;

    if let Some(items) = routing.get("rules").and_then(Value::as_array) {
        let total = items.len();
        for (index, item) in items.iter().enumerate() {
            let outbound = item.get("outboundTag").and_then(Value::as_str);
            let balancer = item.get("balancerTag").and_then(Value::as_str);
            if outbound == Some("api") {
                continue;
            }
            if let Some(tag) = outbound
                && dns_tags.contains(tag)
            {
                continue;
            }
            let inbounds = json_string_list(item.get("inboundTag"));
            if inbounds.len() == 1
                && (inbounds[0] == "api"
                    || inbounds[0] == "api-in"
                    || dns_inbound_detours.contains_key(&inbounds[0]))
                && item.get("domain").is_none()
                && item.get("ip").is_none()
            {
                continue;
            }

            let action = if let Some(btag) = balancer {
                if let Some(group_id) = balancer_tags.get(btag) {
                    RouteAction::proxy_via(group_id)
                } else {
                    fallback_targets += 1;
                    RouteAction::proxy()
                }
            } else if let Some(otag) = outbound {
                if direct_tags.contains(otag) {
                    RouteAction::direct()
                } else if block_tags.contains(otag) {
                    RouteAction::block()
                } else if node_tags.contains_key(otag) {
                    fallback_targets += 1;
                    RouteAction::proxy()
                } else {
                    fallback_targets += 1;
                    RouteAction::proxy()
                }
            } else {
                skipped_rules += 1;
                continue;
            };

            let mut matchers = Matchers::default();
            for entry in json_string_list(item.get("domain")) {
                if let Some(rest) = entry.strip_prefix("full:") {
                    matchers.domains.push(rest.to_string());
                } else if let Some(rest) = entry.strip_prefix("domain:") {
                    matchers
                        .domain_suffixes
                        .push(rest.trim_start_matches('.').to_string());
                } else if let Some(rest) = entry.strip_prefix("keyword:") {
                    matchers.domain_keywords.push(rest.to_string());
                } else if let Some(rest) = entry.strip_prefix("regexp:") {
                    matchers.domain_regexes.push(rest.to_string());
                } else if let Some(rest) = entry.strip_prefix("geosite:") {
                    if safe_tag(rest) {
                        matchers.geosites.push(rest.to_string());
                    }
                } else {
                    matchers.domain_keywords.push(entry);
                }
            }
            for entry in json_string_list(item.get("ip")) {
                if let Some(rest) = entry.strip_prefix("geoip:") {
                    if safe_tag(rest) {
                        matchers.geoips.push(rest.to_string());
                    }
                } else if let Some(cidr) = normalize_ip_cidr(&entry) {
                    matchers.ip_cidrs.push(cidr);
                }
            }
            for entry in json_string_list(item.get("sourceIP").or_else(|| item.get("source"))) {
                if let Some(cidr) = normalize_ip_cidr(&entry) {
                    matchers.source_ip_cidrs.push(cidr);
                }
            }
            matchers.ports = split_port_field(item.get("port"));
            matchers.source_ports = split_port_field(item.get("sourcePort"));
            if let Some(net) = item.get("network").and_then(Value::as_str) {
                let lower = net.to_ascii_lowercase();
                let has_tcp = lower.contains("tcp");
                let has_udp = lower.contains("udp");
                matchers.network = match (has_tcp, has_udp) {
                    (true, true) => Some(Network::TcpUdp),
                    (true, false) => Some(Network::Tcp),
                    (false, true) => Some(Network::Udp),
                    (false, false) => None,
                };
            }
            matchers.process_names = json_string_list(item.get("process"));
            matchers.inbound_tags = inbounds;
            matchers.protocols = json_string_list(item.get("protocol"));

            if matchers.is_empty() {
                skipped_rules += 1;
                continue;
            }

            // Treat a trailing catch-all rule (`network: tcp,udp` or `port: 0-65535`) as `final_action`.
            if index + 1 == total && is_xray_catch_all(&matchers) {
                final_action = Some(action);
                continue;
            }

            let summary = matchers.summary();
            let name = if summary.chars().count() > 42 {
                format!("Xray 规则 #{}", index + 1)
            } else {
                summary
            };
            rules.push(RouteRule::new(name, matchers, action));
        }
    }
    if skipped_rules > 0 {
        warnings.push(format!("已跳过 {skipped_rules} 条无法映射的 Xray 路由规则"));
    }
    if fallback_targets > 0 {
        warnings.push(format!(
            "{fallback_targets} 条 Xray 路由规则指向单个节点或未知目标，已映射为默认代理组"
        ));
    }

    let domain_strategy = routing
        .get("domainStrategy")
        .and_then(Value::as_str)
        .and_then(|s| match s {
            "IPIfNonMatch" => Some(DomainStrategy::IpIfNonMatch),
            "IPOnDemand" => Some(DomainStrategy::IpOnDemand),
            "AsIs" => Some(DomainStrategy::AsIs),
            _ => None,
        })
        .or_else(|| {
            dns_root
                .and_then(|d| d.get("queryStrategy"))
                .and_then(Value::as_str)
                .and_then(|s| match s {
                    "UseIPv4" => Some(DomainStrategy::PreferIpv4),
                    "UseIPv6" => Some(DomainStrategy::PreferIpv6),
                    _ => None,
                })
        });

    let rules = compact_rules_if_large(rules);
    let parsed = ParsedRouting {
        preset: RoutePreset::Custom,
        rules,
        rule_groups: Vec::new(),
        final_action,
        rule_sets: Vec::new(),
        domain_strategy,
        rule_set_source: None,
        custom_rule_set_template: None,
        preserve_structure: false,
    };
    (!parsed.is_empty()).then_some(parsed)
}

fn is_xray_catch_all(m: &Matchers) -> bool {
    let only_network = *m
        == Matchers {
            network: Some(Network::TcpUdp),
            ..Default::default()
        };
    let only_all_ports = *m
        == Matchers {
            ports: vec!["0-65535".into()],
            ..Default::default()
        };
    only_network || only_all_ports
}

fn parse_xray_dns(
    dns: &Value,
    dns_inbound_detours: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Option<DnsConfig> {
    let mut config = DnsConfig {
        enabled: true,
        fake_ip: false,
        fake_ip_range: crate::model::dns::default_fakeip_range(),
        fake_ip_filter: Vec::new(),
        servers: Vec::new(),
        hosts: BTreeMap::new(),
        final_server: None,
        independent_cache: true,
    };

    if let Some(hosts) = dns.get("hosts").and_then(Value::as_object) {
        for (domain, val) in hosts {
            let domain = domain.trim();
            if domain.is_empty() {
                continue;
            }
            let ips = json_string_list(Some(val));
            if !ips.is_empty() {
                config.hosts.insert(domain.to_string(), ips);
            }
        }
    }

    let mut used_tags: BTreeSet<String> = BTreeSet::new();
    let mut final_candidate: Option<String> = None;

    if let Some(servers) = dns.get("servers").and_then(Value::as_array) {
        for (index, item) in servers.iter().enumerate() {
            let (raw_addr, port, domains, expect_ips, raw_tag) = match item {
                Value::String(s) => (s.as_str(), None, Vec::new(), Vec::new(), None),
                Value::Object(obj) => {
                    let Some(addr) = obj.get("address").and_then(Value::as_str) else {
                        continue;
                    };
                    let port = json_u16(obj.get("port"));
                    let domains: Vec<String> = json_string_list(obj.get("domains"))
                        .into_iter()
                        .map(normalize_xray_dns_domain)
                        .collect();
                    let expect_ips = json_string_list(obj.get("expectIPs"));
                    let raw_tag = obj
                        .get("tag")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_string);
                    (addr, port, domains, expect_ips, raw_tag)
                }
                _ => continue,
            };

            let Some((server_type, mut address, parsed_detour)) = parse_dns_address(raw_addr)
            else {
                warnings.push(format!("已跳过暂不支持的 Xray DNS 上游：{raw_addr}"));
                continue;
            };
            if server_type == DnsServerType::FakeIp {
                config.fake_ip = true;
                continue;
            }
            if let Some(port) = port
                && matches!(server_type, DnsServerType::Udp | DnsServerType::Tcp)
                && address != "system"
                && port != 53
                && !address.contains(':')
            {
                address = format!("{address}:{port}");
            }

            let detour = raw_tag
                .as_deref()
                .and_then(|t| dns_inbound_detours.get(t).cloned())
                .or(parsed_detour);

            let base_tag = raw_tag.unwrap_or_else(|| {
                if address == "system" {
                    "local".to_string()
                } else {
                    format!("dns-{}", index + 1)
                }
            });
            let tag = unique_tag(&base_tag, &mut used_tags);
            if domains.is_empty() && final_candidate.is_none() {
                final_candidate = Some(tag.clone());
            }
            config.servers.push(DnsServer {
                tag,
                server_type,
                address,
                domains,
                detour,
                strategy: None,
                expect_ips,
                client_subnet: None,
            });
        }
    }

    if config.servers.is_empty() && config.hosts.is_empty() && !config.fake_ip {
        return None;
    }
    if config.servers.is_empty() {
        config.servers.push(DnsServer::udp("local", "system"));
    }
    config.final_server = final_candidate.or_else(|| {
        config
            .servers
            .iter()
            .find(|s| !s.is_fakeip())
            .map(|s| s.tag.clone())
    });
    Some(config)
}

fn normalize_xray_dns_domain(entry: String) -> String {
    if let Some(rest) = entry.strip_prefix("domain:") {
        format!("suffix:{}", rest.trim_start_matches('.'))
    } else if entry.starts_with("full:")
        || entry.starts_with("geosite:")
        || entry.starts_with("keyword:")
        || entry.starts_with("regexp:")
    {
        entry
    } else {
        format!("keyword:{entry}")
    }
}

fn unique_tag(base: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    for index in 2..=9999 {
        let candidate = format!("{base}-{index}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    crate::util::new_id(base)
}

fn split_host_port(endpoint: &str) -> Option<(String, u16)> {
    let endpoint = endpoint.trim();
    if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?.parse::<u16>().ok()?;
        return (!host.is_empty() && port > 0).then(|| (host.to_string(), port));
    }
    let (host, port_str) = endpoint.rsplit_once(':')?;
    let port = port_str.parse::<u16>().ok()?;
    (!host.is_empty() && port > 0).then(|| (host.to_string(), port))
}

fn split_port_field(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Number(n)) => vec![n.to_string()],
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Number(n) => Some(n.to_string()),
                Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn json_u16(value: Option<&Value>) -> Option<u16> {
    match value {
        Some(Value::Number(n)) => n.as_u64().and_then(|v| u16::try_from(v).ok()),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

fn json_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::trim))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}
