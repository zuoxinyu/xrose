//! sing-box JSON full configuration parser (1.12+ typed schema with legacy fallbacks).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::Result;
use crate::model::{
    DnsConfig, DnsServer, DnsServerType, DomainStrategy, GroupKind, Matchers, Network, NodeSource,
    OutboundGroup, RouteAction, RoutePreset, RouteRule, RuleSet, RuleSetFormat,
};
use crate::route::transfer::safe_tag;
use crate::subscription::parse::parse_singbox_outbound;

use super::{
    ConfigFormat, ParsedConfig, ParsedProfile, ParsedRouting, compact_rules_if_large,
    dedup_warnings, expand_group_members, normalize_ip_cidr, parse_dns_address,
    parse_duration_hours, parse_duration_secs, safe_rule_set_url,
};

pub(crate) fn parse(root: &Value) -> Result<ParsedConfig> {
    let mut warnings = Vec::new();
    let mut nodes = Vec::new();
    let mut node_tags: BTreeMap<String, String> = BTreeMap::new();
    let mut direct_tags: BTreeSet<String> = BTreeSet::from(["direct".to_string()]);
    let mut block_tags: BTreeSet<String> = BTreeSet::from(["block".to_string()]);
    let mut dns_tags: BTreeSet<String> = BTreeSet::from(["dns-out".to_string()]);

    let mut raw_groups: Vec<&Value> = Vec::new();
    let mut raw_group_members: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for key in ["outbounds", "endpoints"] {
        let Some(items) = root.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(kind) = item.get("type").and_then(Value::as_str) else {
                continue;
            };
            let tag = item
                .get("tag")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match kind {
                "direct" => {
                    if !tag.is_empty() {
                        direct_tags.insert(tag);
                    }
                }
                "block" => {
                    if !tag.is_empty() {
                        block_tags.insert(tag);
                    }
                }
                "dns" => {
                    if !tag.is_empty() {
                        dns_tags.insert(tag);
                    }
                }
                "selector" | "urltest" => {
                    if !tag.is_empty() {
                        let members = json_string_list(item.get("outbounds"));
                        raw_group_members.insert(tag, members);
                        raw_groups.push(item);
                    }
                }
                _ => match parse_singbox_outbound(item) {
                    Some(Ok(mut node)) => {
                        node.source = NodeSource::Import;
                        if !tag.is_empty() {
                            node_tags.entry(tag).or_insert_with(|| node.id.clone());
                        }
                        nodes.push(node);
                    }
                    Some(Err(reason)) => warnings.push(reason),
                    None => {}
                },
            }
        }
    }

    // Resolve selector / urltest outbound groups.
    let mut groups = Vec::new();
    let mut group_tag_to_id: BTreeMap<String, String> = BTreeMap::new();
    let mut flattened_any = false;
    for entry in raw_groups {
        let Some(tag) = entry.get("tag").and_then(Value::as_str) else {
            continue;
        };
        let kind = match entry.get("type").and_then(Value::as_str) {
            Some("urltest") => GroupKind::UrlTest,
            _ => GroupKind::Selector,
        };
        let mut expanded = Vec::new();
        let mut visited = BTreeSet::new();
        let mut seen_nodes = BTreeSet::new();
        expand_group_members(
            tag,
            &raw_group_members,
            &node_tags,
            &mut visited,
            &mut expanded,
            &mut seen_nodes,
            &mut flattened_any,
        );
        if expanded.is_empty() {
            continue;
        }
        let mut group = OutboundGroup::new(tag, kind);
        group.members = expanded;
        if let Some(default_tag) = entry.get("default").and_then(Value::as_str) {
            if let Some(node_id) = node_tags.get(default_tag)
                && group.members.contains(node_id)
            {
                group.selected = Some(node_id.clone());
            } else if raw_group_members.contains_key(default_tag) {
                let mut sub_expanded = Vec::new();
                let mut sub_visited = BTreeSet::new();
                let mut sub_seen = BTreeSet::new();
                let mut dummy = false;
                expand_group_members(
                    default_tag,
                    &raw_group_members,
                    &node_tags,
                    &mut sub_visited,
                    &mut sub_expanded,
                    &mut sub_seen,
                    &mut dummy,
                );
                if let Some(first) = sub_expanded.first()
                    && group.members.contains(first)
                {
                    group.selected = Some(first.clone());
                }
            }
        }
        if let Some(url) = entry
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            group.url = Some(url.to_string());
        }
        if let Some(interval) = entry
            .get("interval")
            .and_then(Value::as_str)
            .and_then(parse_duration_secs)
        {
            group.interval_secs = interval.max(30);
        }
        if let Some(tolerance) = entry.get("tolerance").and_then(Value::as_u64) {
            group.tolerance_ms = tolerance.min(u32::MAX as u64) as u32;
        }
        group.repair();
        group_tag_to_id.insert(tag.to_string(), group.id.clone());
        groups.push(group);
    }
    if flattened_any {
        warnings.push("已将嵌套出站组展开为平铺节点列表".into());
    }

    // Parse route (rule sets, rules, final).
    let routing = root.get("route").and_then(|route| {
        parse_singbox_route(
            route,
            root.get("dns"),
            &direct_tags,
            &block_tags,
            &dns_tags,
            &group_tag_to_id,
            &node_tags,
            &mut warnings,
        )
    });

    let default_group = routing
        .as_ref()
        .and_then(|r| r.final_action.as_ref())
        .and_then(|a| a.group.clone())
        .or_else(|| {
            groups
                .iter()
                .find(|g| g.kind == GroupKind::Selector)
                .map(|g| g.id.clone())
        })
        .or_else(|| groups.first().map(|g| g.id.clone()));

    let dns = root
        .get("dns")
        .and_then(|dns| parse_singbox_dns(dns, &direct_tags, &group_tag_to_id, &mut warnings));

    dedup_warnings(&mut warnings);

    let profile = ParsedProfile {
        name: "sing-box 配置".into(),
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
        format: ConfigFormat::SingBoxJson,
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

#[allow(clippy::too_many_arguments)]
fn parse_singbox_route(
    route: &Value,
    dns_root: Option<&Value>,
    direct_tags: &BTreeSet<String>,
    block_tags: &BTreeSet<String>,
    dns_tags: &BTreeSet<String>,
    group_tags: &BTreeMap<String, String>,
    node_tags: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Option<ParsedRouting> {
    let mut rule_sets = Vec::new();
    let mut defined_rule_sets: BTreeSet<String> = BTreeSet::new();
    let mut skipped_local_sets = 0usize;
    if let Some(sets) = route.get("rule_set").and_then(Value::as_array) {
        for set in sets {
            let kind = set.get("type").and_then(Value::as_str).unwrap_or_default();
            let tag = set.get("tag").and_then(Value::as_str).unwrap_or_default();
            if kind != "remote" {
                skipped_local_sets += 1;
                continue;
            }
            let url = set.get("url").and_then(Value::as_str).unwrap_or_default();
            if !safe_tag(tag) || !safe_rule_set_url(url) {
                warnings.push(format!("已跳过不安全或无效的规则集“{tag}”"));
                continue;
            }
            let format = match set.get("format").and_then(Value::as_str) {
                Some("source") => RuleSetFormat::Source,
                _ => RuleSetFormat::Binary,
            };
            let download_detour = set
                .get("download_detour")
                .and_then(Value::as_str)
                .map(|d| !direct_tags.contains(d))
                .unwrap_or(true);
            let update_interval_hours = set
                .get("update_interval")
                .and_then(Value::as_str)
                .and_then(parse_duration_hours)
                .unwrap_or(24);
            defined_rule_sets.insert(tag.to_string());
            rule_sets.push(RuleSet {
                tag: tag.to_string(),
                url: url.to_string(),
                format,
                download_detour,
                update_interval_hours,
            });
        }
    }
    if skipped_local_sets > 0 {
        warnings.push(format!(
            "出于安全考虑，已跳过 {skipped_local_sets} 个本地或内联 rule_set，引用它们的规则同步跳过"
        ));
    }

    let mut rules = Vec::new();
    let mut skipped_rules = 0usize;
    let mut fallback_targets = 0usize;
    if let Some(items) = route.get("rules").and_then(Value::as_array) {
        for (index, item) in items.iter().enumerate() {
            if item.get("type").and_then(Value::as_str) == Some("logical")
                || item.get("invert").and_then(Value::as_bool).unwrap_or(false)
            {
                skipped_rules += 1;
                continue;
            }
            let action_field = item.get("action").and_then(Value::as_str);
            if matches!(action_field, Some("sniff" | "hijack-dns" | "resolve")) {
                continue;
            }
            let outbound = item.get("outbound").and_then(Value::as_str);
            if let Some(tag) = outbound
                && dns_tags.contains(tag)
            {
                continue;
            }
            if item.get("clash_mode").is_some() {
                skipped_rules += 1;
                continue;
            }
            let action = if action_field == Some("reject") {
                RouteAction::block()
            } else if let Some(tag) = outbound {
                resolve_singbox_action(
                    tag,
                    direct_tags,
                    block_tags,
                    group_tags,
                    node_tags,
                    &mut fallback_targets,
                )
            } else {
                skipped_rules += 1;
                continue;
            };

            let mut matchers = Matchers::default();
            matchers.domains = json_string_list(item.get("domain"));
            matchers.domain_suffixes = json_string_list(item.get("domain_suffix"))
                .into_iter()
                .map(|s| s.trim_start_matches('.').to_string())
                .collect();
            matchers.domain_keywords = json_string_list(item.get("domain_keyword"));
            matchers.domain_regexes = json_string_list(item.get("domain_regex"));
            matchers.geosites = json_string_list(item.get("geosite"))
                .into_iter()
                .filter(|s| safe_tag(s))
                .collect();
            matchers.geoips = json_string_list(item.get("geoip"))
                .into_iter()
                .filter(|s| safe_tag(s))
                .collect();
            if item
                .get("ip_is_private")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !matchers.geoips.iter().any(|g| g == "private")
            {
                matchers.geoips.push("private".into());
            }
            matchers.ip_cidrs = json_string_list(item.get("ip_cidr"))
                .into_iter()
                .filter_map(|v| normalize_ip_cidr(&v))
                .collect();
            matchers.source_ip_cidrs = json_string_list(item.get("source_ip_cidr"))
                .into_iter()
                .filter_map(|v| normalize_ip_cidr(&v))
                .collect();
            matchers.ports = collect_singbox_ports(item.get("port"), item.get("port_range"));
            matchers.source_ports =
                collect_singbox_ports(item.get("source_port"), item.get("source_port_range"));
            let nets = json_string_list(item.get("network"));
            if !nets.is_empty() {
                let has_tcp = nets.iter().any(|n| n.eq_ignore_ascii_case("tcp"));
                let has_udp = nets.iter().any(|n| n.eq_ignore_ascii_case("udp"));
                matchers.network = match (has_tcp, has_udp) {
                    (true, true) => Some(Network::TcpUdp),
                    (true, false) => Some(Network::Tcp),
                    (false, true) => Some(Network::Udp),
                    (false, false) => None,
                };
            }
            matchers.process_names = json_string_list(item.get("process_name"));
            matchers.process_paths = json_string_list(item.get("process_path"));
            matchers.inbound_tags = json_string_list(item.get("inbound"));
            matchers.protocols = json_string_list(item.get("protocol"));

            let mut missing_set = false;
            for tag in json_string_list(item.get("rule_set")) {
                if !safe_tag(&tag) {
                    missing_set = true;
                    continue;
                }
                if defined_rule_sets.contains(&tag)
                    || crate::adapters::default_rule_set(&tag).is_some()
                {
                    matchers.rule_sets.push(tag);
                } else {
                    missing_set = true;
                }
            }
            if matchers.is_empty() {
                if missing_set {
                    skipped_rules += 1;
                }
                continue;
            }
            let summary = matchers.summary();
            let name = if summary.chars().count() > 42 {
                format!("规则 #{}", index + 1)
            } else {
                summary
            };
            rules.push(RouteRule::new(name, matchers, action));
        }
    }
    if skipped_rules > 0 {
        warnings.push(format!(
            "已跳过 {skipped_rules} 条逻辑组合、取反或含未定义规则集的 sing-box 路由规则"
        ));
    }
    if fallback_targets > 0 {
        warnings.push(format!(
            "{fallback_targets} 条路由规则指向单个节点或未知出站，已映射为默认代理组"
        ));
    }

    let final_action = route.get("final").and_then(Value::as_str).map(|tag| {
        resolve_singbox_action(
            tag,
            direct_tags,
            block_tags,
            group_tags,
            node_tags,
            &mut fallback_targets,
        )
    });

    let domain_strategy = dns_root
        .and_then(|d| d.get("strategy"))
        .and_then(Value::as_str)
        .and_then(|s| match s {
            "prefer_ipv4" | "ipv4_only" => Some(DomainStrategy::PreferIpv4),
            "prefer_ipv6" | "ipv6_only" => Some(DomainStrategy::PreferIpv6),
            _ => None,
        });

    let rules = compact_rules_if_large(rules);
    let routing = ParsedRouting {
        preset: RoutePreset::Custom,
        rules,
        rule_groups: Vec::new(),
        final_action,
        rule_sets,
        domain_strategy,
        rule_set_source: None,
        custom_rule_set_template: None,
        preserve_structure: false,
    };
    (!routing.is_empty()).then_some(routing)
}

fn resolve_singbox_action(
    tag: &str,
    direct_tags: &BTreeSet<String>,
    block_tags: &BTreeSet<String>,
    group_tags: &BTreeMap<String, String>,
    node_tags: &BTreeMap<String, String>,
    fallback_targets: &mut usize,
) -> RouteAction {
    if direct_tags.contains(tag) {
        return RouteAction::direct();
    }
    if block_tags.contains(tag) {
        return RouteAction::block();
    }
    if let Some(group_id) = group_tags.get(tag) {
        return RouteAction::proxy_via(group_id);
    }
    if node_tags.contains_key(tag) {
        *fallback_targets += 1;
        return RouteAction::proxy();
    }
    *fallback_targets += 1;
    RouteAction::proxy()
}

fn parse_singbox_dns(
    dns: &Value,
    direct_tags: &BTreeSet<String>,
    group_tags: &BTreeMap<String, String>,
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
        independent_cache: dns
            .get("independent_cache")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    };

    if let Some(fakeip) = dns.get("fakeip") {
        if fakeip
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            config.fake_ip = true;
            if let Some(range) = fakeip.get("inet4_range").and_then(Value::as_str)
                && let Some(normalized) = normalize_ip_cidr(range)
            {
                config.fake_ip_range = normalized;
            }
        }
    }

    let mut fakeip_tags: BTreeSet<String> = BTreeSet::new();
    if let Some(servers) = dns.get("servers").and_then(Value::as_array) {
        for (index, item) in servers.iter().enumerate() {
            let tag = item
                .get("tag")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("dns-{}", index + 1));

            let detour = item.get("detour").and_then(Value::as_str).map(|d| {
                if direct_tags.contains(d) {
                    "direct".to_string()
                } else {
                    group_tags
                        .get(d)
                        .cloned()
                        .unwrap_or_else(|| "proxy".to_string())
                }
            });

            if let Some(kind) = item.get("type").and_then(Value::as_str) {
                match kind {
                    "local" => {
                        config.servers.push(DnsServer {
                            tag,
                            server_type: DnsServerType::Udp,
                            address: "system".into(),
                            domains: Vec::new(),
                            detour: Some("direct".into()),
                            strategy: None,
                            expect_ips: Vec::new(),
                            client_subnet: None,
                        });
                    }
                    "fakeip" => {
                        config.fake_ip = true;
                        fakeip_tags.insert(tag);
                        if let Some(range) = item.get("inet4_range").and_then(Value::as_str)
                            && let Some(normalized) = normalize_ip_cidr(range)
                        {
                            config.fake_ip_range = normalized;
                        }
                    }
                    "hosts" => {
                        if let Some(predefined) = item.get("predefined").and_then(Value::as_object)
                        {
                            for (domain, val) in predefined {
                                let ips = json_string_list(Some(val));
                                if !domain.trim().is_empty() && !ips.is_empty() {
                                    config.hosts.insert(domain.trim().to_string(), ips);
                                }
                            }
                        }
                    }
                    "udp" | "tcp" | "tls" | "https" | "quic" | "h3" => {
                        let Some(host) = item
                            .get("server")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                        else {
                            warnings.push(format!("DNS 服务器“{tag}”缺少 server 地址，已跳过"));
                            continue;
                        };
                        let port = item
                            .get("server_port")
                            .and_then(Value::as_u64)
                            .and_then(|p| u16::try_from(p).ok());
                        let server_type = DnsServerType::parse(kind).unwrap_or(DnsServerType::Udp);
                        let address = format_singbox_dns_address(
                            server_type,
                            host,
                            port,
                            item.get("path").and_then(Value::as_str),
                        );
                        config.servers.push(DnsServer {
                            tag,
                            server_type,
                            address,
                            domains: Vec::new(),
                            detour,
                            strategy: None,
                            expect_ips: Vec::new(),
                            client_subnet: item
                                .get("client_subnet")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        });
                    }
                    other => {
                        warnings.push(format!("已跳过暂不支持的 sing-box DNS 类型：{other}"));
                    }
                }
            } else if let Some(addr_raw) = item.get("address").and_then(Value::as_str) {
                match parse_dns_address(addr_raw) {
                    Some((DnsServerType::FakeIp, _, _)) => {
                        config.fake_ip = true;
                        fakeip_tags.insert(tag);
                    }
                    Some((server_type, address, parsed_detour)) => {
                        config.servers.push(DnsServer {
                            tag,
                            server_type,
                            address,
                            domains: Vec::new(),
                            detour: detour.or(parsed_detour),
                            strategy: None,
                            expect_ips: Vec::new(),
                            client_subnet: item
                                .get("client_subnet")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        });
                    }
                    None => {
                        warnings.push(format!("已跳过暂不支持的 sing-box DNS 地址：{addr_raw}"));
                    }
                }
            }
        }
    }

    // Map sing-box dns.rules domain conditions onto matching servers.
    if let Some(rules) = dns.get("rules").and_then(Value::as_array) {
        let mut skipped_dns_rules = 0usize;
        for rule in rules {
            let Some(server_tag) = rule.get("server").and_then(Value::as_str) else {
                continue;
            };
            if fakeip_tags.contains(server_tag) {
                continue;
            }
            let Some(server) = config.servers.iter_mut().find(|s| s.tag == server_tag) else {
                continue;
            };
            let mut added = false;
            for d in json_string_list(rule.get("domain")) {
                let entry = format!("full:{d}");
                if !server.domains.contains(&entry) {
                    server.domains.push(entry);
                }
                added = true;
            }
            for d in json_string_list(rule.get("domain_suffix")) {
                let entry = format!("suffix:{}", d.trim_start_matches('.'));
                if !server.domains.contains(&entry) {
                    server.domains.push(entry);
                }
                added = true;
            }
            for d in json_string_list(rule.get("domain_keyword")) {
                let entry = format!("keyword:{d}");
                if !server.domains.contains(&entry) {
                    server.domains.push(entry);
                }
                added = true;
            }
            for d in json_string_list(rule.get("domain_regex")) {
                let entry = format!("regexp:{d}");
                if !server.domains.contains(&entry) {
                    server.domains.push(entry);
                }
                added = true;
            }
            for g in json_string_list(rule.get("geosite")) {
                if safe_tag(&g) {
                    let entry = format!("geosite:{g}");
                    if !server.domains.contains(&entry) {
                        server.domains.push(entry);
                    }
                    added = true;
                }
            }
            for rs in json_string_list(rule.get("rule_set")) {
                if safe_tag(&rs) {
                    let entry = if let Some(gs) = rs.strip_prefix("geosite-") {
                        format!("geosite:{gs}")
                    } else {
                        format!("rule_set:{rs}")
                    };
                    if !server.domains.contains(&entry) {
                        server.domains.push(entry);
                    }
                    added = true;
                }
            }
            if !added && rule.get("outbound").is_none() {
                skipped_dns_rules += 1;
            }
        }
        if skipped_dns_rules > 0 {
            warnings.push(format!(
                "已跳过 {skipped_dns_rules} 条无域名匹配条件的 sing-box DNS 规则"
            ));
        }
    }

    if config.servers.is_empty() && config.hosts.is_empty() && !config.fake_ip {
        return None;
    }
    if config.servers.is_empty() {
        config.servers.push(DnsServer::udp("local", "system"));
    }

    let final_tag = dns
        .get("final")
        .and_then(Value::as_str)
        .filter(|tag| {
            config
                .servers
                .iter()
                .any(|s| s.tag == *tag && !s.is_fakeip())
        })
        .map(str::to_string)
        .or_else(|| {
            config
                .servers
                .iter()
                .find(|s| !s.is_fakeip() && s.domains.is_empty())
                .or_else(|| config.servers.iter().find(|s| !s.is_fakeip()))
                .map(|s| s.tag.clone())
        });
    config.final_server = final_tag;
    Some(config)
}

fn format_singbox_dns_address(
    kind: DnsServerType,
    host: &str,
    port: Option<u16>,
    path: Option<&str>,
) -> String {
    let formatted_host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    match kind {
        DnsServerType::Https => {
            let port_part = port
                .filter(|&p| p != 443)
                .map(|p| format!(":{p}"))
                .unwrap_or_default();
            let path = path
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or("/dns-query");
            let path = if path.starts_with('/') {
                path.to_string()
            } else {
                format!("/{path}")
            };
            format!("https://{formatted_host}{port_part}{path}")
        }
        DnsServerType::Tls | DnsServerType::Quic | DnsServerType::H3 => match port {
            Some(p) if p != 853 => format!("{formatted_host}:{p}"),
            _ => formatted_host,
        },
        DnsServerType::Udp | DnsServerType::Tcp => match port {
            Some(p) if p != 53 => format!("{formatted_host}:{p}"),
            _ => formatted_host,
        },
        DnsServerType::FakeIp => "fakeip".into(),
    }
}

fn json_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| match v {
                Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::Number(n)) => vec![n.to_string()],
        _ => Vec::new(),
    }
}

fn collect_singbox_ports(port: Option<&Value>, range: Option<&Value>) -> Vec<String> {
    let mut out = json_string_list(port);
    for item in json_string_list(range) {
        out.push(item.replace(':', "-"));
    }
    out
}
