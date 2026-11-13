//! Compile the intent model into core-specific JSON and validate it with the
//! real core binary. All schema knowledge lives here.

pub mod singbox;
pub mod xray;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::process::run_capture;
use crate::error::Result;
use crate::model::{
    AppModel, CoreKind, DnsServer, Matchers, RouteAction, RouteActionKind, RuleSet, RuleSetFormat,
    WorkMode,
};
use crate::settings::LogLevel;
use crate::settings::Settings;

pub use singbox::SingBoxAdapter;
pub use xray::XrayAdapter;

/// Built-in sing-box asset sources, shared by compilation and downloads.
pub(crate) fn default_rule_set(tag: &str) -> Option<RuleSet> {
    let url = if tag == "geosite-gfw" {
        // SagerNet follows domain-list-community, which does not publish gfw.
        "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/geosite/gfw.srs".into()
    } else if let Some(name) = tag.strip_prefix("geosite-") {
        format!(
            "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-{name}.srs"
        )
    } else {
        let code = tag.strip_prefix("geoip-")?;
        format!("https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-{code}.srs")
    };
    Some(RuleSet {
        tag: tag.into(),
        url,
        format: RuleSetFormat::Binary,
        download_detour: false,
        update_interval_hours: 168,
    })
}

pub(crate) fn resolve_rule_set(model: &AppModel, tag: &str) -> Option<RuleSet> {
    use crate::model::RuleSetSource;
    let configured = model.routing.rule_sets.iter().find(|set| set.tag == tag);
    let Some(default) = default_rule_set(tag) else {
        return configured.cloned();
    };
    let mut set = configured.cloned().unwrap_or_else(|| default.clone());
    let legacy_gfw = tag == "geosite-gfw"
        && set.url
            == "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-gfw.srs";
    if set.format != RuleSetFormat::Binary || (set.url != default.url && !legacy_gfw) {
        return Some(set);
    }
    let (kind, name) = tag.split_once('-')?;
    set.url = match model.routing.rule_set_source {
        RuleSetSource::Upstream => default.url,
        RuleSetSource::MetaCubeX => format!(
            "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/sing/geo/{kind}/{name}.srs"
        ),
        RuleSetSource::JsDelivr => {
            let repo = match kind {
                "geoip" => "sing-geoip",
                _ => "sing-geosite",
            };
            if tag == "geosite-gfw" {
                "https://cdn.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/geo/geosite/gfw.srs"
                    .into()
            } else {
                format!("https://cdn.jsdelivr.net/gh/SagerNet/{repo}@rule-set/{tag}.srs")
            }
        }
        RuleSetSource::Custom => crate::model::route::expand_rule_set_template(
            &model.routing.custom_rule_set_template,
            kind,
            name,
        ),
    };
    Some(set)
}

/// Only the files referenced by this run are startup dependencies.
pub(crate) fn required_rule_sets(model: &AppModel) -> Vec<RuleSet> {
    rule_set_tags(model, model.work_mode)
        .into_iter()
        .filter_map(|tag| resolve_rule_set(model, &tag))
        .collect()
}

fn add_matcher_tags(tags: &mut BTreeSet<String>, matchers: &Matchers) {
    tags.extend(matchers.rule_sets.iter().cloned());
    tags.extend(matchers.geosites.iter().map(|s| format!("geosite-{s}")));
    tags.extend(matchers.geoips.iter().map(|s| format!("geoip-{s}")));
}

fn add_dns_tags(tags: &mut BTreeSet<String>, server: &DnsServer) {
    let domains = split_dns_domains(&server.domains);
    tags.extend(domains.geosites.into_iter().map(|s| format!("geosite-{s}")));
    tags.extend(domains.rule_sets);
}

pub(crate) fn rule_set_tags(model: &AppModel, mode: WorkMode) -> BTreeSet<String> {
    let mut tags = BTreeSet::new();
    for rule in model.routing.rules_for_mode(mode) {
        add_matcher_tags(&mut tags, &rule.matchers);
    }
    if model.dns.enabled {
        for server in &model.dns.servers {
            if mode == WorkMode::Direct && (server.requires_proxy() || server.is_fakeip()) {
                continue;
            }
            add_dns_tags(&mut tags, server);
        }
    }
    if mode == WorkMode::Proxy && model.routing.bypass_china_ips {
        tags.insert("geoip-cn".into());
    }
    tags
}

/// Assets available for manual download, including inactive rules and other built-in presets.
pub(crate) fn downloadable_rule_sets(model: &AppModel) -> Vec<RuleSet> {
    use crate::model::{RoutePreset, preset_rules, route::default_rule_sets};
    static PRESET_TAGS: std::sync::LazyLock<BTreeSet<String>> = std::sync::LazyLock::new(|| {
        let mut tags: BTreeSet<_> = default_rule_sets().into_iter().map(|set| set.tag).collect();
        for preset in RoutePreset::all() {
            for rule in preset_rules(preset) {
                add_matcher_tags(&mut tags, &rule.matchers);
            }
        }
        tags
    });
    let mut tags = PRESET_TAGS.clone();
    tags.extend(model.routing.rule_sets.iter().map(|set| set.tag.clone()));
    for rule in model
        .routing
        .rules
        .iter()
        .chain(model.routing.rule_groups.iter().flat_map(|g| &g.rules))
    {
        add_matcher_tags(&mut tags, &rule.matchers);
    }
    for server in &model.dns.servers {
        add_dns_tags(&mut tags, server);
    }
    tags.into_iter()
        .filter_map(|tag| resolve_rule_set(model, &tag))
        .collect()
}

/// Reduce saved user intent to the node and resolver graph for this run.
fn compilation_model(model: &AppModel, warnings: &mut Vec<String>) -> AppModel {
    let mut effective = model.clone();
    if effective.work_mode == WorkMode::Direct {
        effective.nodes.clear();
        effective.groups.clear();
        effective.default_group = None;
        effective.routing.bypass_lan = false;
        effective.routing.bypass_china_ips = false;
        if effective.dns.enabled {
            effective.dns.servers.retain(|server| {
                if server.requires_proxy() {
                    warnings.push(format!(
                        "直连模式下已跳过依赖代理出站的 DNS 服务器「{}」，改用本地/直连 DNS",
                        server.tag
                    ));
                    false
                } else {
                    true
                }
            });
            if effective.dns.final_server.as_ref().is_none_or(|tag| {
                !effective
                    .dns
                    .servers
                    .iter()
                    .any(|s| &s.tag == tag && !s.is_fakeip())
            }) {
                // Reuse an unscoped direct upstream when the saved default was
                // removed. Scoped private resolvers never become a public fallback.
                effective.dns.final_server = effective
                    .dns
                    .servers
                    .iter()
                    .find(|s| !s.is_fakeip() && s.domains.is_empty())
                    .map(|s| s.tag.clone());
            }
        }
    }
    effective
}

#[derive(Debug, Clone)]
pub struct CompileContext {
    pub core_version: Option<semver::Version>,
    pub mesh: crate::mesh::coexist::CoexistencePlan,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub api_port: u16,
    pub stats_port: u16,
    pub log_level: LogLevel,
    pub urltest_url: String,
    pub mixed_port: u16,
}

impl CompileContext {
    pub fn new(settings: &Settings, data_dir: PathBuf, runtime_dir: PathBuf) -> Self {
        Self {
            core_version: None,
            mesh: Default::default(),
            data_dir,
            runtime_dir,
            api_port: settings.api_port,
            stats_port: settings.stats_port,
            log_level: settings.log_level,
            urltest_url: settings.urltest_url.clone(),
            mixed_port: settings.mixed_port,
        }
    }

    pub fn rule_set_path(&self, tag: &str) -> PathBuf {
        self.data_dir.join("rulesets").join(format!("{tag}.srs"))
    }
}

#[derive(Debug, Clone)]
pub struct CompiledConfig {
    pub core: CoreKind,
    pub json: Value,
    pub warnings: Vec<String>,
    pub inbounds: Vec<CompiledInbound>,
    pub rule_sets: Vec<RuleSet>,
    pub default_outbound: String,
    pub(crate) default_proxy: CompiledProxySelection,
    /// Managed groups after removing nodes that this core could not compile.
    pub(crate) managed_groups: Vec<crate::model::OutboundGroup>,
    pub(crate) core_version: Option<semver::Version>,
}

/// Selection after the adapter has removed unsupported nodes and repaired the default group.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompiledProxySelection {
    pub node: Option<String>,
    pub live_group: Option<String>,
}

impl CompiledProxySelection {
    fn from_model(model: &AppModel, core: CoreKind) -> Self {
        let Some(group) = model.default_group_id().and_then(|id| model.group(&id)) else {
            return Self::default();
        };
        Self {
            node: if group.manually_selectable() || group.managed() {
                group.active_node().cloned()
            } else {
                None
            },
            live_group: (core == CoreKind::SingBox && !group.managed()).then(|| group.tag()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledInbound {
    #[serde(default)]
    pub listen: String,
    pub tag: String,
    pub kind: String,
    pub port: u16,
    pub allow_lan: bool,
}

impl CompiledConfig {
    pub fn to_pretty_string(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(&self.json)?)
    }

    pub fn http_port(&self) -> u16 {
        self.inbounds
            .iter()
            .find(|i| i.kind == "http" || i.kind == "mixed")
            .map(|i| i.port)
            .unwrap_or(7890)
    }
}

pub trait CoreAdapter: Send + Sync {
    fn kind(&self) -> CoreKind;
    fn compile(
        &self,
        model: &AppModel,
        settings: &Settings,
        ctx: &CompileContext,
    ) -> Result<CompiledConfig>;
    fn check_args(&self, config_path: &Path) -> Vec<String>;
}

pub fn adapter_for(core: CoreKind) -> &'static dyn CoreAdapter {
    match core {
        CoreKind::Xray => &XRAY,
        CoreKind::SingBox => &SINGBOX,
    }
}

static XRAY: XrayAdapter = XrayAdapter;
static SINGBOX: SingBoxAdapter = SingBoxAdapter;

/// A private HTTP listener routed exclusively through the requested node.
/// No selectors, TUN, cache file, API ports or user routing are shared.
pub(crate) fn compile_probe(core: CoreKind, node: &crate::model::Node, port: u16) -> Result<Value> {
    if !node.usable_with(core) {
        return Err(crate::Error::config("该节点未启用或当前内核不支持该协议"));
    }
    match core {
        CoreKind::SingBox => singbox::compile_probe(node, port),
        CoreKind::Xray => xray::compile_probe(node, port),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationIssue {
    pub line: Option<u32>,
    pub message: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ValidationReport {
    pub ok: bool,
    pub core: CoreKind,
    pub issues: Vec<ValidationIssue>,
    pub raw_output: String,
    pub duration_ms: u64,
    pub checked_at: i64,
}

impl ValidationReport {
    pub fn summary(&self) -> String {
        if self.ok {
            format!(
                "{} 配置检查通过（{} ms）",
                self.core.display_name(),
                self.duration_ms
            )
        } else if let Some(issue) = self.issues.first() {
            issue.message.clone()
        } else {
            format!("{} 配置检查失败", self.core.display_name())
        }
    }
}

/// Runs `xray run -test` / `sing-box check` and turns its output into issues.
pub async fn validate(
    core: CoreKind,
    binary: &Path,
    config_path: &Path,
) -> Result<ValidationReport> {
    let args = adapter_for(core).check_args(config_path);
    let started = Instant::now();
    let (code, output) = run_capture(binary, &args, Duration::from_secs(20)).await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let issues = parse_validation_output(core, &output, code);
    Ok(ValidationReport {
        ok: code == 0,
        core,
        issues,
        raw_output: output,
        duration_ms,
        checked_at: crate::util::now_unix(),
    })
}

pub fn parse_validation_output(
    core: CoreKind,
    output: &str,
    exit_code: i32,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let is_error = match core {
            CoreKind::Xray => {
                trimmed.starts_with("Failed")
                    || trimmed.starts_with("panic:")
                    || trimmed.contains("failed to")
                    || trimmed.contains("unknown field")
                    || trimmed.contains("cannot unmarshal")
            }
            CoreKind::SingBox => {
                trimmed.starts_with("FATAL")
                    || trimmed.starts_with("ERROR")
                    || trimmed.contains("decode config")
                    || trimmed.contains("unknown field")
                    || trimmed.contains("invalid")
            }
        };
        if is_error {
            let cleaned = trimmed
                .trim_start_matches("FATAL")
                .trim_start_matches("ERROR")
                .trim_start_matches("Failed to start:")
                .trim()
                .to_string();
            issues.push(ValidationIssue {
                line: crate::util::extract_line_reference(trimmed),
                message: if cleaned.is_empty() {
                    trimmed.to_string()
                } else {
                    cleaned
                },
                raw: trimmed.to_string(),
            });
        }
    }
    if issues.is_empty() && exit_code != 0 {
        issues.push(ValidationIssue {
            line: None,
            message: format!(
                "{} 配置检查未通过（退出码 {exit_code}）",
                core.display_name()
            ),
            raw: output.lines().last().unwrap_or_default().to_string(),
        });
    }
    issues
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub fn parse_port_specs(specs: &[String]) -> (Vec<u16>, Vec<String>) {
    let mut ports = Vec::new();
    let mut ranges = Vec::new();
    for spec in specs {
        for part in spec.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((a, b)) = part.split_once('-') {
                ranges.push(format!("{}-{}", a.trim(), b.trim()));
            } else if let Ok(p) = part.parse::<u16>() {
                ports.push(p);
            }
        }
    }
    (ports, ranges)
}

pub fn network_str(matchers: &Matchers) -> Option<&'static str> {
    matchers.network.map(|n| match n {
        crate::model::Network::Tcp => "tcp",
        crate::model::Network::Udp => "udp",
        crate::model::Network::TcpUdp => "tcp,udp",
    })
}

pub fn proxy_group_tag(model: &AppModel, action: &RouteAction) -> Option<String> {
    model.resolve_proxy_group(action).map(|gid| {
        model
            .group(&gid)
            .map(|g| g.tag())
            .unwrap_or_else(|| "direct".into())
    })
}

/// Outbound tag for a route action, honoring each core's group model.
pub fn action_tag(core: CoreKind, model: &AppModel, action: &RouteAction) -> Option<String> {
    match action.kind {
        RouteActionKind::Direct => Some("direct".into()),
        RouteActionKind::Block => Some("block".into()),
        RouteActionKind::Proxy if model.work_mode == WorkMode::Direct => Some("direct".into()),
        RouteActionKind::Proxy => {
            if core == CoreKind::Xray {
                // Manual and client-managed groups route directly to the selected node.
                if let Some(group_id) = model.resolve_proxy_group(action) {
                    if let Some(group) = model.group(&group_id) {
                        if group.manually_selectable() || group.managed() {
                            if let Some(node_id) = group.active_node() {
                                if let Some(node) = model.node(node_id) {
                                    return Some(node.outbound_tag());
                                }
                            }
                        }
                        return Some(group.tag());
                    }
                }
                model
                    .default_group_id()
                    .and_then(|gid| model.group(&gid).map(|g| g.tag()))
            } else {
                proxy_group_tag(model, action)
            }
        }
    }
}

/// Maps a model DNS server to an Xray DNS address string.
pub fn xray_dns_address(server: &DnsServer, warnings: &mut Vec<String>) -> Option<String> {
    use crate::model::DnsServerType;
    if server.is_fakeip() {
        warnings.push(format!(
            "Xray 不支持 fake-ip DNS 服务器“{}”，已跳过",
            server.tag
        ));
        return None;
    }
    match server.server_type {
        DnsServerType::Udp | DnsServerType::Tcp
            if matches!(server.address.trim(), "local" | "system" | "localhost") =>
        {
            Some("localhost".into())
        }
        DnsServerType::Udp => Some(server.address.clone()),
        DnsServerType::Tcp => Some(format!("tcp://{}", server.address)),
        DnsServerType::Https => Some(server.display_address()),
        DnsServerType::Tls | DnsServerType::Quic | DnsServerType::H3 => {
            warnings.push(format!(
                "Xray 的 DNS 传输不支持 {}（“{}”），已回退到 UDP",
                server.server_type.as_str(),
                server.tag
            ));
            Some(server.address.clone())
        }
        DnsServerType::FakeIp => None,
    }
}

/// Parses the domain-ish entries of a DNS server into buckets.
#[derive(Debug, Default)]
pub struct DnsDomainSets {
    pub domains: Vec<String>,
    pub suffixes: Vec<String>,
    pub keywords: Vec<String>,
    pub regexes: Vec<String>,
    pub geosites: Vec<String>,
    pub rule_sets: Vec<String>,
}

pub fn split_dns_domains(entries: &[String]) -> DnsDomainSets {
    let mut sets = DnsDomainSets::default();
    for entry in entries {
        let trimmed = entry.trim();
        if let Some(rest) = trimmed.strip_prefix("geosite:") {
            sets.geosites.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("rule_set:") {
            sets.rule_sets.push(rest.to_string());
        } else if let Some(rest) = trimmed
            .strip_prefix("domain:")
            .or_else(|| trimmed.strip_prefix("full:"))
        {
            sets.domains.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("suffix:") {
            sets.suffixes.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("keyword:") {
            sets.keywords.push(rest.to_string());
        } else if let Some(rest) = trimmed
            .strip_prefix("regexp:")
            .or_else(|| trimmed.strip_prefix("regex:"))
        {
            sets.regexes.push(rest.to_string());
        } else if trimmed.starts_with('.') {
            sets.suffixes
                .push(trimmed.trim_start_matches('.').to_string());
        } else {
            sets.suffixes.push(trimmed.to_string());
        }
    }
    sets
}

/// Builds one sing-box DNS rule for a server's domain conditions.
pub fn dns_server_rule(sets: &DnsDomainSets, server_tag: &str) -> serde_json::Value {
    let mut rule = serde_json::Map::new();
    if !sets.domains.is_empty() {
        rule.insert("domain".into(), serde_json::json!(sets.domains));
    }
    if !sets.suffixes.is_empty() {
        rule.insert("domain_suffix".into(), serde_json::json!(sets.suffixes));
    }
    if !sets.keywords.is_empty() {
        rule.insert("domain_keyword".into(), serde_json::json!(sets.keywords));
    }
    if !sets.regexes.is_empty() {
        rule.insert("domain_regex".into(), serde_json::json!(sets.regexes));
    }
    let mut rule_sets: Vec<String> = sets.rule_sets.clone();
    for geosite in &sets.geosites {
        rule_sets.push(format!("geosite-{geosite}"));
    }
    if !rule_sets.is_empty() {
        rule.insert("rule_set".into(), serde_json::json!(rule_sets));
    }
    rule.insert("server".into(), serde_json::json!(server_tag));
    serde_json::Value::Object(rule)
}

pub fn parse_geosite_name(value: &str) -> String {
    format!("geosite-{}", value.trim())
}

pub fn parse_geoip_name(value: &str) -> String {
    format!("geoip-{}", value.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, Protocol, RoutePreset, RouteRule, RouteRuleGroup};
    use serde_json::json;

    #[test]
    fn global_can_boot_without_files_and_prefetch_every_routing_preset() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let ctx = CompileContext::new(&settings, dir.path().join("data"), dir.path().join("run"));
        let mut model = AppModel::default();
        model.add_nodes([Node::new("Fixture", Protocol::Socks, "proxy.example", 1080)]);
        model.work_mode = WorkMode::Proxy;
        model.apply_preset(RoutePreset::Global);
        model.routing.rule_sets.clear();
        assert!(required_rule_sets(&model).is_empty());
        assert!(
            SingBoxAdapter
                .compile(&model, &settings, &ctx)
                .unwrap()
                .json["route"]
                .get("rule_set")
                .is_none()
        );

        // A saved mirror, disabled group, disabled rule, and inactive DNS still belong to the catalog.
        let mut mirror = default_rule_set("geosite-gfw").unwrap();
        mirror.url = "https://mirror.example/gfw.srs".into();
        model.routing.rule_sets.push(mirror.clone());
        let mut inactive = RouteRule::new(
            "Fixture",
            Matchers {
                geoips: vec!["fixture".into()],
                ..Default::default()
            },
            RouteAction::direct(),
        );
        inactive.enabled = false;
        model.routing.rule_groups.push(RouteRuleGroup {
            enabled: false,
            rules: vec![inactive],
            ..Default::default()
        });
        model.dns.enabled = false;
        model.dns.servers[0].domains = vec!["geosite:fixture-dns".into()];
        let all = downloadable_rule_sets(&model);
        let tags: BTreeSet<_> = all.iter().map(|s| s.tag.as_str()).collect();
        assert_eq!(tags.len(), all.len());
        for tag in [
            "geosite-gfw",
            "geosite-cn",
            "geoip-cn",
            "geosite-geolocation-!cn",
            "geosite-category-ads-all",
            "geoip-fixture",
            "geosite-fixture-dns",
        ] {
            assert!(tags.contains(tag), "{tag}");
        }
        assert_eq!(all.iter().find(|s| s.tag == mirror.tag), Some(&mirror));
        assert!(required_rule_sets(&model).is_empty());
        for set in &all {
            crate::util::atomic_write(&ctx.rule_set_path(&set.tag), b"fixture").unwrap();
        }
        for preset in [RoutePreset::BypassMainland, RoutePreset::GfwList] {
            model.apply_preset(preset);
            let compiled = SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
            let sets = compiled.json["route"]["rule_set"].as_array().unwrap();
            assert!(!sets.is_empty());
            assert!(
                sets.iter()
                    .all(|s| s["type"] == "local" && tags.contains(s["tag"].as_str().unwrap()))
            );
        }

        model.apply_preset(RoutePreset::Global);
        model.routing.bypass_china_ips = true;
        assert_eq!(
            required_rule_sets(&model)
                .iter()
                .map(|s| s.tag.as_str())
                .collect::<Vec<_>>(),
            ["geoip-cn"]
        );
        model.dns.enabled = true;
        assert!(
            required_rule_sets(&model)
                .iter()
                .any(|s| s.tag == "geosite-fixture-dns")
        );
    }

    #[test]
    fn zero_node_monitor_keeps_firewall_mesh_and_direct_dns_in_both_cores() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let mut ctx =
            CompileContext::new(&settings, dir.path().join("data"), dir.path().join("run"));
        ctx.mesh.routes.push(crate::mesh::coexist::MeshRoute {
            cidr: "192.0.2.0/24".into(),
            interface: "mesh-test".into(),
        });
        ctx.mesh.dns.push(crate::mesh::coexist::MeshDns {
            suffix: "mesh.example".into(),
            address: "192.0.2.53".into(),
        });
        let mut model = AppModel::default();
        assert!(required_rule_sets(&model).is_empty());
        assert!(
            SingBoxAdapter
                .compile(&model, &settings, &ctx)
                .unwrap()
                .json["route"]["rule_set"]
                .is_null()
        );
        model.dns.fake_ip = true;
        model.apply_preset(RoutePreset::GfwList);
        model.routing.rule_groups.push(RouteRuleGroup {
            rules: vec![RouteRule::new(
                "firewall",
                Matchers {
                    process_names: vec!["fixture".into()],
                    ..Default::default()
                },
                RouteAction::block(),
            )],
            ..Default::default()
        });
        model.routing.rules.push(RouteRule::new(
            "default route",
            Matchers {
                domains: vec!["proxy.example".into()],
                ..Default::default()
            },
            RouteAction::proxy(),
        ));
        model
            .dns
            .hosts
            .insert("hosts.example".into(), vec!["192.0.2.12".into()]);
        let mut remote = DnsServer::https("remote", "192.0.2.20");
        remote.detour = Some("group-private".into());
        remote.domains = vec!["geosite:private-only".into()];
        let mut encrypted = DnsServer::https("secure", "192.0.2.21");
        encrypted.detour = Some("direct".into());
        encrypted.domains = vec!["geosite:service".into()];
        model.dns.servers = vec![remote, encrypted];
        model.dns.final_server = Some("secure".into());
        let required: BTreeSet<_> = required_rule_sets(&model)
            .into_iter()
            .map(|set| set.tag)
            .collect();
        assert_eq!(
            required,
            BTreeSet::from(["geosite-category-ads-all".into(), "geosite-service".into()])
        );
        for core in CoreKind::all() {
            let compiled = adapter_for(core).compile(&model, &settings, &ctx).unwrap();
            assert_eq!(compiled.default_outbound, "direct");
            assert!(compiled.managed_groups.is_empty());
            assert!(
                compiled
                    .warnings
                    .iter()
                    .any(|s| s.contains("已跳过依赖代理出站的 DNS 服务器「remote」"))
            );
            let config = &compiled.json;
            assert!(!config.to_string().contains("private-only"));
            assert!(!config.to_string().contains("192.0.2.20"));
            assert!(config.to_string().contains("192.0.2.21"));
            assert!(config.to_string().contains("hosts.example"));
            assert!(config.to_string().contains("mesh.example"));
            assert!(
                config["outbounds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|o| o["tag"] == "xrose-mesh-0")
            );
            if core == CoreKind::SingBox {
                assert_eq!(config["outbounds"].as_array().unwrap().len(), 2);
                let rules = config["route"]["rules"].as_array().unwrap();
                let block = rules
                    .iter()
                    .position(|r| r["process_name"] == json!(["fixture"]))
                    .unwrap();
                assert_eq!(rules[block]["action"], "reject");
                let forward = rules
                    .iter()
                    .position(|r| {
                        r["outbound"] == "direct" && r["domain"] == json!(["proxy.example"])
                    })
                    .unwrap();
                assert!(block < forward - 1);
                assert_eq!(
                    rules[forward - 1],
                    json!({"domain":["proxy.example"], "action":"resolve"})
                );
                assert_eq!(rules.last().unwrap(), &json!({"action":"resolve"}));
                assert!(config["dns"]["rules"].as_array().unwrap().iter().any(|r| r["server"] == "fakeip" && r["query_type"] == json!(["A", "AAAA"])));
                assert_eq!(config["dns"]["final"], "secure");
            } else {
                assert_eq!(config["outbounds"].as_array().unwrap().len(), 4);
                let rules = config["routing"]["rules"].as_array().unwrap();
                assert!(
                    rules
                        .iter()
                        .any(|r| r["process"] == json!(["fixture"]) && r["outboundTag"] == "block")
                );
                assert!(
                    rules
                        .iter()
                        .any(|r| r["domain"] == json!(["full:proxy.example"])
                            && r["outboundTag"] == "direct")
                );
                assert!(
                    compiled
                        .warnings
                        .iter()
                        .any(|w| w.contains("Xray 不支持连接列表"))
                );
            }
        }
    }

    #[test]
    fn direct_resolver_fallbacks_remain_scoped_without_tun_recursion() {
        let settings = Settings::default();
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = CompileContext::new(&settings, dir.path().into(), dir.path().into());
        let mut model = AppModel::default();
        let mut scoped = DnsServer::udp("xrose-mesh-dns-0", "192.0.2.10");
        scoped.domains = vec!["domain:private.example".into()];
        model.dns.servers = vec![scoped.clone()];
        model.dns.final_server = None;
        model.inbounds.push(crate::model::Inbound::new(
            "fixture-tun",
            crate::model::InboundKind::Tun,
            0,
        ));
        let config = XrayAdapter.compile(&model, &settings, &ctx).unwrap().json;
        assert_eq!(config["dns"]["servers"][0]["skipFallback"], true);
        assert_eq!(config["dns"]["servers"][1]["address"], "localhost");
        assert!(
            config["inbounds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["tag"] == "fixture-tun")
                .unwrap()["settings"]["dns"]
                .is_null()
        );
        assert!(
            config["routing"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["outboundTag"] == "dns-out")
                .all(|r| r["inboundTag"]
                    .as_array()
                    .expect("scope DNS interception to proxy inbounds")
                    .iter()
                    .all(|tag| tag != "fixture-tun"))
        );

        model
            .dns
            .servers
            .push(DnsServer::https("fakeip", "192.0.2.20"));
        model.dns.fake_ip = true;
        ctx.mesh.dns.push(crate::mesh::coexist::MeshDns {
            suffix: "mesh.example".into(),
            address: "127.0.0.153".into(),
        });
        let config = SingBoxAdapter
            .compile(&model, &settings, &ctx)
            .unwrap()
            .json;
        let dns = &config["dns"];
        assert_eq!(dns["final"], "fakeip"); // the real, unscoped resolver
        let servers = dns["servers"].as_array().unwrap();
        let tags: BTreeSet<_> = servers.iter().map(|s| s["tag"].as_str().unwrap()).collect();
        assert_eq!(tags.len(), servers.len());
        let mesh_tag = &config["route"]["rules"][0]["server"];
        assert_ne!(mesh_tag, "xrose-mesh-dns-0");
        assert_eq!(dns["rules"][0]["server"], *mesh_tag);
        assert!(
            servers
                .iter()
                .any(|s| s["tag"] == "fakeip" && s["type"] == "https")
        );
        assert!(
            servers
                .iter()
                .any(|s| s["tag"] != "fakeip" && s["type"] == "fakeip")
        );

        model.dns.enabled = false;
        let config = SingBoxAdapter
            .compile(&model, &settings, &ctx)
            .unwrap()
            .json;
        assert_eq!(config["dns"]["final"], "local");
        assert_eq!(config["route"]["default_domain_resolver"], "local");
        assert_eq!(config["dns"]["servers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn direct_skips_proxy_nodes_and_falls_back_to_system_dns() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::default();
        let ctx = CompileContext::new(&settings, dir.path().into(), dir.path().into());
        let mut model = AppModel::default();
        model.add_nodes([Node::new("proxy", Protocol::Http, "proxy.example", 8080)]);
        let mut upstream = DnsServer::https("remote", "192.0.2.22");
        upstream.detour = Some("proxy".into());
        model.dns.servers = vec![upstream];
        model.dns.final_server = Some("remote".into());
        let original = model.clone();
        for core in CoreKind::all() {
            let compiled = adapter_for(core).compile(&model, &settings, &ctx).unwrap();
            assert!(!compiled.json.to_string().contains("proxy.example"));
            let dns = &compiled.json["dns"];
            if core == CoreKind::SingBox {
                assert_eq!(dns["final"], "local");
                assert!(
                    dns["servers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|s| s["tag"] == "local" && s["type"] == "local")
                );
            } else {
                assert_eq!(dns["servers"], json!([{"address":"localhost"}]));
            }
        }
        assert_eq!(model, original);
        // A direct DoH with the reserved-looking name "local" still needs a distinct bootstrap.
        model
            .dns
            .servers
            .push(DnsServer::https("local", "resolver.example"));
        let config = SingBoxAdapter
            .compile(&model, &settings, &ctx)
            .unwrap()
            .json;
        let dns = &config["dns"];
        assert_eq!(dns["final"], "local");
        let encrypted = dns["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["tag"] == "local")
            .unwrap();
        assert_eq!(encrypted["type"], "https");
        assert_ne!(encrypted["domain_resolver"], "local");
        assert_eq!(
            config["route"]["default_domain_resolver"],
            encrypted["domain_resolver"]
        );
        model.work_mode = WorkMode::Proxy;
        assert!(
            SingBoxAdapter
                .compile(&model, &settings, &ctx)
                .unwrap()
                .json
                .to_string()
                .contains("proxy.example")
        );
    }
}
