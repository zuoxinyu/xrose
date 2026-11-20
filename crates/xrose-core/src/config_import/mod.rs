//! Unified configuration importer for Xrose JSON backups, Clash/Mihomo YAML,
//! sing-box JSON, and Xray JSON.
//!
//! Parsed data stays in memory until the user confirms the selected sections
//! and conflict strategy (`merge` or `replace`).

mod apply;
mod clash;
mod singbox;
mod xray;
mod xrose_backup;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::error::{Error, Result};
use crate::model::{
    AppModel, DnsConfig, DnsServerType, DomainStrategy, Matchers, Node, OutboundGroup, RouteAction,
    RoutePreset, RouteRule, RouteRuleGroup, RuleSet, WorkMode,
};
use crate::subscription::Subscription;

const MAX_BYTES: usize = 16 * 1024 * 1024;
const LARGE_RULE_COMPACT_THRESHOLD: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFormat {
    XroseBackup,
    ClashYaml,
    SingBoxJson,
    XrayJson,
}

impl ConfigFormat {
    pub fn label(&self) -> &'static str {
        match self {
            Self::XroseBackup => "Xrose 备份 JSON",
            Self::ClashYaml => "Clash / Mihomo YAML",
            Self::SingBoxJson => "sing-box JSON",
            Self::XrayJson => "Xray JSON",
        }
    }

    pub fn en_label(&self) -> &'static str {
        match self {
            Self::XroseBackup => "Xrose backup JSON",
            Self::ClashYaml => "Clash / Mihomo YAML",
            Self::SingBoxJson => "sing-box JSON",
            Self::XrayJson => "Xray JSON",
        }
    }

    pub(crate) fn import_group_name(&self) -> &'static str {
        match self {
            Self::XroseBackup => "备份导入规则",
            Self::ClashYaml => "Clash 导入规则",
            Self::SingBoxJson => "sing-box 导入规则",
            Self::XrayJson => "Xray 导入规则",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSection {
    Nodes,
    Groups,
    Routing,
    Dns,
    Subscriptions,
}

impl ImportSection {
    pub const ALL: [Self; 5] = [
        Self::Nodes,
        Self::Groups,
        Self::Routing,
        Self::Dns,
        Self::Subscriptions,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "node" | "nodes" => Some(Self::Nodes),
            "group" | "groups" | "outbound-groups" | "outbound_groups" => Some(Self::Groups),
            "route" | "routing" | "rules" => Some(Self::Routing),
            "dns" => Some(Self::Dns),
            "sub" | "subs" | "subscription" | "subscriptions" => Some(Self::Subscriptions),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    #[default]
    Merge,
    Replace,
}

impl ImportMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "merge" => Some(Self::Merge),
            "replace" | "overwrite" => Some(Self::Replace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportOptions {
    pub mode: ImportMode,
    pub sections: BTreeSet<ImportSection>,
    pub profile_index: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            mode: ImportMode::Merge,
            sections: ImportSection::ALL.into_iter().collect(),
            profile_index: 0,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionCounts {
    pub nodes: usize,
    pub groups: usize,
    pub rules: usize,
    pub rule_groups: usize,
    pub rule_sets: usize,
    pub dns_servers: usize,
    pub dns_hosts: usize,
    pub dns_fake_ip: bool,
    pub subscriptions: usize,
}

impl SectionCounts {
    pub fn has_section(&self, section: ImportSection) -> bool {
        match section {
            ImportSection::Nodes => self.nodes > 0,
            ImportSection::Groups => self.groups > 0,
            ImportSection::Routing => self.rules > 0 || self.rule_groups > 0 || self.rule_sets > 0,
            ImportSection::Dns => self.dns_servers > 0 || self.dns_hosts > 0 || self.dns_fake_ip,
            ImportSection::Subscriptions => self.subscriptions > 0,
        }
    }

    pub fn available_sections(&self) -> Vec<ImportSection> {
        ImportSection::ALL
            .into_iter()
            .filter(|section| self.has_section(*section))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportProfileSummary {
    pub name: String,
    pub counts: SectionCounts,
}

impl Default for ImportProfileSummary {
    fn default() -> Self {
        Self {
            name: String::new(),
            counts: SectionCounts::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigImportSummary {
    pub format: ConfigFormat,
    #[serde(default)]
    pub source_name: Option<String>,
    #[serde(default)]
    pub exported_at: Option<String>,
    #[serde(default)]
    pub counts: SectionCounts,
    #[serde(default)]
    pub available_sections: Vec<ImportSection>,
    #[serde(default)]
    pub profiles: Vec<ImportProfileSummary>,
    #[serde(default)]
    pub selected_profile: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigImportView {
    pub id: String,
    pub summary: ConfigImportSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportResult {
    pub format: ConfigFormat,
    pub mode: ImportMode,
    pub nodes_added: usize,
    pub nodes_skipped: usize,
    pub groups_applied: usize,
    pub rules_applied: usize,
    pub rule_groups_applied: usize,
    pub rule_sets_applied: usize,
    pub dns_applied: bool,
    pub subscriptions_added: usize,
    pub subscriptions_skipped: usize,
    pub warnings: Vec<String>,
}

impl Default for ImportResult {
    fn default() -> Self {
        Self {
            format: ConfigFormat::XroseBackup,
            mode: ImportMode::Merge,
            nodes_added: 0,
            nodes_skipped: 0,
            groups_applied: 0,
            rules_applied: 0,
            rule_groups_applied: 0,
            rule_sets_applied: 0,
            dns_applied: false,
            subscriptions_added: 0,
            subscriptions_skipped: 0,
            warnings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedRouting {
    pub preset: RoutePreset,
    pub rules: Vec<RouteRule>,
    pub rule_groups: Vec<RouteRuleGroup>,
    pub final_action: Option<RouteAction>,
    pub rule_sets: Vec<RuleSet>,
    pub domain_strategy: Option<DomainStrategy>,
    pub rule_set_source: Option<crate::model::RuleSetSource>,
    pub custom_rule_set_template: Option<String>,
    /// True when the source already organizes rules into Xrose's native structure.
    pub preserve_structure: bool,
}

impl Default for ParsedRouting {
    fn default() -> Self {
        Self {
            preset: RoutePreset::Custom,
            rules: Vec::new(),
            rule_groups: Vec::new(),
            final_action: None,
            rule_sets: Vec::new(),
            domain_strategy: None,
            rule_set_source: None,
            custom_rule_set_template: None,
            preserve_structure: false,
        }
    }
}

impl ParsedRouting {
    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
            && self.rule_groups.is_empty()
            && self.rule_sets.is_empty()
            && self.final_action.is_none()
    }

    pub(crate) fn rule_count(&self) -> usize {
        self.rules.len()
            + self
                .rule_groups
                .iter()
                .map(|g| g.rules.len())
                .sum::<usize>()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ParsedProfile {
    pub name: String,
    pub work_mode: Option<WorkMode>,
    pub nodes: Vec<Node>,
    pub groups: Vec<OutboundGroup>,
    pub default_group: Option<String>,
    pub routing: Option<ParsedRouting>,
    pub dns: Option<DnsConfig>,
    pub warnings: Vec<String>,
}

impl ParsedProfile {
    pub(crate) fn counts(&self, subscriptions: usize) -> SectionCounts {
        let (rules, rule_groups, rule_sets) = match &self.routing {
            Some(routing) => (
                routing.rule_count(),
                routing.rule_groups.len(),
                routing.rule_sets.len(),
            ),
            None => (0, 0, 0),
        };
        let (dns_servers, dns_hosts, dns_fake_ip) = match &self.dns {
            Some(dns) => (
                dns.servers.iter().filter(|s| !s.is_fakeip()).count(),
                dns.hosts.len(),
                dns.fake_ip,
            ),
            None => (0, 0, false),
        };
        SectionCounts {
            nodes: self.nodes.len(),
            groups: self.groups.len(),
            rules,
            rule_groups,
            rule_sets,
            dns_servers,
            dns_hosts,
            dns_fake_ip,
            subscriptions,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedConfig {
    pub format: ConfigFormat,
    pub exported_at: Option<String>,
    pub profiles: Vec<ParsedProfile>,
    pub subscriptions: Vec<Subscription>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedConfigImport {
    pub(crate) parsed: ParsedConfig,
    pub(crate) source_name: Option<String>,
    pub(crate) selected_profile: usize,
}

impl PreparedConfigImport {
    pub fn summary(&self) -> ConfigImportSummary {
        self.summary_for(self.selected_profile)
    }

    pub fn select_profile(&mut self, index: usize) -> Result<ConfigImportSummary> {
        if self.parsed.profiles.is_empty() {
            if index != 0 {
                return Err(Error::config("所选配置档超出范围"));
            }
            self.selected_profile = 0;
            return Ok(self.summary());
        }
        if index >= self.parsed.profiles.len() {
            return Err(Error::config("所选配置档超出范围"));
        }
        self.selected_profile = index;
        Ok(self.summary())
    }

    pub fn summary_for(&self, profile_index: usize) -> ConfigImportSummary {
        let sub_count = self.parsed.subscriptions.len();
        let profiles: Vec<ImportProfileSummary> = self
            .parsed
            .profiles
            .iter()
            .map(|profile| ImportProfileSummary {
                name: profile.name.clone(),
                counts: profile.counts(sub_count),
            })
            .collect();
        let selected_profile = if profiles.is_empty() {
            0
        } else {
            profile_index.min(profiles.len() - 1)
        };
        let counts = self
            .parsed
            .profiles
            .get(selected_profile)
            .map(|p| p.counts(sub_count))
            .unwrap_or(SectionCounts {
                subscriptions: sub_count,
                ..Default::default()
            });
        let mut warnings = self.parsed.warnings.clone();
        if let Some(profile) = self.parsed.profiles.get(selected_profile) {
            warnings.extend(profile.warnings.iter().cloned());
        }
        dedup_warnings(&mut warnings);
        ConfigImportSummary {
            format: self.parsed.format,
            source_name: self.source_name.clone(),
            exported_at: self.parsed.exported_at.clone(),
            available_sections: counts.available_sections(),
            counts,
            profiles,
            selected_profile,
            warnings,
        }
    }

    pub fn apply(
        &self,
        model: &mut AppModel,
        subscriptions: &mut Vec<Subscription>,
        options: &ImportOptions,
    ) -> Result<ImportResult> {
        apply::apply_import(&self.parsed, model, subscriptions, options)
    }
}

pub fn read_text(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::config("读取配置文件失败，请检查文件权限"))?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::config("配置文件超过 16 MiB，请精简后重试"));
    }
    String::from_utf8(bytes).map_err(|_| Error::config("配置文件需为 UTF-8 文本"))
}

pub fn read_file(path: &Path) -> Result<String> {
    read_text(
        std::fs::File::open(path).map_err(|_| Error::config("无法打开配置文件，请检查文件权限"))?,
    )
}

/// Detects the configuration format and prepares an in-memory import plan.
pub fn prepare(text: &str, source_name: Option<String>) -> Result<PreparedConfigImport> {
    if text.len() > MAX_BYTES {
        return Err(Error::config("配置文件超过 16 MiB，请精简后重试"));
    }
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(Error::config("配置文件内容为空"));
    }

    let parsed = if trimmed.starts_with('{') {
        let stripped = strip_json_comments(trimmed);
        let value: JsonValue = serde_json::from_str(&stripped).map_err(|err| {
            Error::config(format!(
                "第 {} 行：不是有效的 JSON 配置文件，请检查语法",
                err.line()
            ))
        })?;
        parse_json_config(&value)?
    } else {
        let root: serde_yaml::Value = serde_yaml::from_str(trimmed)
            .map_err(|err| Error::config(format!("配置文件解析失败：{err}")))?;
        if looks_like_clash_yaml(&root) {
            clash::parse(&root)?
        } else {
            return Err(Error::config(
                "无法识别配置文件格式，支持 Xrose 备份 JSON、Clash / Mihomo YAML、sing-box JSON 与 Xray JSON",
            ));
        }
    };

    let mut prepared = PreparedConfigImport {
        parsed,
        source_name,
        selected_profile: 0,
    };
    let summary = prepared.summary();
    if summary.available_sections.is_empty() {
        return Err(Error::config(
            "配置文件中没有可导入的节点、出站组、路由规则、DNS 或订阅",
        ));
    }
    if prepared.parsed.profiles.is_empty() {
        prepared.selected_profile = 0;
    }
    Ok(prepared)
}

fn parse_json_config(value: &JsonValue) -> Result<ParsedConfig> {
    let Some(obj) = value.as_object() else {
        return Err(Error::config("配置文件顶层必须是 JSON 对象"));
    };

    if is_xrose_backup(obj) {
        return xrose_backup::parse(value);
    }
    if obj.contains_key("proxies") || obj.contains_key("proxy-groups") {
        let yaml_val: serde_yaml::Value = serde_json::from_value(value.clone())
            .map_err(|err| Error::config(format!("Clash 配置解析失败：{err}")))?;
        return clash::parse(&yaml_val);
    }
    match detect_core_json_format(obj) {
        Some(ConfigFormat::SingBoxJson) => singbox::parse(value),
        Some(ConfigFormat::XrayJson) => xray::parse(value),
        _ => Err(Error::config(
            "无法识别该 JSON 配置文件，支持 Xrose 备份 JSON、sing-box JSON 与 Xray JSON",
        )),
    }
}

fn is_xrose_backup(obj: &serde_json::Map<String, JsonValue>) -> bool {
    obj.get("format").is_some_and(JsonValue::is_u64)
        && (obj.get("profiles").is_some_and(JsonValue::is_array)
            || obj.get("subscriptions").is_some_and(JsonValue::is_array))
        && !obj.contains_key("outbounds")
        && !obj.contains_key("route")
        && !obj.contains_key("routing")
}

fn detect_core_json_format(obj: &serde_json::Map<String, JsonValue>) -> Option<ConfigFormat> {
    let has_xray_outbound = obj
        .get("outbounds")
        .and_then(JsonValue::as_array)
        .is_some_and(|list| list.iter().any(|item| item.get("protocol").is_some()));
    let has_singbox_outbound = obj
        .get("outbounds")
        .and_then(JsonValue::as_array)
        .is_some_and(|list| list.iter().any(|item| item.get("type").is_some()));
    let has_xray_inbound = obj
        .get("inbounds")
        .and_then(JsonValue::as_array)
        .is_some_and(|list| list.iter().any(|item| item.get("protocol").is_some()));
    let has_singbox_inbound = obj
        .get("inbounds")
        .and_then(JsonValue::as_array)
        .is_some_and(|list| list.iter().any(|item| item.get("type").is_some()));

    if has_xray_outbound || has_xray_inbound || obj.contains_key("routing") {
        if !has_singbox_outbound && !obj.contains_key("route") && !obj.contains_key("endpoints") {
            return Some(ConfigFormat::XrayJson);
        }
    }
    if has_singbox_outbound
        || has_singbox_inbound
        || obj.contains_key("route")
        || obj.contains_key("endpoints")
    {
        return Some(ConfigFormat::SingBoxJson);
    }
    if let Some(dns) = obj.get("dns").and_then(JsonValue::as_object) {
        if dns.contains_key("rules") || dns.contains_key("final") || dns.contains_key("fakeip") {
            return Some(ConfigFormat::SingBoxJson);
        }
        if let Some(servers) = dns.get("servers").and_then(JsonValue::as_array) {
            if servers.iter().any(|s| s.get("type").is_some()) {
                return Some(ConfigFormat::SingBoxJson);
            }
            if servers
                .iter()
                .any(|s| s.is_string() || s.get("address").is_some())
            {
                return Some(ConfigFormat::XrayJson);
            }
        }
        if dns.contains_key("hosts") {
            return Some(ConfigFormat::XrayJson);
        }
    }
    None
}

fn looks_like_clash_yaml(root: &serde_yaml::Value) -> bool {
    let Some(map) = root.as_mapping() else {
        return false;
    };
    const CLASH_KEYS: [&str; 7] = [
        "proxies",
        "proxy-groups",
        "rules",
        "rule-providers",
        "proxy-providers",
        "dns",
        "hosts",
    ];
    CLASH_KEYS
        .iter()
        .any(|key| map.contains_key(serde_yaml::Value::String((*key).to_string())))
}

/// Strips `//` line comments and `/* ... */` block comments outside JSON strings.
pub(crate) fn strip_json_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_string = false;
            }
            let c = input[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        if ch == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        let c = input[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

pub(crate) fn safe_rule_set_url(raw: &str) -> bool {
    url::Url::parse(raw.trim()).ok().is_some_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.has_host()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
    })
}

pub(crate) fn safe_http_subscription_url(raw: &str) -> bool {
    url::Url::parse(raw.trim())
        .ok()
        .is_some_and(|u| matches!(u.scheme(), "http" | "https") && u.has_host())
}

pub(crate) fn parse_duration_secs(raw: &str) -> Option<u64> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Some(secs);
    }
    let mut total = 0u64;
    let mut num = String::new();
    let mut chars = s.chars().peekable();
    let mut matched = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        if num.is_empty() {
            return None;
        }
        let value: u64 = num.parse().ok()?;
        num.clear();
        let unit = if c == 'm' && chars.peek() == Some(&'s') {
            chars.next();
            "ms"
        } else {
            match c {
                's' => "s",
                'm' => "m",
                'h' => "h",
                'd' => "d",
                _ => return None,
            }
        };
        matched = true;
        total = total.saturating_add(match unit {
            "ms" => value / 1000,
            "s" => value,
            "m" => value.saturating_mul(60),
            "h" => value.saturating_mul(3600),
            "d" => value.saturating_mul(86400),
            _ => 0,
        });
    }
    if !num.is_empty() || !matched {
        return None;
    }
    Some(total)
}

pub(crate) fn parse_duration_hours(raw: &str) -> Option<u64> {
    parse_duration_secs(raw).map(|secs| (secs / 3600).max(1))
}

pub(crate) fn normalize_ip_cidr(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(net) = trimmed.parse::<ipnet::IpNet>() {
        return Some(net.to_string());
    }
    if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
        return Some(match ip {
            std::net::IpAddr::V4(v4) => format!("{v4}/32"),
            std::net::IpAddr::V6(v6) => format!("{v6}/128"),
        });
    }
    None
}

/// Parses a DNS upstream specification from Clash, sing-box legacy, or Xray into
/// `(DnsServerType, address, optional_detour)`.
pub(crate) fn parse_dns_address(raw: &str) -> Option<(DnsServerType, String, Option<String>)> {
    let mut value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let mut detour = None;
    if let Some((before, fragment)) = value.split_once('#') {
        value = before.trim();
        let frag = fragment.trim();
        if !frag.is_empty() && !frag.contains('=') {
            detour = Some(frag.to_string());
        } else if let Some(first) = frag.split('&').next() {
            if !first.contains('=') && !first.is_empty() {
                detour = Some(first.to_string());
            }
        }
    }
    if value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "system" | "local" | "localhost" | "dhcp://system"
    ) {
        return Some((
            DnsServerType::Udp,
            "system".into(),
            detour.or_else(|| Some("direct".into())),
        ));
    }
    if matches!(lower.as_str(), "fakeip" | "fakedns") {
        return Some((DnsServerType::FakeIp, "fakeip".into(), None));
    }
    if lower.starts_with("rcode://") || lower.starts_with("dhcp://") {
        return None;
    }
    if let Some(rest) = value.strip_prefix("https+local://") {
        return Some((
            DnsServerType::Https,
            format!("https://{rest}"),
            Some("direct".into()),
        ));
    }
    if let Some(rest) = value.strip_prefix("tls+local://") {
        return Some((DnsServerType::Tls, rest.to_string(), Some("direct".into())));
    }
    if let Some(rest) = value.strip_prefix("quic+local://") {
        return Some((DnsServerType::Quic, rest.to_string(), Some("direct".into())));
    }
    if let Some(rest) = value.strip_prefix("tcp+local://") {
        return Some((DnsServerType::Tcp, rest.to_string(), Some("direct".into())));
    }
    if value.starts_with("https://") {
        return Some((DnsServerType::Https, value.to_string(), detour));
    }
    if let Some(rest) = value.strip_prefix("h3://") {
        return Some((DnsServerType::H3, rest.to_string(), detour));
    }
    if let Some(rest) = value.strip_prefix("quic://") {
        return Some((DnsServerType::Quic, rest.to_string(), detour));
    }
    if let Some(rest) = value.strip_prefix("tls://") {
        return Some((DnsServerType::Tls, rest.to_string(), detour));
    }
    if let Some(rest) = value.strip_prefix("tcp://") {
        return Some((DnsServerType::Tcp, rest.to_string(), detour));
    }
    if let Some(rest) = value.strip_prefix("udp://") {
        return Some((DnsServerType::Udp, rest.to_string(), detour));
    }
    Some((DnsServerType::Udp, value.to_string(), detour))
}

/// Flattens nested group references into ordered unique node IDs while detecting cycles.
pub(crate) fn expand_group_members(
    group_name: &str,
    raw_groups: &BTreeMap<String, Vec<String>>,
    node_ids: &BTreeMap<String, String>,
    visited: &mut BTreeSet<String>,
    out: &mut Vec<String>,
    seen_nodes: &mut BTreeSet<String>,
    flattened: &mut bool,
) {
    if !visited.insert(group_name.to_string()) {
        return;
    }
    if let Some(members) = raw_groups.get(group_name) {
        for member in members {
            if let Some(node_id) = node_ids.get(member) {
                if seen_nodes.insert(node_id.clone()) {
                    out.push(node_id.clone());
                }
            } else if raw_groups.contains_key(member) {
                *flattened = true;
                expand_group_members(
                    member, raw_groups, node_ids, visited, out, seen_nodes, flattened,
                );
            }
        }
    }
    visited.remove(group_name);
}

pub(crate) fn dedup_warnings(warnings: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    warnings.retain(|w| !w.trim().is_empty() && seen.insert(w.clone()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SingleMatcherField {
    Domains,
    DomainSuffixes,
    DomainKeywords,
    DomainRegexes,
    Geosites,
    IpCidrs,
    Geoips,
    SourceIpCidrs,
    Ports,
    SourcePorts,
    ProcessNames,
    ProcessPaths,
    RuleSets,
}

fn single_matcher_field(m: &Matchers) -> Option<SingleMatcherField> {
    if m.network.is_some() || !m.inbound_tags.is_empty() || !m.protocols.is_empty() {
        return None;
    }
    let flags = [
        (!m.domains.is_empty(), SingleMatcherField::Domains),
        (
            !m.domain_suffixes.is_empty(),
            SingleMatcherField::DomainSuffixes,
        ),
        (
            !m.domain_keywords.is_empty(),
            SingleMatcherField::DomainKeywords,
        ),
        (
            !m.domain_regexes.is_empty(),
            SingleMatcherField::DomainRegexes,
        ),
        (!m.geosites.is_empty(), SingleMatcherField::Geosites),
        (!m.ip_cidrs.is_empty(), SingleMatcherField::IpCidrs),
        (!m.geoips.is_empty(), SingleMatcherField::Geoips),
        (
            !m.source_ip_cidrs.is_empty(),
            SingleMatcherField::SourceIpCidrs,
        ),
        (!m.ports.is_empty(), SingleMatcherField::Ports),
        (!m.source_ports.is_empty(), SingleMatcherField::SourcePorts),
        (
            !m.process_names.is_empty(),
            SingleMatcherField::ProcessNames,
        ),
        (
            !m.process_paths.is_empty(),
            SingleMatcherField::ProcessPaths,
        ),
        (!m.rule_sets.is_empty(), SingleMatcherField::RuleSets),
    ];
    let mut found = None;
    for (present, field) in flags {
        if present {
            if found.is_some() {
                return None;
            }
            found = Some(field);
        }
    }
    found
}

fn matcher_field_mut(m: &mut Matchers, field: SingleMatcherField) -> &mut Vec<String> {
    match field {
        SingleMatcherField::Domains => &mut m.domains,
        SingleMatcherField::DomainSuffixes => &mut m.domain_suffixes,
        SingleMatcherField::DomainKeywords => &mut m.domain_keywords,
        SingleMatcherField::DomainRegexes => &mut m.domain_regexes,
        SingleMatcherField::Geosites => &mut m.geosites,
        SingleMatcherField::IpCidrs => &mut m.ip_cidrs,
        SingleMatcherField::Geoips => &mut m.geoips,
        SingleMatcherField::SourceIpCidrs => &mut m.source_ip_cidrs,
        SingleMatcherField::Ports => &mut m.ports,
        SingleMatcherField::SourcePorts => &mut m.source_ports,
        SingleMatcherField::ProcessNames => &mut m.process_names,
        SingleMatcherField::ProcessPaths => &mut m.process_paths,
        SingleMatcherField::RuleSets => &mut m.rule_sets,
    }
}

fn field_label(field: SingleMatcherField) -> &'static str {
    match field {
        SingleMatcherField::Domains => "域名规则",
        SingleMatcherField::DomainSuffixes => "域名后缀规则",
        SingleMatcherField::DomainKeywords => "域名关键词规则",
        SingleMatcherField::DomainRegexes => "域名正则规则",
        SingleMatcherField::Geosites => "Geosite 规则",
        SingleMatcherField::IpCidrs => "IP 网段规则",
        SingleMatcherField::Geoips => "GeoIP 规则",
        SingleMatcherField::SourceIpCidrs => "源 IP 规则",
        SingleMatcherField::Ports => "目标端口规则",
        SingleMatcherField::SourcePorts => "源端口规则",
        SingleMatcherField::ProcessNames => "进程名规则",
        SingleMatcherField::ProcessPaths => "进程路径规则",
        SingleMatcherField::RuleSets => "规则集规则",
    }
}

/// Losslessly merges adjacent single-dimension rules sharing the same action when
/// a foreign config contains a large flat list of individual rules.
pub(crate) fn compact_rules_if_large(rules: Vec<RouteRule>) -> Vec<RouteRule> {
    if rules.len() <= LARGE_RULE_COMPACT_THRESHOLD {
        return rules;
    }
    let mut compacted: Vec<(RouteRule, Option<SingleMatcherField>, usize)> = Vec::new();
    for rule in rules {
        let field = single_matcher_field(&rule.matchers);
        if let (Some(field), Some((prev, Some(prev_field), merged_count))) =
            (field, compacted.last_mut())
            && *prev_field == field
            && prev.enabled == rule.enabled
            && prev.action == rule.action
        {
            let target = matcher_field_mut(&mut prev.matchers, field);
            let mut incoming = rule.matchers;
            for item in matcher_field_mut(&mut incoming, field).drain(..) {
                if !target.contains(&item) {
                    target.push(item);
                }
            }
            *merged_count += 1;
            prev.name = format!("{}（{} 项）", field_label(field), target.len());
            continue;
        }
        compacted.push((rule, field, 1));
    }
    compacted.into_iter().map(|(rule, _, _)| rule).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{CompileContext, CoreAdapter, SingBoxAdapter, XrayAdapter};
    use crate::model::{CoreKind, Protocol};
    use crate::settings::Settings;

    #[test]
    fn strips_jsonc_comments_without_touching_urls_inside_strings() {
        let raw = r#"
        {
            // line comment
            "url": "https://example.com/a//b/*c*/",
            /* block
               comment */
            "ok": true
        }
        "#;
        let parsed: JsonValue = serde_json::from_str(&strip_json_comments(raw)).unwrap();
        assert_eq!(parsed["url"], "https://example.com/a//b/*c*/");
        assert_eq!(parsed["ok"], true);
    }

    #[test]
    fn imports_clash_yaml_and_compiles_for_both_cores() {
        let yaml = r#"
proxies:
  - name: HK-01
    type: vless
    server: hk.example.com
    port: 443
    uuid: 11111111-2222-3333-4444-555555555555
    tls: true
    servername: hk.example.com
    network: ws
    ws-opts:
      path: /ws
      headers:
        Host: hk.example.com
  - name: JP-01
    type: trojan
    server: jp.example.com
    port: 443
    password: secret-password
    sni: jp.example.com
proxy-groups:
  - name: 自动选择
    type: url-test
    url: https://www.gstatic.com/generate_204
    interval: 180
    tolerance: 80
    proxies:
      - HK-01
      - JP-01
  - name: 节点选择
    type: select
    proxies:
      - 自动选择
      - HK-01
      - DIRECT
proxy-providers:
  provider-a:
    type: http
    url: https://sub.example.com/clash.yaml
    interval: 3600
rules:
  - DOMAIN-SUFFIX,local.example,DIRECT
  - IP-CIDR,192.0.2.0/24,DIRECT
  - DOMAIN,ads.example,REJECT
  - GEOSITE,cn,DIRECT
  - DOMAIN-SUFFIX,ai.example,自动选择
  - MATCH,节点选择
dns:
  enable: true
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
    - "*.lan"
  nameserver:
    - https://1.1.1.1/dns-query
    - 223.5.5.5
  nameserver-policy:
    "geosite:cn": 223.5.5.5
hosts:
  nas.example: 192.0.2.10
"#;
        let prepared = prepare(yaml, Some("clash.yaml".into())).unwrap();
        let summary = prepared.summary();
        assert_eq!(summary.format, ConfigFormat::ClashYaml);
        assert_eq!(summary.counts.nodes, 2);
        assert_eq!(summary.counts.groups, 2);
        assert_eq!(summary.counts.rules, 5);
        assert_eq!(summary.counts.subscriptions, 1);
        assert!(summary.counts.dns_fake_ip);
        assert_eq!(summary.counts.dns_hosts, 1);

        let mut model = AppModel::default();
        model.work_mode = WorkMode::Proxy;
        let mut subs = Vec::new();
        let result = prepared
            .apply(
                &mut model,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(result.nodes_added, 2);
        assert_eq!(result.groups_applied, 2);
        assert_eq!(result.subscriptions_added, 1);
        assert!(result.dns_applied);
        assert_eq!(model.routing.preset, RoutePreset::Custom);
        assert!(crate::dns::validate(&model.dns).is_empty());

        let settings = Settings::default();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompileContext::new(
            &settings,
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
        XrayAdapter.compile(&model, &settings, &ctx).unwrap();
    }

    #[test]
    fn imports_singbox_json_with_groups_rules_and_typed_dns() {
        let json = r#"
        {
          // sing-box 1.12+ config
          "dns": {
            "servers": [
              {"type": "local", "tag": "local"},
              {"type": "https", "tag": "cf", "server": "1.1.1.1", "detour": "proxy-select"},
              {"type": "udp", "tag": "cn", "server": "223.5.5.5"},
              {"type": "fakeip", "tag": "fakeip", "inet4_range": "198.18.0.0/15"},
              {"type": "hosts", "tag": "hosts", "predefined": {"app.example": ["192.0.2.5"]}}
            ],
            "rules": [
              {"rule_set": ["geosite-cn"], "action": "route", "server": "cn"}
            ],
            "final": "cf"
          },
          "outbounds": [
            {"type": "vless", "tag": "hk-node", "server": "hk.example.com", "server_port": 443, "uuid": "11111111-2222-3333-4444-555555555555", "tls": {"enabled": true, "server_name": "hk.example.com"}},
            {"type": "hysteria2", "tag": "us-hy2", "server": "us.example.com", "server_port": 443, "password": "pw", "tls": {"enabled": true, "server_name": "us.example.com"}},
            {"type": "urltest", "tag": "auto", "outbounds": ["hk-node", "us-hy2"], "interval": "3m", "tolerance": 60},
            {"type": "selector", "tag": "proxy-select", "outbounds": ["auto", "hk-node", "us-hy2"], "default": "hk-node"},
            {"type": "direct", "tag": "direct"}
          ],
          "route": {
            "rule_set": [
              {"type": "remote", "tag": "custom-ads", "format": "binary", "url": "https://example.com/ads.srs", "download_detour": "proxy-select", "update_interval": "24h"}
            ],
            "rules": [
              {"action": "sniff"},
              {"protocol": "dns", "action": "hijack-dns"},
              {"ip_is_private": true, "outbound": "direct"},
              {"rule_set": ["custom-ads"], "action": "reject"},
              {"domain_suffix": ["work.example"], "outbound": "auto"}
            ],
            "final": "proxy-select"
          }
        }
        "#;
        let prepared = prepare(json, Some("sing-box.json".into())).unwrap();
        let summary = prepared.summary();
        assert_eq!(summary.format, ConfigFormat::SingBoxJson);
        assert_eq!(summary.counts.nodes, 2);
        assert_eq!(summary.counts.groups, 2);
        assert_eq!(summary.counts.rules, 3);
        assert_eq!(summary.counts.rule_sets, 1);
        assert_eq!(summary.counts.dns_hosts, 1);
        assert!(summary.counts.dns_fake_ip);

        let mut model = AppModel::default();
        model.work_mode = WorkMode::Proxy;
        let mut subs = Vec::new();
        let result = prepared
            .apply(
                &mut model,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(result.nodes_added, 2);
        assert_eq!(result.rule_sets_applied, 1);
        assert!(crate::dns::validate(&model.dns).is_empty());

        let settings = Settings::default();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompileContext::new(
            &settings,
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
    }

    #[test]
    fn imports_xray_json_with_balancers_rules_and_dns() {
        let json = r#"
        {
          "dns": {
            "hosts": {
              "intra.example": "192.0.2.20"
            },
            "servers": [
              "https://1.1.1.1/dns-query",
              {"address": "223.5.5.5", "port": 53, "domains": ["geosite:cn", "domain:cn.example"]},
              "localhost"
            ]
          },
          "outbounds": [
            {
              "tag": "node-a",
              "protocol": "vless",
              "settings": {"vnext": [{"address": "a.example.com", "port": 443, "users": [{"id": "11111111-2222-3333-4444-555555555555", "flow": "xtls-rprx-vision"}]}]},
              "streamSettings": {"network": "tcp", "security": "reality", "realitySettings": {"serverName": "cdn.example.com", "fingerprint": "chrome", "publicKey": "pubkey-demo", "shortId": "abcd"}}
            },
            {
              "tag": "node-b",
              "protocol": "shadowsocks",
              "settings": {"servers": [{"address": "b.example.com", "port": 8388, "method": "aes-256-gcm", "password": "pw"}]}
            },
            {"tag": "direct", "protocol": "freedom", "settings": {}},
            {"tag": "block", "protocol": "blackhole", "settings": {}}
          ],
          "routing": {
            "domainStrategy": "IPIfNonMatch",
            "balancers": [
              {"tag": "auto-pool", "selector": ["node-"], "strategy": {"type": "leastPing"}}
            ],
            "rules": [
              {"type": "field", "inboundTag": ["api"], "outboundTag": "api"},
              {"type": "field", "domain": ["geosite:category-ads-all"], "outboundTag": "block"},
              {"type": "field", "ip": ["geoip:private", "192.0.2.0/24"], "outboundTag": "direct"},
              {"type": "field", "domain": ["full:exact.example", "domain:suffix.example"], "balancerTag": "auto-pool"},
              {"type": "field", "network": "tcp,udp", "balancerTag": "auto-pool"}
            ]
          }
        }
        "#;
        let prepared = prepare(json, Some("xray.json".into())).unwrap();
        let summary = prepared.summary();
        assert_eq!(summary.format, ConfigFormat::XrayJson);
        assert_eq!(summary.counts.nodes, 2);
        assert_eq!(summary.counts.groups, 1);
        assert_eq!(summary.counts.rules, 3);
        assert_eq!(summary.counts.dns_hosts, 1);

        let mut model = AppModel::default();
        model.work_mode = WorkMode::Proxy;
        let mut subs = Vec::new();
        prepared
            .apply(
                &mut model,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(model.nodes[0].protocol, Protocol::Vless);
        assert!(model.nodes[0].tls.reality.is_some());
        assert_eq!(model.routing.domain_strategy, DomainStrategy::IpIfNonMatch);
        assert!(crate::dns::validate(&model.dns).is_empty());

        let settings = Settings::default();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompileContext::new(
            &settings,
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
        );
        XrayAdapter.compile(&model, &settings, &ctx).unwrap();
        SingBoxAdapter.compile(&model, &settings, &ctx).unwrap();
    }

    #[test]
    fn imports_xrose_backup_with_merge_and_replace_preserving_mesh_gateway() {
        let mut backup_model = AppModel::new("导出配置");
        backup_model.work_mode = WorkMode::Proxy;
        let n1 = Node::new("备份节点", Protocol::Socks, "backup.example", 1080);
        backup_model.add_nodes([n1]);
        let bundle = crate::storage::BackupBundle {
            format: 1,
            exported_at: "2026-11-20T00:00:00Z".into(),
            profiles: vec![crate::storage::BackupProfile {
                id: "p1".into(),
                name: "导出配置".into(),
                model_json: serde_json::to_value(&backup_model).unwrap(),
            }],
            subscriptions: vec![crate::storage::BackupSubscription {
                name: "我的订阅".into(),
                url: "https://sub.example.com/feed".into(),
                user_agent: String::new(),
                headers: BTreeMap::new(),
            }],
        };
        let raw = serde_json::to_string_pretty(&bundle).unwrap();
        let prepared = prepare(&raw, Some("xrose-export.json".into())).unwrap();
        assert_eq!(prepared.summary().format, ConfigFormat::XroseBackup);

        let mut current = AppModel::default();
        let mut mesh_node = Node::new("Mesh Exit", Protocol::Socks, "127.0.0.1", 19001);
        mesh_node.id = "mesh-exit-test".into();
        mesh_node.tags.push("mesh-exit".into());
        current.add_nodes([mesh_node]);
        let existing = Node::new("现有节点", Protocol::Socks, "existing.example", 1080);
        current.add_nodes([existing]);
        let mut subs = vec![Subscription::new(
            "已有订阅",
            "https://sub.example.com/feed",
        )];

        // Merge skips duplicate subscription URL and keeps existing nodes.
        let merged = prepared
            .apply(
                &mut current,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Merge,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(merged.nodes_added, 1);
        assert_eq!(merged.subscriptions_added, 0);
        assert_eq!(merged.subscriptions_skipped, 1);
        assert_eq!(current.nodes.len(), 3);

        // Replace drops ordinary proxy nodes but preserves the local mesh gateway.
        let replaced = prepared
            .apply(
                &mut current,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: ImportSection::ALL.into_iter().collect(),
                    profile_index: 0,
                },
            )
            .unwrap();
        assert_eq!(replaced.nodes_added, 1);
        assert_eq!(current.nodes.len(), 2);
        assert!(current.nodes.iter().any(|n| n.is_mesh_gateway()));
        assert!(current.nodes.iter().any(|n| n.name == "备份节点"));
        assert!(current.work_mode == WorkMode::Proxy);
        assert!(CoreKind::SingBox.capabilities().native_selector);
    }

    #[test]
    fn rejects_empty_or_unrecognized_input() {
        assert!(prepare("   ", None).is_err());
        assert!(prepare("hello world", None).is_err());
        assert!(prepare("{\"unrelated\": 1}", None).is_err());
    }

    #[test]
    fn compacts_large_flat_rule_lists_preserving_order_boundaries() {
        let mut lines = String::from(
            "proxies:\n  - {name: N1, type: socks5, server: a.example, port: 1080}\nrules:\n",
        );
        for i in 0..40 {
            lines.push_str(&format!("  - DOMAIN-SUFFIX,d{i}.example,DIRECT\n"));
        }
        lines.push_str("  - DOMAIN,override.example,REJECT\n");
        for i in 40..70 {
            lines.push_str(&format!("  - DOMAIN-SUFFIX,d{i}.example,DIRECT\n"));
        }
        lines.push_str("  - MATCH,PROXY\n");

        let prepared = prepare(&lines, None).unwrap();
        let routing = prepared.parsed.profiles[0].routing.as_ref().unwrap();
        // 40 contiguous direct suffixes -> 1 rule, 1 reject domain -> 1 rule, 30 direct suffixes -> 1 rule.
        assert_eq!(routing.rules.len(), 3);
        assert_eq!(routing.rules[0].matchers.domain_suffixes.len(), 40);
        assert_eq!(routing.rules[1].matchers.domains, ["override.example"]);
        assert_eq!(routing.rules[2].matchers.domain_suffixes.len(), 30);
    }

    #[test]
    fn filters_unsafe_rule_set_urls_and_local_providers_with_warnings() {
        let json = r#"{
            "outbounds": [{"type": "socks", "tag": "s1", "server": "s.example", "server_port": 1080}],
            "route": {
                "rule_set": [
                    {"type": "local", "tag": "local-set", "format": "binary", "path": "/etc/passwd"},
                    {"type": "remote", "tag": "bad-query", "format": "binary", "url": "https://example.com/a.srs?token=123"},
                    {"type": "remote", "tag": "good-set", "format": "binary", "url": "https://example.com/good.srs"}
                ],
                "rules": [
                    {"rule_set": ["local-set"], "outbound": "direct"},
                    {"rule_set": ["good-set"], "outbound": "direct"}
                ]
            }
        }"#;
        let prepared = prepare(json, None).unwrap();
        let summary = prepared.summary();
        assert_eq!(summary.counts.rule_sets, 1);
        assert_eq!(summary.counts.rules, 1);
        assert!(!summary.warnings.is_empty());
        assert!(summary.warnings.iter().all(|w| !w.contains("token=123")));
    }

    #[test]
    fn supports_multi_profile_xrose_backup_selection() {
        let mut p1 = AppModel::new("配置 A");
        p1.add_nodes([Node::new("A1", Protocol::Socks, "a.example", 1080)]);
        let mut p2 = AppModel::new("配置 B");
        p2.add_nodes([
            Node::new("B1", Protocol::Socks, "b1.example", 1080),
            Node::new("B2", Protocol::Socks, "b2.example", 1080),
        ]);
        let bundle = crate::storage::BackupBundle {
            format: 1,
            exported_at: "2026-11-20T00:00:00Z".into(),
            profiles: vec![
                crate::storage::BackupProfile {
                    id: "p1".into(),
                    name: "配置 A".into(),
                    model_json: serde_json::to_value(&p1).unwrap(),
                },
                crate::storage::BackupProfile {
                    id: "p2".into(),
                    name: "配置 B".into(),
                    model_json: serde_json::to_value(&p2).unwrap(),
                },
            ],
            subscriptions: Vec::new(),
        };
        let mut prepared = prepare(&serde_json::to_string(&bundle).unwrap(), None).unwrap();
        assert_eq!(prepared.summary().profiles.len(), 2);
        assert_eq!(prepared.summary().counts.nodes, 1);
        let second = prepared.select_profile(1).unwrap();
        assert_eq!(second.selected_profile, 1);
        assert_eq!(second.counts.nodes, 2);

        let mut target = AppModel::default();
        let mut subs = Vec::new();
        let res = prepared
            .apply(
                &mut target,
                &mut subs,
                &ImportOptions {
                    mode: ImportMode::Replace,
                    sections: [ImportSection::Nodes].into_iter().collect(),
                    profile_index: 1,
                },
            )
            .unwrap();
        assert_eq!(res.nodes_added, 2);
        assert_eq!(target.nodes.len(), 2);
    }
}
