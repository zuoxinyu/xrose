//! Merge and replace application logic with referential integrity repair.

use std::collections::{BTreeSet, HashMap};

use crate::error::{Error, Result};
use crate::model::{
    AppModel, DIRECT_VIRTUAL_NODE_ID, DnsConfig, DnsServer, GroupKind, OutboundGroup, RouteAction,
    RouteRule, RouteRuleGroup,
};
use crate::route::transfer::safe_tag;
use crate::subscription::Subscription;
use crate::util::{new_id, now_unix};

use super::{
    ConfigFormat, ImportMode, ImportOptions, ImportResult, ImportSection, ParsedConfig,
    ParsedProfile, ParsedRouting, dedup_warnings, safe_rule_set_url,
};

pub(crate) fn apply_import(
    parsed: &ParsedConfig,
    model: &mut AppModel,
    subscriptions: &mut Vec<Subscription>,
    options: &ImportOptions,
) -> Result<ImportResult> {
    if options.sections.is_empty() {
        return Err(Error::config("请至少选择一个要导入的配置模块"));
    }

    let profile = if parsed.profiles.is_empty() {
        None
    } else {
        let index = options
            .profile_index
            .min(parsed.profiles.len().saturating_sub(1));
        Some(&parsed.profiles[index])
    };

    let available: BTreeSet<ImportSection> = profile
        .map(|p| p.counts(parsed.subscriptions.len()).available_sections())
        .unwrap_or_else(|| {
            if parsed.subscriptions.is_empty() {
                Vec::new()
            } else {
                vec![ImportSection::Subscriptions]
            }
        })
        .into_iter()
        .collect();

    let effective_sections: BTreeSet<ImportSection> =
        options.sections.intersection(&available).copied().collect();
    if effective_sections.is_empty() {
        return Err(Error::config("所选模块中没有可导入的内容"));
    }

    let mut result = ImportResult {
        format: parsed.format,
        mode: options.mode,
        ..Default::default()
    };

    let want_nodes = effective_sections.contains(&ImportSection::Nodes);
    let want_groups = effective_sections.contains(&ImportSection::Groups);
    let want_routing = effective_sections.contains(&ImportSection::Routing);
    let want_dns = effective_sections.contains(&ImportSection::Dns);
    let want_subs = effective_sections.contains(&ImportSection::Subscriptions);

    let mut node_id_map: HashMap<String, String> = HashMap::new();
    let mut group_id_map: HashMap<String, String> = HashMap::new();
    let mut group_name_by_old_id: HashMap<String, String> = HashMap::new();

    if let Some(profile) = profile {
        for group in &profile.groups {
            group_name_by_old_id.insert(group.id.clone(), group.name.clone());
        }

        // 1. Nodes ----------------------------------------------------------
        if want_nodes {
            apply_nodes(
                profile,
                model,
                options.mode,
                want_groups,
                &mut node_id_map,
                &mut result,
            );
        } else if want_groups {
            // Map source nodes to existing local nodes by fingerprint or name so
            // importing groups alone still binds valid members when present.
            let by_fp: HashMap<String, String> = model
                .nodes
                .iter()
                .map(|n| (n.fingerprint(), n.id.clone()))
                .collect();
            let by_name: HashMap<String, String> = model
                .nodes
                .iter()
                .map(|n| (n.name.clone(), n.id.clone()))
                .collect();
            for node in &profile.nodes {
                if let Some(id) = by_fp
                    .get(&node.fingerprint())
                    .or_else(|| by_name.get(&node.name))
                {
                    node_id_map.insert(node.id.clone(), id.clone());
                }
            }
        }

        // 2. Outbound groups ------------------------------------------------
        if want_groups {
            apply_groups(
                profile,
                model,
                options.mode,
                &node_id_map,
                &mut group_id_map,
                &mut result,
            );
        } else if want_nodes && options.mode == ImportMode::Replace {
            repair_existing_groups_after_node_replace(model);
        }

        // 3. Routing --------------------------------------------------------
        if want_routing && let Some(routing) = &profile.routing {
            apply_routing(
                parsed.format,
                routing,
                model,
                options.mode,
                &group_id_map,
                &group_name_by_old_id,
                &mut result,
            );
        }

        // 4. DNS ------------------------------------------------------------
        if want_dns && let Some(dns) = &profile.dns {
            apply_dns(
                dns,
                model,
                options.mode,
                &group_id_map,
                &group_name_by_old_id,
                &mut result,
            );
        }

        // Repair dangling group references when groups were replaced independently.
        if want_groups && options.mode == ImportMode::Replace {
            repair_dangling_group_refs(model);
        }

        // Restore work_mode only on full Xrose backup replace (never auto-enter Mesh).
        if parsed.format == ConfigFormat::XroseBackup
            && options.mode == ImportMode::Replace
            && want_nodes
            && let Some(mode) = profile.work_mode
        {
            model.work_mode = mode;
        }
    }

    // 5. Subscriptions ------------------------------------------------------
    if want_subs {
        apply_subscriptions(
            &parsed.subscriptions,
            subscriptions,
            options.mode,
            &mut result,
        );
    }

    model.ensure_default_groups();
    model.updated_at = now_unix();
    dedup_warnings(&mut result.warnings);
    Ok(result)
}

fn apply_nodes(
    profile: &ParsedProfile,
    model: &mut AppModel,
    mode: ImportMode,
    importing_groups: bool,
    node_id_map: &mut HashMap<String, String>,
    result: &mut ImportResult,
) {
    match mode {
        ImportMode::Replace => {
            // Preserve local mesh exit gateway virtual nodes bound to this machine's mesh profiles.
            model.nodes.retain(|node| node.is_mesh_gateway());
            let mut seen_fp: HashMap<String, String> = model
                .nodes
                .iter()
                .map(|n| (n.fingerprint(), n.id.clone()))
                .collect();
            for source_node in &profile.nodes {
                if source_node.id == DIRECT_VIRTUAL_NODE_ID || source_node.is_mesh_gateway() {
                    continue;
                }
                let fp = source_node.fingerprint();
                if let Some(existing_id) = seen_fp.get(&fp) {
                    node_id_map.insert(source_node.id.clone(), existing_id.clone());
                    result.nodes_skipped += 1;
                    continue;
                }
                let mut node = source_node.clone();
                let new_node_id = new_id("node");
                node.id = new_node_id.clone();
                node.sort = model.nodes.len() as i64;
                seen_fp.insert(fp, new_node_id.clone());
                node_id_map.insert(source_node.id.clone(), new_node_id);
                model.nodes.push(node);
                result.nodes_added += 1;
            }
            for (idx, node) in model.nodes.iter_mut().enumerate() {
                node.sort = idx as i64;
            }
        }
        ImportMode::Merge => {
            let mut seen_fp: HashMap<String, String> = model
                .nodes
                .iter()
                .map(|n| (n.fingerprint(), n.id.clone()))
                .collect();
            let mut newly_added_ids = Vec::new();
            for source_node in &profile.nodes {
                if source_node.id == DIRECT_VIRTUAL_NODE_ID || source_node.is_mesh_gateway() {
                    continue;
                }
                let fp = source_node.fingerprint();
                if let Some(existing_id) = seen_fp.get(&fp) {
                    node_id_map.insert(source_node.id.clone(), existing_id.clone());
                    result.nodes_skipped += 1;
                    continue;
                }
                let mut node = source_node.clone();
                let new_node_id = new_id("node");
                node.id = new_node_id.clone();
                node.sort = model.nodes.len() as i64;
                seen_fp.insert(fp, new_node_id.clone());
                node_id_map.insert(source_node.id.clone(), new_node_id.clone());
                newly_added_ids.push(new_node_id);
                model.nodes.push(node);
                result.nodes_added += 1;
            }
            if !newly_added_ids.is_empty() && !importing_groups {
                let default_id = model.default_group_id();
                for group in &mut model.groups {
                    if matches!(group.id.as_str(), "proxy" | "auto")
                        || default_id.as_deref() == Some(group.id.as_str())
                    {
                        for id in &newly_added_ids {
                            if !group.members.contains(id) {
                                group.members.push(id.clone());
                            }
                        }
                        group.repair();
                    }
                }
            }
        }
    }
}

fn repair_existing_groups_after_node_replace(model: &mut AppModel) {
    let valid_ids: BTreeSet<String> = model.nodes.iter().map(|n| n.id.clone()).collect();
    let proxy_node_ids: Vec<String> = model
        .nodes
        .iter()
        .filter(|n| !n.is_mesh_gateway())
        .map(|n| n.id.clone())
        .collect();
    let default_id = model.default_group_id();
    for group in &mut model.groups {
        group.members.retain(|id| valid_ids.contains(id));
        if matches!(group.id.as_str(), "proxy" | "auto")
            || default_id.as_deref() == Some(group.id.as_str())
        {
            for id in &proxy_node_ids {
                if !group.members.contains(id) {
                    group.members.push(id.clone());
                }
            }
        }
        group.repair();
    }
}

fn apply_groups(
    profile: &ParsedProfile,
    model: &mut AppModel,
    mode: ImportMode,
    node_id_map: &HashMap<String, String>,
    group_id_map: &mut HashMap<String, String>,
    result: &mut ImportResult,
) {
    let valid_nodes: BTreeSet<String> = model.nodes.iter().map(|n| n.id.clone()).collect();
    let map_member = |old: &str| -> Option<String> {
        let mapped = node_id_map.get(old).map(String::as_str).unwrap_or(old);
        valid_nodes.contains(mapped).then(|| mapped.to_string())
    };

    match mode {
        ImportMode::Replace => {
            let mesh_gateways: Vec<String> = model
                .nodes
                .iter()
                .filter(|n| n.is_mesh_gateway())
                .map(|n| n.id.clone())
                .collect();
            let mut next_groups = Vec::new();
            let mut used_ids: BTreeSet<String> = BTreeSet::new();
            for source in &profile.groups {
                let mut members = Vec::new();
                for m in &source.members {
                    if let Some(id) = map_member(m)
                        && !members.contains(&id)
                    {
                        members.push(id);
                    }
                }
                let new_group_id = if source.id == "proxy" && used_ids.insert("proxy".into()) {
                    "proxy".into()
                } else {
                    let id = new_id("group");
                    used_ids.insert(id.clone());
                    id
                };
                let mut group = source.clone();
                group.id = new_group_id.clone();
                group.members = members;
                group.selected = source
                    .selected
                    .as_deref()
                    .and_then(map_member)
                    .filter(|id| group.members.contains(id));
                group.repair();
                group_id_map.insert(source.id.clone(), new_group_id);
                next_groups.push(group);
                result.groups_applied += 1;
            }
            if !next_groups.is_empty() {
                // Re-attach preserved local mesh exit gateways to native selector/urltest groups.
                if !mesh_gateways.is_empty() {
                    for group in &mut next_groups {
                        if !group.managed()
                            && (group.manually_selectable() || group.kind == GroupKind::UrlTest)
                        {
                            for gw in &mesh_gateways {
                                if !group.members.contains(gw) {
                                    group.members.push(gw.clone());
                                }
                            }
                            group.repair();
                        }
                    }
                }
                model.groups = next_groups;
                model.default_group = profile
                    .default_group
                    .as_deref()
                    .and_then(|id| group_id_map.get(id).cloned())
                    .or_else(|| {
                        model
                            .groups
                            .iter()
                            .find(|g| g.kind == GroupKind::Selector && g.enabled)
                            .or_else(|| model.groups.first())
                            .map(|g| g.id.clone())
                    });
            }
        }
        ImportMode::Merge => {
            let had_empty_default = model
                .default_group_id()
                .and_then(|id| model.group(&id))
                .is_some_and(|g| g.members.is_empty());

            for source in &profile.groups {
                let mut members = Vec::new();
                for m in &source.members {
                    if let Some(id) = map_member(m)
                        && !members.contains(&id)
                    {
                        members.push(id);
                    }
                }
                if let Some(existing) = model
                    .groups
                    .iter_mut()
                    .find(|g| g.name.eq_ignore_ascii_case(&source.name))
                {
                    for id in members {
                        if !existing.members.contains(&id) {
                            existing.members.push(id);
                        }
                    }
                    existing.repair();
                    group_id_map.insert(source.id.clone(), existing.id.clone());
                    result.groups_applied += 1;
                } else {
                    let mut group = source.clone();
                    let new_group_id = new_id("group");
                    group.id = new_group_id.clone();
                    group.members = members;
                    group.selected = source
                        .selected
                        .as_deref()
                        .and_then(map_member)
                        .filter(|id| group.members.contains(id));
                    group.repair();
                    group_id_map.insert(source.id.clone(), new_group_id);
                    model.groups.push(group);
                    result.groups_applied += 1;
                }
            }

            // If the default group started empty (fresh profile), populate it with newly
            // imported nodes so the header node switcher is immediately populated.
            if had_empty_default
                && let Some(default_id) = model.default_group_id()
                && let Some(group) = model.group_mut(&default_id)
                && group.members.is_empty()
            {
                for node_id in node_id_map.values() {
                    if valid_nodes.contains(node_id) && !group.members.contains(node_id) {
                        group.members.push(node_id.clone());
                    }
                }
                group.repair();
            }
        }
    }
}

fn apply_routing(
    format: ConfigFormat,
    incoming: &ParsedRouting,
    model: &mut AppModel,
    mode: ImportMode,
    group_id_map: &HashMap<String, String>,
    group_name_by_old_id: &HashMap<String, String>,
    result: &mut ImportResult,
) {
    // 1. Rule sets -----------------------------------------------------------
    let mut valid_sets = Vec::new();
    let mut seen_incoming_tags: BTreeSet<String> = BTreeSet::new();
    for set in &incoming.rule_sets {
        let key = set.tag.to_ascii_lowercase();
        if !safe_tag(&set.tag) || !safe_rule_set_url(&set.url) || !seen_incoming_tags.insert(key) {
            result
                .warnings
                .push(format!("已跳过无效或重复的规则集“{}”", set.tag));
            continue;
        }
        valid_sets.push(set.clone());
    }

    match mode {
        ImportMode::Replace => {
            model.routing.rule_sets = valid_sets;
            result.rule_sets_applied = model.routing.rule_sets.len();
            if let Some(source) = incoming.rule_set_source {
                model.routing.rule_set_source = source;
            }
            if let Some(template) = &incoming.custom_rule_set_template {
                model.routing.custom_rule_set_template = template.clone();
            }
        }
        ImportMode::Merge => {
            for set in valid_sets {
                let key = set.tag.to_ascii_lowercase();
                if let Some(existing) = model
                    .routing
                    .rule_sets
                    .iter()
                    .find(|s| s.tag.to_ascii_lowercase() == key)
                {
                    if *existing != set
                        && crate::adapters::resolve_rule_set(model, &existing.tag).as_ref()
                            != Some(&set)
                    {
                        result.warnings.push(format!(
                            "规则集“{}”与本机已有定义不同，已保留本机定义",
                            set.tag
                        ));
                    }
                    continue;
                }
                model.routing.rule_sets.push(set);
                result.rule_sets_applied += 1;
            }
        }
    }

    // 2. Remap rule actions & filter rules referencing undefined rule sets ---
    let defined_sets: BTreeSet<String> = model
        .routing
        .rule_sets
        .iter()
        .map(|s| s.tag.clone())
        .collect();
    let mut fallback_group_refs = 0usize;
    let remap_action = |action: &mut RouteAction, fallbacks: &mut usize| {
        if let Some(old_group_id) = action.group.clone() {
            if let Some(mapped) = group_id_map.get(&old_group_id) {
                action.group = Some(mapped.clone());
            } else if model.group(&old_group_id).is_some() {
                action.group = Some(old_group_id);
            } else if let Some(old_name) = group_name_by_old_id.get(&old_group_id)
                && let Some(existing) = model
                    .groups
                    .iter()
                    .find(|g| g.name.eq_ignore_ascii_case(old_name))
            {
                action.group = Some(existing.id.clone());
            } else {
                action.group = None;
                *fallbacks += 1;
            }
        }
    };

    let sanitize_rule = |mut rule: RouteRule, fallbacks: &mut usize| -> Option<RouteRule> {
        rule.id = new_id("rule");
        rule.matchers.rule_sets.retain(|tag| {
            safe_tag(tag)
                && (defined_sets.contains(tag) || crate::adapters::default_rule_set(tag).is_some())
        });
        if rule.matchers.is_empty() {
            return None;
        }
        remap_action(&mut rule.action, fallbacks);
        Some(rule)
    };

    let mut remapped_groups: Vec<RouteRuleGroup> = Vec::new();
    for src_group in &incoming.rule_groups {
        let mut rules = Vec::new();
        for rule in &src_group.rules {
            if let Some(clean) = sanitize_rule(rule.clone(), &mut fallback_group_refs) {
                rules.push(clean);
            }
        }
        if !rules.is_empty() || incoming.preserve_structure {
            remapped_groups.push(RouteRuleGroup {
                id: new_id("rule-group"),
                name: src_group.name.clone(),
                enabled: src_group.enabled,
                rules,
            });
        }
    }

    let mut remapped_rules: Vec<RouteRule> = Vec::new();
    for rule in &incoming.rules {
        if let Some(clean) = sanitize_rule(rule.clone(), &mut fallback_group_refs) {
            remapped_rules.push(clean);
        }
    }

    let remapped_final = incoming.final_action.clone().map(|mut final_action| {
        let mut dummy = 0;
        remap_action(&mut final_action, &mut dummy);
        final_action
    });

    if fallback_group_refs > 0 {
        result.warnings.push(format!(
            "{fallback_group_refs} 条规则引用的出站组不存在，已改为默认代理出站"
        ));
    }

    match mode {
        ImportMode::Replace => {
            result.rule_groups_applied = remapped_groups.len();
            result.rules_applied =
                remapped_rules.len() + remapped_groups.iter().map(|g| g.rules.len()).sum::<usize>();
            model.routing.rule_groups = remapped_groups;
            model.routing.rules = remapped_rules;
            model.routing.preset = incoming.preset;
            if let Some(final_action) = remapped_final {
                model.routing.final_action = final_action;
            }
            if let Some(strategy) = incoming.domain_strategy {
                model.routing.domain_strategy = strategy;
            }
        }
        ImportMode::Merge => {
            if incoming.preserve_structure {
                for group in remapped_groups {
                    result.rules_applied += group.rules.len();
                    result.rule_groups_applied += 1;
                    model.routing.rule_groups.push(group);
                }
                for rule in remapped_rules {
                    let duplicate = model.routing.rules.iter().any(|existing| {
                        existing.name == rule.name
                            && existing.matchers == rule.matchers
                            && existing.action == rule.action
                            && existing.enabled == rule.enabled
                    });
                    if !duplicate {
                        model.routing.rules.push(rule);
                        result.rules_applied += 1;
                    }
                }
            } else {
                for group in remapped_groups {
                    result.rules_applied += group.rules.len();
                    result.rule_groups_applied += 1;
                    model.routing.rule_groups.push(group);
                }
                if !remapped_rules.is_empty() {
                    let group_name = unique_rule_group_name(
                        format.import_group_name(),
                        &model.routing.rule_groups,
                    );
                    result.rules_applied += remapped_rules.len();
                    result.rule_groups_applied += 1;
                    model.routing.rule_groups.push(RouteRuleGroup {
                        id: new_id("rule-group"),
                        name: group_name,
                        enabled: true,
                        rules: remapped_rules,
                    });
                }
            }
            if incoming.final_action.is_some() {
                result.warnings.push(
                    "合并模式下保留了当前默认路由动作（如需覆盖默认动作请使用覆盖模式）".into(),
                );
            }
        }
    }
}

fn unique_rule_group_name(base: &str, existing: &[RouteRuleGroup]) -> String {
    if !existing.iter().any(|g| g.name == base) {
        return base.to_string();
    }
    for n in 2..=999 {
        let candidate = format!("{base} {n}");
        if !existing.iter().any(|g| g.name == candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", now_unix())
}

fn apply_dns(
    incoming: &DnsConfig,
    model: &mut AppModel,
    mode: ImportMode,
    group_id_map: &HashMap<String, String>,
    group_name_by_old_id: &HashMap<String, String>,
    result: &mut ImportResult,
) {
    let mut remapped = incoming.clone();
    let mut remapped_detours = 0usize;
    for server in &mut remapped.servers {
        if let Some(detour) = server.detour.clone() {
            let trimmed = detour.trim();
            if trimmed.is_empty() {
                server.detour = None;
            } else if matches!(trimmed, "direct" | "proxy" | "default") {
                server.detour = Some(trimmed.to_string());
            } else if let Some(new_id) = group_id_map.get(trimmed) {
                server.detour = Some(new_id.clone());
            } else if let Some(old_id) = trimmed.strip_prefix("group-")
                && let Some(new_id) = group_id_map.get(old_id)
            {
                server.detour = Some(new_id.clone());
            } else if model
                .groups
                .iter()
                .any(|g| g.id == trimmed || g.tag() == trimmed)
            {
                server.detour = Some(trimmed.to_string());
            } else if let Some(old_name) = group_name_by_old_id.get(trimmed)
                && let Some(existing) = model
                    .groups
                    .iter()
                    .find(|g| g.name.eq_ignore_ascii_case(old_name))
            {
                server.detour = Some(existing.id.clone());
            } else {
                server.detour = Some("proxy".into());
                remapped_detours += 1;
            }
        }
    }
    if remapped_detours > 0 {
        result.warnings.push(format!(
            "{remapped_detours} 个 DNS 服务器的出站引用不存在，已改为默认代理出站"
        ));
    }

    match mode {
        ImportMode::Replace => {
            if remapped.fake_ip && remapped.fake_ip_range.trim().is_empty() {
                remapped.fake_ip_range = crate::model::dns::default_fakeip_range();
            }
            if remapped.servers.iter().all(DnsServer::is_fakeip) {
                remapped.servers.push(DnsServer::udp("local", "system"));
            }
            if remapped.final_server.as_deref().is_none_or(|tag| {
                !remapped
                    .servers
                    .iter()
                    .any(|s| s.tag == tag && !s.is_fakeip())
            }) {
                remapped.final_server = remapped
                    .servers
                    .iter()
                    .find(|s| !s.is_fakeip() && s.domains.is_empty())
                    .or_else(|| remapped.servers.iter().find(|s| !s.is_fakeip()))
                    .map(|s| s.tag.clone());
            }
            model.dns = remapped;
            result.dns_applied = true;
        }
        ImportMode::Merge => {
            let mut used_tags: BTreeSet<String> =
                model.dns.servers.iter().map(|s| s.tag.clone()).collect();
            for mut server in remapped.servers {
                if server.is_fakeip() {
                    continue;
                }
                let same_server = model.dns.servers.iter().any(|existing| {
                    existing.server_type == server.server_type
                        && existing.address == server.address
                        && existing.detour == server.detour
                        && existing.domains == server.domains
                });
                if same_server {
                    continue;
                }
                if used_tags.contains(&server.tag) {
                    let base = server.tag.clone();
                    let mut suffix = 2usize;
                    let mut candidate = format!("{base}-import");
                    while !used_tags.insert(candidate.clone()) {
                        candidate = format!("{base}-import-{suffix}");
                        suffix += 1;
                    }
                    server.tag = candidate;
                } else {
                    used_tags.insert(server.tag.clone());
                }
                model.dns.servers.push(server);
            }
            for (domain, ips) in remapped.hosts {
                let entry = model.dns.hosts.entry(domain).or_default();
                for ip in ips {
                    if !entry.contains(&ip) {
                        entry.push(ip);
                    }
                }
            }
            for pattern in remapped.fake_ip_filter {
                if !model.dns.fake_ip_filter.contains(&pattern) {
                    model.dns.fake_ip_filter.push(pattern);
                }
            }
            if model.dns.final_server.is_none() {
                model.dns.final_server = model
                    .dns
                    .servers
                    .iter()
                    .find(|s| !s.is_fakeip())
                    .map(|s| s.tag.clone());
            }
            result.dns_applied = true;
        }
    }

    for issue in crate::dns::validate(&model.dns) {
        result.warnings.push(format!("DNS 校验提示：{issue}"));
    }
}

fn repair_dangling_group_refs(model: &mut AppModel) {
    let valid_ids: BTreeSet<String> = model.groups.iter().map(|g| g.id.clone()).collect();
    let valid_tags: BTreeSet<String> = model.groups.iter().map(OutboundGroup::tag).collect();
    for rule in model.routing.rules.iter_mut().chain(
        model
            .routing
            .rule_groups
            .iter_mut()
            .flat_map(|g| &mut g.rules),
    ) {
        if rule
            .action
            .group
            .as_deref()
            .is_some_and(|id| !valid_ids.contains(id))
        {
            rule.action.group = None;
        }
    }
    if model
        .routing
        .final_action
        .group
        .as_deref()
        .is_some_and(|id| !valid_ids.contains(id))
    {
        model.routing.final_action.group = None;
    }
    for server in &mut model.dns.servers {
        if let Some(detour) = server.detour.as_deref()
            && !matches!(detour.trim(), "" | "direct" | "proxy" | "default")
            && !valid_ids.contains(detour)
            && !valid_tags.contains(detour)
        {
            server.detour = Some("proxy".into());
        }
    }
}

fn apply_subscriptions(
    incoming: &[Subscription],
    current: &mut Vec<Subscription>,
    mode: ImportMode,
    result: &mut ImportResult,
) {
    match mode {
        ImportMode::Replace => {
            current.clear();
            let mut seen_urls = BTreeSet::new();
            for source in incoming {
                let key = source.url.trim().to_string();
                if !seen_urls.insert(key) {
                    result.subscriptions_skipped += 1;
                    continue;
                }
                let mut sub = source.clone();
                sub.id = new_id("sub");
                sub.sort = current.len() as i64;
                current.push(sub);
                result.subscriptions_added += 1;
            }
        }
        ImportMode::Merge => {
            let mut seen_urls: BTreeSet<String> =
                current.iter().map(|s| s.url.trim().to_string()).collect();
            for source in incoming {
                let key = source.url.trim().to_string();
                if !seen_urls.insert(key) {
                    result.subscriptions_skipped += 1;
                    continue;
                }
                let mut sub = source.clone();
                sub.id = new_id("sub");
                sub.sort = current.len() as i64;
                current.push(sub);
                result.subscriptions_added += 1;
            }
        }
    }
}
