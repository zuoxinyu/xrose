//! Clash / Mihomo YAML full configuration parser.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde_yaml::Value;

use crate::error::Result;
use crate::model::{
    DnsConfig, DnsServer, DnsServerType, GroupKind, Matchers, Network, NodeSource, OutboundGroup,
    RouteAction, RoutePreset, RouteRule, RuleSet, RuleSetFormat,
};
use crate::route::transfer::safe_tag;
use crate::subscription::{Subscription, parse::parse_clash_proxy};

use super::{
    ConfigFormat, ParsedConfig, ParsedProfile, ParsedRouting, compact_rules_if_large,
    dedup_warnings, expand_group_members, normalize_ip_cidr, parse_dns_address,
    parse_duration_hours, parse_duration_secs, safe_http_subscription_url, safe_rule_set_url,
};

pub(crate) fn parse(root: &Value) -> Result<ParsedConfig> {
    let mut warnings = Vec::new();
    let mut nodes = Vec::new();
    let mut node_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut node_names_in_order: Vec<(String, String)> = Vec::new();

    if let Some(proxies) = root.get("proxies").and_then(Value::as_sequence) {
        for entry in proxies {
            match parse_clash_proxy(entry) {
                Ok(mut node) => {
                    node.source = NodeSource::Import;
                    node_ids
                        .entry(node.name.clone())
                        .or_insert_with(|| node.id.clone());
                    node_names_in_order.push((node.name.clone(), node.id.clone()));
                    nodes.push(node);
                }
                Err(reason) => warnings.push(reason),
            }
        }
    }

    // Parse proxy-providers (HTTP subscriptions).
    let mut subscriptions = Vec::new();
    let mut skipped_local_providers = 0usize;
    if let Some(providers) = root.get("proxy-providers").and_then(Value::as_mapping) {
        for (key, value) in providers {
            let Some(name) = key.as_str() else {
                continue;
            };
            let kind = yaml_str(value, "type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            if kind == "file" {
                skipped_local_providers += 1;
                continue;
            }
            if let Some(url) = yaml_str(value, "url") {
                if !safe_http_subscription_url(&url) {
                    warnings.push(format!(
                        "已跳过无效 proxy-provider“{name}”：地址不是 HTTP(S)"
                    ));
                    continue;
                }
                let mut sub = Subscription::new(name, url);
                if let Some(interval) = yaml_u64(value, "interval") {
                    if interval > 0 {
                        let hours = Subscription::clamp_interval_hours((interval / 3600).max(1));
                        sub.custom_update_interval_secs = Some(hours * 3600);
                    }
                }
                if let Some(headers) = value.get("header").and_then(Value::as_mapping) {
                    for (hk, hv) in headers {
                        let Some(hk) = hk.as_str() else {
                            continue;
                        };
                        let val = hv.as_str().map(str::to_string).or_else(|| {
                            hv.as_sequence()
                                .and_then(|seq| seq.first()?.as_str().map(str::to_string))
                        });
                        if let Some(val) = val {
                            if hk.eq_ignore_ascii_case("user-agent") {
                                sub.user_agent = val;
                            } else {
                                sub.headers.insert(hk.to_string(), val);
                            }
                        }
                    }
                }
                subscriptions.push(sub);
            }
        }
    }
    if skipped_local_providers > 0 {
        warnings.push(format!(
            "出于安全考虑，已跳过 {skipped_local_providers} 个本地文件 proxy-provider"
        ));
    }

    // Parse proxy-groups with nested-group flattening and filter support.
    let mut raw_group_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut raw_groups: Vec<&Value> = Vec::new();
    if let Some(groups) = root.get("proxy-groups").and_then(Value::as_sequence) {
        for group in groups {
            let Some(name) = yaml_str(group, "name").filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            let mut members = Vec::new();
            if let Some(list) = group.get("proxies").and_then(Value::as_sequence) {
                for item in list {
                    if let Some(member) = item.as_str() {
                        if !is_clash_builtin(member) {
                            members.push(member.to_string());
                        }
                    }
                }
            }
            if yaml_bool(group, "include-all").unwrap_or(false)
                || yaml_bool(group, "include-all-proxies").unwrap_or(false)
            {
                let filter = compile_optional_regex(
                    yaml_str(group, "filter").as_deref(),
                    &name,
                    "filter",
                    &mut warnings,
                );
                let exclude = compile_optional_regex(
                    yaml_str(group, "exclude-filter").as_deref(),
                    &name,
                    "exclude-filter",
                    &mut warnings,
                );
                for (node_name, _) in &node_names_in_order {
                    let included = filter.as_ref().is_none_or(|re| re.is_match(node_name));
                    let excluded = exclude.as_ref().is_some_and(|re| re.is_match(node_name));
                    if included && !excluded && !members.contains(node_name) {
                        members.push(node_name.clone());
                    }
                }
            }
            raw_group_members.insert(name, members);
            raw_groups.push(group);
        }
    }

    let mut groups = Vec::new();
    let mut group_name_to_id: BTreeMap<String, String> = BTreeMap::new();
    let mut flattened_any = false;
    let mut provider_backed_groups = 0usize;
    for entry in raw_groups {
        let Some(name) = yaml_str(entry, "name").filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let raw_type = yaml_str(entry, "type").unwrap_or_else(|| "select".into());
        let kind = match GroupKind::parse(&raw_type) {
            Some(kind) => kind,
            None => {
                warnings.push(format!(
                    "策略组“{name}”的类型 {raw_type} 暂不支持，已降级为手动选择组"
                ));
                GroupKind::Selector
            }
        };
        let mut expanded = Vec::new();
        let mut visited = BTreeSet::new();
        let mut seen_nodes = BTreeSet::new();
        expand_group_members(
            &name,
            &raw_group_members,
            &node_ids,
            &mut visited,
            &mut expanded,
            &mut seen_nodes,
            &mut flattened_any,
        );
        if expanded.is_empty() {
            if entry
                .get("use")
                .and_then(Value::as_sequence)
                .is_some_and(|u| !u.is_empty())
            {
                provider_backed_groups += 1;
            }
            continue;
        }
        let mut group = OutboundGroup::new(name.clone(), kind);
        group.members = expanded;
        if let Some(url) = yaml_str(entry, "url").filter(|u| !u.trim().is_empty()) {
            group.url = Some(url);
        }
        if let Some(interval) = yaml_u64(entry, "interval") {
            group.interval_secs = interval.max(30);
        }
        if let Some(tolerance) = yaml_u64(entry, "tolerance") {
            group.tolerance_ms = tolerance.min(u32::MAX as u64) as u32;
        }
        group.repair();
        group_name_to_id.insert(name, group.id.clone());
        groups.push(group);
    }
    if flattened_any {
        warnings.push("已将嵌套策略组展开为平铺节点列表".into());
    }
    if provider_backed_groups > 0 {
        warnings.push(format!(
            "{provider_backed_groups} 个仅引用远程 proxy-provider 的策略组在订阅拉取前无本地成员，已跳过"
        ));
    }

    // Parse rule-providers (only sing-box compatible binary/source URLs are imported).
    let mut rule_sets = Vec::new();
    let mut known_rule_set_tags: BTreeSet<String> = BTreeSet::new();
    let mut skipped_clash_rule_providers = 0usize;
    if let Some(providers) = root.get("rule-providers").and_then(Value::as_mapping) {
        for (key, value) in providers {
            let Some(tag) = key.as_str() else {
                continue;
            };
            let kind = yaml_str(value, "type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let format_str = yaml_str(value, "format")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let url = yaml_str(value, "url").unwrap_or_default();
            let compatible_format = if format_str == "binary" || url.ends_with(".srs") {
                Some(RuleSetFormat::Binary)
            } else if format_str == "source" || url.ends_with(".json") {
                Some(RuleSetFormat::Source)
            } else {
                None
            };
            if kind == "http"
                && safe_tag(tag)
                && safe_rule_set_url(&url)
                && let Some(format) = compatible_format
            {
                let interval = yaml_u64(value, "interval")
                    .map(|secs| (secs / 3600).max(1))
                    .or_else(|| yaml_str(value, "interval").and_then(|s| parse_duration_hours(&s)))
                    .unwrap_or(24);
                known_rule_set_tags.insert(tag.to_string());
                rule_sets.push(RuleSet {
                    tag: tag.to_string(),
                    url,
                    format,
                    download_detour: true,
                    update_interval_hours: interval,
                });
            } else {
                skipped_clash_rule_providers += 1;
            }
        }
    }
    if skipped_clash_rule_providers > 0 {
        warnings.push(format!(
            "已跳过 {skipped_clash_rule_providers} 个非 sing-box 格式或本地路径的 rule-provider"
        ));
    }

    // Parse routing rules.
    let mut rules = Vec::new();
    let mut final_action = None;
    let mut skipped_rules = 0usize;
    let mut fallback_target_count = 0usize;
    if let Some(raw_rules) = root.get("rules").and_then(Value::as_sequence) {
        for item in raw_rules {
            let Some(line) = item.as_str() else {
                skipped_rules += 1;
                continue;
            };
            match parse_clash_rule(
                line,
                &group_name_to_id,
                &known_rule_set_tags,
                &mut fallback_target_count,
            ) {
                ClashRuleOutcome::Rule(rule) => rules.push(rule),
                ClashRuleOutcome::Final(action) => final_action = Some(action),
                ClashRuleOutcome::Skipped => skipped_rules += 1,
            }
        }
    }
    if skipped_rules > 0 {
        warnings.push(format!(
            "已跳过 {skipped_rules} 条 Xrose 暂不支持或引用未导入规则集的 Clash 规则"
        ));
    }
    if fallback_target_count > 0 {
        warnings.push(format!(
            "{fallback_target_count} 条规则指向未导入的目标出站，已映射为默认代理组"
        ));
    }

    let rules = compact_rules_if_large(rules);
    let routing = if !rules.is_empty() || !rule_sets.is_empty() || final_action.is_some() {
        Some(ParsedRouting {
            preset: RoutePreset::Custom,
            rules,
            rule_groups: Vec::new(),
            final_action,
            rule_sets,
            domain_strategy: None,
            rule_set_source: None,
            custom_rule_set_template: None,
            preserve_structure: false,
        })
    } else {
        None
    };

    // Determine suggested default outbound group.
    let default_group = routing
        .as_ref()
        .and_then(|r| r.final_action.as_ref())
        .and_then(|a| a.group.clone())
        .or_else(|| {
            groups
                .iter()
                .find(|g| {
                    g.kind == GroupKind::Selector
                        && matches!(
                            g.name.to_ascii_lowercase().as_str(),
                            "proxy" | "select" | "节点选择" | "代理"
                        )
                })
                .map(|g| g.id.clone())
        })
        .or_else(|| {
            groups
                .iter()
                .find(|g| g.kind == GroupKind::Selector)
                .map(|g| g.id.clone())
        })
        .or_else(|| groups.first().map(|g| g.id.clone()));

    // Parse DNS + top-level hosts.
    let dns = parse_clash_dns(root, &group_name_to_id, &mut warnings);

    dedup_warnings(&mut warnings);

    let profile = ParsedProfile {
        name: "Clash 配置".into(),
        work_mode: None,
        nodes,
        groups,
        default_group,
        routing,
        dns,
        warnings: Vec::new(),
    };
    let has_profile_data = profile.counts(0).available_sections().iter().any(|_| true);

    Ok(ParsedConfig {
        format: ConfigFormat::ClashYaml,
        exported_at: None,
        profiles: if has_profile_data {
            vec![profile]
        } else {
            Vec::new()
        },
        subscriptions,
        warnings,
    })
}

enum ClashRuleOutcome {
    Rule(RouteRule),
    Final(RouteAction),
    Skipped,
}

fn parse_clash_rule(
    line: &str,
    group_ids: &BTreeMap<String, String>,
    rule_sets: &BTreeSet<String>,
    fallback_targets: &mut usize,
) -> ClashRuleOutcome {
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.is_empty() {
        return ClashRuleOutcome::Skipped;
    }
    let kind = parts[0].to_ascii_uppercase();
    if matches!(kind.as_str(), "MATCH" | "FINAL") {
        let Some(target) = parts.get(1) else {
            return ClashRuleOutcome::Skipped;
        };
        let action = resolve_clash_action(target, group_ids, fallback_targets);
        return ClashRuleOutcome::Final(action);
    }
    if parts.len() < 3 {
        return ClashRuleOutcome::Skipped;
    }
    let payload = parts[1];
    let target = parts[2];
    if payload.is_empty() || target.is_empty() {
        return ClashRuleOutcome::Skipped;
    }

    let mut matchers = Matchers::default();
    match kind.as_str() {
        "DOMAIN" => matchers.domains.push(payload.to_string()),
        "DOMAIN-SUFFIX" => matchers
            .domain_suffixes
            .push(payload.trim_start_matches('.').to_string()),
        "DOMAIN-KEYWORD" => matchers.domain_keywords.push(payload.to_string()),
        "DOMAIN-REGEX" => matchers.domain_regexes.push(payload.to_string()),
        "GEOSITE" => {
            let tag = payload.to_ascii_lowercase();
            if !safe_tag(&tag) {
                return ClashRuleOutcome::Skipped;
            }
            matchers.geosites.push(tag);
        }
        "GEOIP" => {
            let tag = match payload.to_ascii_lowercase().as_str() {
                "lan" => "private".to_string(),
                other => other.to_string(),
            };
            if !safe_tag(&tag) {
                return ClashRuleOutcome::Skipped;
            }
            matchers.geoips.push(tag);
        }
        "IP-CIDR" | "IP-CIDR6" => {
            let Some(cidr) = normalize_ip_cidr(payload) else {
                return ClashRuleOutcome::Skipped;
            };
            matchers.ip_cidrs.push(cidr);
        }
        "SRC-IP-CIDR" => {
            let Some(cidr) = normalize_ip_cidr(payload) else {
                return ClashRuleOutcome::Skipped;
            };
            matchers.source_ip_cidrs.push(cidr);
        }
        "DST-PORT" => matchers.ports.push(payload.replace('/', ",")),
        "SRC-PORT" => matchers.source_ports.push(payload.replace('/', ",")),
        "NETWORK" => match payload.to_ascii_lowercase().as_str() {
            "tcp" => matchers.network = Some(Network::Tcp),
            "udp" => matchers.network = Some(Network::Udp),
            _ => return ClashRuleOutcome::Skipped,
        },
        "PROCESS-NAME" => matchers.process_names.push(payload.to_string()),
        "PROCESS-PATH" => matchers.process_paths.push(payload.to_string()),
        "RULE-SET" => {
            if !safe_tag(payload)
                || (!rule_sets.contains(payload)
                    && crate::adapters::default_rule_set(payload).is_none())
            {
                return ClashRuleOutcome::Skipped;
            }
            matchers.rule_sets.push(payload.to_string());
        }
        _ => return ClashRuleOutcome::Skipped,
    }

    let action = resolve_clash_action(target, group_ids, fallback_targets);
    let name = format!("{kind} {payload}");
    ClashRuleOutcome::Rule(RouteRule::new(name, matchers, action))
}

fn resolve_clash_action(
    target: &str,
    group_ids: &BTreeMap<String, String>,
    fallback_targets: &mut usize,
) -> RouteAction {
    match target.to_ascii_uppercase().as_str() {
        "DIRECT" => RouteAction::direct(),
        "REJECT" | "REJECT-DROP" => RouteAction::block(),
        "PROXY" | "COMPATIBLE" => RouteAction::proxy(),
        _ => {
            if let Some(group_id) = group_ids.get(target) {
                RouteAction::proxy_via(group_id)
            } else {
                *fallback_targets += 1;
                RouteAction::proxy()
            }
        }
    }
}

fn parse_clash_dns(
    root: &Value,
    group_ids: &BTreeMap<String, String>,
    warnings: &mut Vec<String>,
) -> Option<DnsConfig> {
    let dns_val = root.get("dns").filter(|v| v.is_mapping());
    let hosts_val = root.get("hosts").filter(|v| v.is_mapping());
    if dns_val.is_none() && hosts_val.is_none() {
        return None;
    }

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

    if let Some(hosts) = hosts_val.and_then(Value::as_mapping) {
        for (domain, val) in hosts {
            let Some(domain) = domain.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let ips: Vec<String> = match val {
                Value::String(s) => vec![s.trim().to_string()],
                Value::Sequence(seq) => seq
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
                _ => Vec::new(),
            };
            if !ips.is_empty() {
                config.hosts.insert(domain.to_string(), ips);
            }
        }
    }

    if let Some(dns) = dns_val {
        if let Some(enabled) = yaml_bool(dns, "enable") {
            config.enabled = enabled;
        }
        if yaml_str(dns, "enhanced-mode").is_some_and(|m| m.eq_ignore_ascii_case("fake-ip")) {
            config.fake_ip = true;
            if let Some(range) = yaml_str(dns, "fake-ip-range") {
                if let Some(normalized) = normalize_ip_cidr(&range) {
                    config.fake_ip_range = normalized;
                }
            }
            config.fake_ip_filter = yaml_string_list(dns, "fake-ip-filter");
        }

        let mut used_tags: BTreeSet<String> = BTreeSet::new();
        let mut add_upstream =
            |prefix: &str, raw: &str, default_detour: Option<&str>, domains: Vec<String>| {
                let Some((server_type, address, parsed_detour)) = parse_dns_address(raw) else {
                    warnings.push(format!("已跳过暂不支持的 Clash DNS 上游：{raw}"));
                    return;
                };
                if server_type == DnsServerType::FakeIp {
                    config.fake_ip = true;
                    return;
                }
                let detour = parsed_detour
                    .map(|d| map_clash_detour(&d, group_ids))
                    .or_else(|| default_detour.map(str::to_string));

                if let Some(existing) = config.servers.iter_mut().find(|s| {
                    s.server_type == server_type && s.address == address && s.detour == detour
                }) {
                    for d in domains {
                        if !existing.domains.contains(&d) {
                            existing.domains.push(d);
                        }
                    }
                    return;
                }

                let tag = unique_dns_tag(prefix, &mut used_tags);
                config.servers.push(DnsServer {
                    tag,
                    server_type,
                    address,
                    domains,
                    detour,
                    strategy: None,
                    expect_ips: Vec::new(),
                    client_subnet: None,
                });
            };

        for raw in yaml_string_list(dns, "nameserver") {
            add_upstream("ns", &raw, None, Vec::new());
        }
        for raw in yaml_string_list(dns, "default-nameserver") {
            add_upstream("local", &raw, Some("direct"), Vec::new());
        }
        for raw in yaml_string_list(dns, "fallback") {
            add_upstream("fallback", &raw, Some("proxy"), Vec::new());
        }
        if let Some(policy) = dns.get("nameserver-policy").and_then(Value::as_mapping) {
            for (k, v) in policy {
                let Some(pattern_raw) = k.as_str() else {
                    continue;
                };
                let domains = parse_clash_dns_policy_domains(pattern_raw);
                if domains.is_empty() {
                    continue;
                }
                let servers: Vec<String> = match v {
                    Value::String(s) => vec![s.clone()],
                    Value::Sequence(seq) => seq
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect(),
                    _ => Vec::new(),
                };
                for raw in servers {
                    add_upstream("policy", &raw, None, domains.clone());
                }
            }
        }
    }

    if config.servers.is_empty() && config.hosts.is_empty() && !config.fake_ip {
        return None;
    }

    if config.servers.is_empty() {
        config.servers.push(DnsServer::udp("local", "system"));
    }
    config.final_server = config
        .servers
        .iter()
        .find(|s| !s.is_fakeip() && s.domains.is_empty())
        .or_else(|| config.servers.iter().find(|s| !s.is_fakeip()))
        .map(|s| s.tag.clone());

    Some(config)
}

fn parse_clash_dns_policy_domains(pattern: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in pattern.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(rest) = item.strip_prefix("geosite:") {
            if safe_tag(rest) {
                out.push(format!("geosite:{rest}"));
            }
        } else if let Some(rest) = item.strip_prefix("rule-set:") {
            if safe_tag(rest) {
                out.push(format!("rule_set:{rest}"));
            }
        } else if let Some(rest) = item
            .strip_prefix("+.")
            .or_else(|| item.strip_prefix("*."))
            .or_else(|| item.strip_prefix('.'))
        {
            if !rest.is_empty() {
                out.push(format!("suffix:{rest}"));
            }
        } else if !item.contains('*') {
            out.push(format!("full:{item}"));
        }
    }
    out
}

fn map_clash_detour(raw: &str, group_ids: &BTreeMap<String, String>) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "DIRECT" => "direct".into(),
        "PROXY" => "proxy".into(),
        _ => group_ids
            .get(raw.trim())
            .cloned()
            .unwrap_or_else(|| "proxy".into()),
    }
}

fn unique_dns_tag(prefix: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(prefix.to_string()) {
        return prefix.to_string();
    }
    for index in 2..=9999 {
        let candidate = format!("{prefix}-{index}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    crate::util::new_id(prefix)
}

fn compile_optional_regex(
    pattern: Option<&str>,
    group_name: &str,
    field: &str,
    warnings: &mut Vec<String>,
) -> Option<Regex> {
    let pattern = pattern?.trim();
    if pattern.is_empty() {
        return None;
    }
    match Regex::new(pattern) {
        Ok(re) => Some(re),
        Err(_) => {
            warnings.push(format!(
                "策略组“{group_name}”的 {field} 正则无效，已忽略过滤条件"
            ));
            None
        }
    }
}

fn is_clash_builtin(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "DIRECT" | "REJECT" | "REJECT-DROP" | "PASS" | "COMPATIBLE"
    )
}

fn yaml_str(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn yaml_u64(value: &Value, key: &str) -> Option<u64> {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => parse_duration_secs(s),
        _ => None,
    }
}

fn yaml_bool(value: &Value, key: &str) -> Option<bool> {
    match value.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => Some(crate::util::truthy(s)),
        Some(Value::Number(n)) => Some(n.as_u64().unwrap_or(0) > 0),
        _ => None,
    }
}

fn yaml_string_list(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::Sequence(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}
