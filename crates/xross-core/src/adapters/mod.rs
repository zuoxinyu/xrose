//! Compile the intent model into core-specific JSON and validate it with the
//! real core binary. All schema knowledge lives here.

pub mod singbox;
pub mod xray;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::process::run_capture;
use crate::error::Result;
use crate::model::{
    AppModel, CoreKind, DnsServer, Matchers, RouteAction, RouteActionKind, RuleSet, RuleSetFormat,
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
    } else if let Some(code) = tag.strip_prefix("geoip-") {
        format!("https://raw.githubusercontent.com/SagerNet/sing-geoip/rule-set/geoip-{code}.srs")
    } else {
        return None;
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
    if let Some(set) = model.routing.rule_sets.iter().find(|set| set.tag == tag) {
        // Repair only the old built-in URL; user mirrors retain precedence.
        if tag != "geosite-gfw"
            || set.url
                != "https://raw.githubusercontent.com/SagerNet/sing-geosite/rule-set/geosite-gfw.srs"
        {
            return Some(set.clone());
        }
    }
    default_rule_set(tag)
}

/// Includes implicit route/DNS references so manual retries match startup.
pub(crate) fn required_rule_sets(model: &AppModel) -> Vec<RuleSet> {
    let mut sets = BTreeMap::new();
    let mut add = |tag: &str| {
        if let Some(set) = resolve_rule_set(model, tag) {
            sets.entry(tag.to_string()).or_insert(set);
        }
    };
    for rule in model.routing.effective_rules() {
        for tag in &rule.matchers.rule_sets {
            add(tag);
        }
        for name in &rule.matchers.geosites {
            add(&format!("geosite-{name}"));
        }
        for code in &rule.matchers.geoips {
            add(&format!("geoip-{code}"));
        }
    }
    if model.dns.enabled {
        for server in &model.dns.servers {
            let split = split_dns_domains(&server.domains);
            for name in split.geosites {
                add(&format!("geosite-{name}"));
            }
            for tag in split.rule_sets {
                add(&tag);
            }
        }
    }
    for tag in ["geosite-cn", "geoip-cn"] {
        add(tag);
    }
    sets.into_values().collect()
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
pub(crate) fn compile_probe(
    core: CoreKind,
    node: &crate::model::Node,
    port: u16,
    front: Option<&crate::model::Node>,
) -> Result<Value> {
    if !node.usable_with(core) || front.is_some_and(|f| !f.usable_with(core)) {
        return Err(crate::Error::config("该节点未启用或当前内核不支持该协议"));
    }
    match core {
        CoreKind::SingBox => singbox::compile_probe(node, port, front),
        CoreKind::Xray => xray::compile_probe(node, port, front),
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
        RouteActionKind::Proxy => {
            if core == CoreKind::Xray {
                // Xray has no selector outbound: a selector compiles to a
                // balancer pinned to the selected node. Switching nodes goes
                // through config regeneration + core restart.
                if let Some(group_id) = model.resolve_proxy_group(action) {
                    if let Some(group) = model.group(&group_id) {
                        if group.kind == crate::model::GroupKind::Selector {
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
