//! Release discovery and updates for the app, cores and geo/rule-set data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::binary::platform_arch;
use crate::error::{Error, Result};
use crate::model::CoreKind;
use crate::util::{atomic_write, now_unix, sha256_bytes};

pub const XRAY_RELEASES: &str = "https://api.github.com/repos/XTLS/Xray-core/releases/latest";
pub const SINGBOX_RELEASES: &str = "https://api.github.com/repos/SagerNet/sing-box/releases/latest";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: String,
    pub notes: String,
    pub published_at: String,
    pub assets: Vec<ReleaseAsset>,
    pub prerelease: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCandidate {
    pub core: CoreKind,
    pub version: String,
    pub asset: ReleaseAsset,
    pub checksum_url: Option<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleSetUpdate {
    #[serde(default)]
    pub updated_at: Option<i64>,
    pub tag: String,
    pub status: String,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub error: Option<String>,
}

/// Connection details from the running core's applied listener.
#[derive(Debug, Clone)]
pub(crate) struct LocalProxy {
    pub host: String,
    pub port: u16,
    pub account: Option<crate::settings::ProxyAccount>,
}

impl LocalProxy {
    pub fn http_proxy(&self) -> Result<reqwest::Proxy> {
        let address = crate::util::host_port(&self.host, self.port);
        let mut proxy = reqwest::Proxy::all(format!("http://{address}"))?;
        if let Some(a) = &self.account {
            proxy = proxy.basic_auth(&a.username, &a.password);
        }
        Ok(proxy)
    }
}

/// Optional SOCKS5 endpoint for application updates. The URL can contain credentials.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Socks5Proxy(String);

impl std::fmt::Debug for Socks5Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Socks5Proxy(..)")
    }
}

impl Socks5Proxy {
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        let proxy = Self(value.into());
        if !value.is_empty() {
            proxy.url()?;
        }
        Ok(proxy)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn url(&self) -> Result<reqwest::Url> {
        let invalid = || Error::config("请输入有效的 SOCKS5 地址和端口");
        let address = self
            .0
            .strip_prefix("socks5://")
            .or_else(|| self.0.strip_prefix("socks5h://"))
            .unwrap_or(&self.0);
        let url = reqwest::Url::parse(&format!("socks5h://{address}")).map_err(|_| invalid())?;
        if url.host().is_none()
            || !matches!(url.port(), Some(1..=65535))
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
            || address.chars().any(char::is_whitespace)
        {
            return Err(invalid());
        }
        Ok(url)
    }
}

/// Explicit update routing, independent of the operating system proxy.
#[derive(Debug, Default, Clone)]
pub struct UpdateNetwork {
    pub(crate) use_proxy: bool,
    pub(crate) socks5: Socks5Proxy,
    pub(crate) proxy: Option<LocalProxy>,
    pub(crate) user_agent: String,
}

impl UpdateNetwork {
    /// CLI operations without a managed core must still honor the resource policy.
    pub fn offline_resources(settings: &crate::Settings) -> Self {
        Self {
            use_proxy: settings.proxy_resource_updates,
            socks5: settings.update_proxy.clone(),
            user_agent: settings.update_user_agent.clone(),
            proxy: None,
        }
    }

    pub(crate) fn builder(&self, fallback_agent: &str) -> Result<reqwest::ClientBuilder> {
        let agent = if self.user_agent.is_empty() {
            fallback_agent
        } else {
            &self.user_agent
        };
        let builder = reqwest::Client::builder().no_proxy().user_agent(agent);
        if self.use_proxy {
            if !self.socks5.as_str().is_empty() {
                let proxy = reqwest::Proxy::all(self.socks5.url()?)
                    .map_err(|_| Error::config("SOCKS5 代理地址无效"))?;
                return Ok(builder.proxy(proxy));
            }
            let proxy = self.proxy.as_ref().ok_or_else(|| {
                Error::other("请设置 SOCKS5 前置代理或启动带 HTTP 或混合入站的内核后重试")
            })?;
            Ok(builder.proxy(proxy.http_proxy()?))
        } else {
            Ok(builder)
        }
    }

    pub(crate) fn client(&self, timeout: Duration) -> Result<reqwest::Client> {
        Ok(self
            .builder(concat!("Xrose/", env!("CARGO_PKG_VERSION")))?
            .timeout(timeout)
            .build()?)
    }
}

async fn fetch_release(url: &str, network: &UpdateNetwork) -> Result<ReleaseInfo> {
    let response = network
        .client(Duration::from_secs(30))?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "查询更新失败：HTTP {}（可能需要代理或 GitHub 访问受限）",
            response.status()
        )));
    }
    let value: serde_json::Value = response.json().await?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if tag.is_empty() {
        return Err(Error::other("更新接口未返回版本号"));
    }
    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|asset| {
                    Some(ReleaseAsset {
                        name: asset.get("name")?.as_str()?.to_string(),
                        url: asset.get("browser_download_url")?.as_str()?.to_string(),
                        size: asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ReleaseInfo {
        version: tag.trim_start_matches('v').to_string(),
        tag,
        notes: value
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .chars()
            .take(4000)
            .collect(),
        published_at: value
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        assets,
        prerelease: value
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

pub async fn check_core_update(
    core: CoreKind,
    current_version: Option<&str>,
    network: &UpdateNetwork,
) -> Result<Option<UpdateCandidate>> {
    let url = match core {
        CoreKind::Xray => XRAY_RELEASES,
        CoreKind::SingBox => SINGBOX_RELEASES,
    };
    let release = fetch_release(url, network).await?;
    if let Some(current) = current_version {
        if !is_newer(&release.version, current) {
            return Ok(None);
        }
    }
    let asset = pick_asset(core, &release).ok_or_else(|| {
        Error::other(format!(
            "{} {} 没有适用于当前平台（{}/{}）的下载包",
            core.display_name(),
            release.version,
            platform_arch().0,
            platform_arch().1
        ))
    })?;
    let checksum_url = release
        .assets
        .iter()
        .find(|a| a.name == format!("{}.dgst", asset.name))
        .map(|a| a.url.clone())
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|a| a.name == format!("{}.sha256", asset.name))
                .map(|a| a.url.clone())
        });
    Ok(Some(UpdateCandidate {
        core,
        version: release.version,
        asset,
        checksum_url,
        notes: release.notes,
    }))
}

pub fn pick_asset(core: CoreKind, release: &ReleaseInfo) -> Option<ReleaseAsset> {
    let (os, arch) = platform_arch();
    pick_asset_for(core, release, os, arch)
}

fn pick_asset_for(
    core: CoreKind,
    release: &ReleaseInfo,
    os: &str,
    arch: &str,
) -> Option<ReleaseAsset> {
    let os_name = match core {
        CoreKind::Xray => os,
        CoreKind::SingBox => match os {
            "macos" => "darwin",
            "windows" => "windows",
            other => other,
        },
    };
    let arch_name = match core {
        CoreKind::Xray => match arch {
            "64" => "64",
            "arm64" => "arm64-v8a",
            "arm" => "arm32-v7a",
            "32" => "32",
            other => other,
        },
        CoreKind::SingBox => match arch {
            "64" => "amd64",
            "arm64" => "arm64",
            "arm" => "armv7",
            "32" => "386",
            other => other,
        },
    };
    let candidates: Vec<&ReleaseAsset> = release
        .assets
        .iter()
        .filter(|asset| {
            let name = asset.name.to_ascii_lowercase();
            let platform_match = name.contains(&format!("{os_name}-{arch_name}"))
                || name.contains(&format!("{os_name}_{arch_name}"));
            let is_archive =
                name.ends_with(".zip") || name.ends_with(".tar.gz") || name.ends_with(".tgz");
            platform_match && is_archive && !name.contains("legacy")
        })
        .collect();
    let chosen = candidates
        .iter()
        .find(|a| !a.name.contains("installer") && !a.name.contains("debug"))
        .copied()
        .or_else(|| candidates.first().copied());
    chosen.cloned()
}

/// Accepts `.dgst` (`SHA2-256= hex`) and plain hex checksum files.
pub fn parse_checksum(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let candidate = if let Some((_, value)) = line.rsplit_once('=') {
            value.trim()
        } else {
            line.split_whitespace().next().unwrap_or("")
        };
        if candidate.len() == 64 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(candidate.to_ascii_lowercase());
        }
    }
    None
}

pub async fn download_checksum(url: &str, network: &UpdateNetwork) -> Result<Option<String>> {
    let response = network
        .client(Duration::from_secs(30))?
        .get(url)
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let text = response.text().await?;
    Ok(parse_checksum(&text))
}

/// Binary semver-ish comparison with a pragmatic fallback.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse =
        |v: &str| semver::Version::parse(v.trim_start_matches('v').split('+').next().unwrap_or(v));
    match (parse(candidate), parse(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => candidate.trim_start_matches('v') != current.trim_start_matches('v'),
    }
}

// ---------------------------------------------------------------------------
// App + rule-set updates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUpdate {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub published_at: String,
}

fn lite_asset_name(version: &str) -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    let platform = std::env::consts::OS;
    let ext = match platform {
        "macos" => "dmg",
        "windows" => "zip",
        "linux" => {
            let executable = std::env::current_exe().ok()?;
            let format = executable.parent()?.join("../share/xrose/package-format");
            match std::fs::read_to_string(format).unwrap_or_default().trim() {
                "deb" => "deb",
                "arch" => "pkg.tar.zst",
                _ => "AppImage",
            }
        }
        _ => return None,
    };
    Some(format!("Xrose-{version}-{platform}-{arch}-lite.{ext}"))
}

pub async fn check_app_update(
    repository: &str,
    current: &str,
    network: &UpdateNetwork,
) -> Result<Option<AppUpdate>> {
    let api = format!("https://api.github.com/repos/{repository}/releases/latest");
    let release = fetch_release(&api, network).await?;
    if !is_newer(&release.version, current) {
        return Ok(None);
    }
    let url = lite_asset_name(&release.version)
        .and_then(|name| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == name)
                .map(|asset| asset.url.clone())
        })
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{repository}/releases/tag/{}",
                release.tag
            )
        });
    Ok(Some(AppUpdate {
        version: release.version,
        url,
        notes: release.notes,
        published_at: release.published_at,
    }))
}

/// Download only assets actually referenced by the generated Xray config.
pub async fn ensure_xray_geo(
    config: &serde_json::Value,
    dir: &Path,
    network: &UpdateNetwork,
) -> Result<()> {
    fn needs(value: &serde_json::Value, prefix: &str) -> bool {
        match value {
            serde_json::Value::String(s) => s.starts_with(prefix),
            serde_json::Value::Array(a) => a.iter().any(|v| needs(v, prefix)),
            serde_json::Value::Object(o) => o.values().any(|v| needs(v, prefix)),
            _ => false,
        }
    }
    for (prefix, file) in [("geoip:", "geoip.dat"), ("geosite:", "geosite.dat")] {
        let target = dir.join(file);
        if !needs(config, prefix) || std::fs::metadata(&target).is_ok_and(|m| m.len() > 0) {
            continue;
        }
        let url = format!(
            "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download/{file}"
        );
        let bytes = network
            .client(Duration::from_secs(30))?
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        if bytes.is_empty() {
            return Err(Error::binary(format!("{file} 下载内容为空，请重试")));
        }
        atomic_write(&target, &bytes)?;
    }
    Ok(())
}

fn rule_set_metadata(dir: &Path, tag: &str) -> BTreeMap<String, String> {
    std::fs::read(dir.join(format!("{tag}.meta.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn cached_rule_set(
    dir: &Path,
    tag: &str,
    meta: &BTreeMap<String, String>,
) -> Option<RuleSetUpdate> {
    let file = std::fs::metadata(rule_set_file(dir, tag)).ok()?;
    if !file.is_file() || file.len() == 0 {
        return None;
    }
    let updated_at = meta
        .get("updated_at")
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            let modified = file
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            i64::try_from(modified.as_secs()).ok()
        });
    Some(RuleSetUpdate {
        tag: tag.into(),
        status: "cached".into(),
        bytes: file.len(),
        updated_at,
        sha256: meta.get("sha256").cloned(),
        error: None,
    })
}

pub(crate) fn cached_rule_set_updates(dir: &Path) -> BTreeMap<String, RuleSetUpdate> {
    let mut results = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(tag) = name.to_str().and_then(|name| name.strip_suffix(".srs")) else {
                continue;
            };
            if let Some(state) = cached_rule_set(dir, tag, &rule_set_metadata(dir, tag)) {
                results.insert(tag.to_owned(), state);
            }
        }
    }
    results
}

async fn update_rule_set(
    set: &crate::model::RuleSet,
    dir: &Path,
    client: &reqwest::Client,
    meta: &BTreeMap<String, String>,
    state: &mut RuleSetUpdate,
) -> Result<()> {
    let mut request = client.get(&set.url);
    let source = sha256_bytes(set.url.as_bytes());
    let same_source = state.bytes > 0 && meta.get("source") == Some(&source);
    if same_source {
        if let Some(etag) = meta.get("etag") {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(modified) = meta.get("last_modified") {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
        }
    }
    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED && same_source {
        state.status = "not-modified".into();
        return Ok(());
    }
    if !response.status().is_success() {
        return Err(Error::other(format!("HTTP {}", response.status())));
    }
    let mut metadata = BTreeMap::new();
    for (name, header) in [
        ("etag", reqwest::header::ETAG),
        ("last_modified", reqwest::header::LAST_MODIFIED),
    ] {
        if let Some(value) = response.headers().get(header).and_then(|h| h.to_str().ok()) {
            metadata.insert(name, value.to_owned());
        }
    }
    let bytes = response.bytes().await?;
    if bytes.is_empty() {
        return Err(Error::other("下载内容为空，请检查规则集来源并重试"));
    }
    let hash = sha256_bytes(&bytes);
    atomic_write(&rule_set_file(dir, &set.tag), &bytes)?;
    let updated_at = now_unix();
    state.updated_at = Some(updated_at);
    state.bytes = bytes.len() as u64;
    state.sha256 = Some(hash.clone());
    metadata.insert("sha256", hash);
    metadata.insert("source", source);
    metadata.insert("updated_at", updated_at.to_string());
    crate::util::atomic_write_json(&dir.join(format!("{}.meta.json", set.tag)), &metadata)?;
    state.status = "updated".into();
    Ok(())
}

pub async fn sync_rule_sets(
    rule_sets: &[crate::model::RuleSet],
    dir: &Path,
    network: &UpdateNetwork,
) -> Vec<RuleSetUpdate> {
    sync_rule_sets_reporting(rule_sets, dir, network, |_| {}).await
}

pub(crate) async fn sync_rule_sets_reporting(
    rule_sets: &[crate::model::RuleSet],
    dir: &Path,
    network: &UpdateNetwork,
    mut report: impl FnMut(&RuleSetUpdate),
) -> Vec<RuleSetUpdate> {
    let mut results = Vec::with_capacity(rule_sets.len());
    let client = network.client(Duration::from_secs(30));
    for set in rule_sets {
        if !crate::route::transfer::safe_tag(&set.tag) {
            let state = RuleSetUpdate {
                tag: set.tag.clone(),
                status: "failed".into(),
                error: Some("规则集标签不能用作文件名，请修改标签后重试".into()),
                ..Default::default()
            };
            report(&state);
            results.push(state);
            continue;
        }
        let meta = rule_set_metadata(dir, &set.tag);
        let mut state = cached_rule_set(dir, &set.tag, &meta).unwrap_or_else(|| RuleSetUpdate {
            tag: set.tag.clone(),
            ..Default::default()
        });
        let error = match &client {
            Ok(client) => update_rule_set(set, dir, client, &meta, &mut state)
                .await
                .err()
                .map(|e| e.to_string()),
            Err(error) => Some(error.to_string()),
        };
        if error.is_some() {
            state.status = "failed".into();
            state.error = error;
        }
        report(&state);
        results.push(state);
    }
    results
}

pub fn rule_set_file(dir: &Path, tag: &str) -> PathBuf {
    dir.join(format!("{tag}.srs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_download_does_not_select_legacy_asset_listed_first() {
        let mut release = ReleaseInfo {
            tag: "v1.14.4".into(),
            version: "1.14.4".into(),
            notes: String::new(),
            published_at: String::new(),
            prerelease: false,
            assets: [
                "sing-box-1.14.4-windows-amd64-legacy-windows-7.zip",
                "sing-box-1.14.4-windows-amd64.zip",
            ]
            .into_iter()
            .map(|name| ReleaseAsset {
                name: name.into(),
                url: "https://example.test/core.zip".into(),
                size: 1,
            })
            .collect(),
        };
        assert_eq!(
            pick_asset_for(CoreKind::SingBox, &release, "windows", "64")
                .unwrap()
                .name,
            "sing-box-1.14.4-windows-amd64.zip"
        );
        release.assets.pop();
        assert!(pick_asset_for(CoreKind::SingBox, &release, "windows", "64").is_none());
    }

    #[tokio::test]
    async fn subscription_and_resource_requests_use_the_configured_proxy_and_agent() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let network = UpdateNetwork {
            use_proxy: true,
            socks5: Default::default(),
            proxy: Some(LocalProxy {
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                account: Some(crate::settings::ProxyAccount {
                    username: "fixture".into(),
                    password: uuid::Uuid::new_v4().to_string(),
                }),
            }),
            user_agent: "Fixture/2".into(),
        };
        let capture = tokio::spawn(async move {
            for (path, status) in [
                ("subscription", "200 OK"),
                ("asset", "200 OK"),
                ("asset", "304 Not Modified"),
                ("asset", "503 Service Unavailable"),
                ("mirror", "200 OK"),
            ] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut first = String::new();
                reader.read_line(&mut first).await.unwrap();
                assert!(first.starts_with(&format!("GET http://updates.invalid/{path} ")));
                let (mut agent, mut authenticated, mut conditional) = (false, false, false);
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).await.unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    let header = header.to_ascii_lowercase();
                    agent |= header.trim() == "user-agent: fixture/2";
                    conditional |= header.trim() == "if-none-match: fixture";
                    authenticated |= header.starts_with("proxy-authorization: basic ");
                }
                assert!(agent && authenticated);
                assert_eq!(conditional, status != "200 OK");
                let body = if status == "200 OK" { "ok" } else { "" };
                let response = format!(
                    "HTTP/1.1 {status}\r\nConnection: close\r\nETag: fixture\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
            }
        });
        let response = crate::subscription::fetch_subscription(crate::subscription::FetchRequest {
            network: network.clone(),
            url: "http://updates.invalid/subscription".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(response.body, "ok");
        let dir = tempfile::tempdir().unwrap();
        let mut set = crate::adapters::default_rule_set("geoip-cn").unwrap();
        set.url = "http://updates.invalid/asset".into();
        let sets = std::slice::from_ref(&set);
        let result = sync_rule_sets(sets, dir.path(), &network).await;
        assert_eq!(result[0].status, "updated");
        assert!(result[0].updated_at.is_some());
        // The timestamp is the last content update, even after a restart, 304 or failed retry.
        let meta = dir.path().join(format!("{}.meta.json", set.tag));
        let mut metadata = rule_set_metadata(dir.path(), &set.tag);
        metadata.insert("updated_at".into(), "1650000000".into());
        crate::util::atomic_write_json(&meta, &metadata).unwrap();
        assert_eq!(
            cached_rule_set_updates(dir.path())[&set.tag].updated_at,
            Some(1650000000)
        );
        for expected in ["not-modified", "failed"] {
            let result = sync_rule_sets(sets, dir.path(), &network).await;
            assert_eq!(result[0].status, expected);
            assert_eq!(result[0].updated_at, Some(1650000000));
            assert_eq!(result[0].error.is_some(), expected == "failed");
            assert_eq!(
                std::fs::read(rule_set_file(dir.path(), &set.tag)).unwrap(),
                b"ok"
            );
        }
        set.url = "http://updates.invalid/mirror".into();
        assert_eq!(
            sync_rule_sets(std::slice::from_ref(&set), dir.path(), &network).await[0].status,
            "updated"
        );
        std::fs::remove_file(&meta).unwrap();
        assert!(
            cached_rule_set_updates(dir.path())[&set.tag]
                .updated_at
                .is_some()
        );
        std::fs::remove_file(rule_set_file(dir.path(), &set.tag)).unwrap();
        assert!(cached_rule_set_updates(dir.path()).is_empty());
        capture.await.unwrap();
        assert!(
            UpdateNetwork {
                use_proxy: true,
                ..Default::default()
            }
            .builder("Fixture")
            .is_err()
        );
    }

    #[tokio::test]
    async fn offline_socks5_updates_send_hostnames_and_user_agent() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let settings = crate::Settings {
            proxy_resource_updates: true,
            update_proxy: Socks5Proxy::parse(&format!(
                "socks5://{}",
                listener.local_addr().unwrap()
            ))
            .unwrap(),
            update_user_agent: "Fixture/3".into(),
            ..Default::default()
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(socket.read_u8().await.unwrap(), 5);
            let mut methods = vec![0; usize::from(socket.read_u8().await.unwrap())];
            socket.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&0));
            socket.write_all(&[5, 0]).await.unwrap();
            let mut command = [0; 4];
            socket.read_exact(&mut command).await.unwrap();
            assert_eq!(command, [5, 1, 0, 3]);
            let mut domain = vec![0; usize::from(socket.read_u8().await.unwrap())];
            socket.read_exact(&mut domain).await.unwrap();
            assert_eq!(domain, b"asset.invalid");
            assert_eq!(socket.read_u16().await.unwrap(), 80);
            socket
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                if line.trim().is_empty() {
                    break;
                }
                lines.push(line.trim().to_ascii_lowercase());
            }
            assert!(lines.iter().any(|line| line == "user-agent: fixture/3"));
            reader
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let network = UpdateNetwork::offline_resources(&settings);
        let client = network.client(Duration::from_secs(5)).unwrap();
        let response = client
            .get("http://asset.invalid/core")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        server.await.unwrap();
    }

    #[test]
    fn update_proxy_validation_and_encryption() {
        for bad in [
            "https://example.invalid:1080",
            "example.invalid",
            "example.invalid:0",
            "example.invalid:1080/path",
            "socks5://example.invalid:1080?x",
        ] {
            assert!(Socks5Proxy::parse(bad).is_err());
        }
        assert!(Socks5Proxy::parse("[::1]:1080").is_ok());
        let secret = uuid::Uuid::new_v4().to_string();
        let proxy = Socks5Proxy::parse(&format!("socks5h://fixture:{secret}@example.invalid:1080"))
            .unwrap();
        let settings = crate::Settings {
            update_proxy: proxy,
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        settings.save(&path).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains(&secret));
        assert!(!format!("{settings:?}").contains(&secret));
        assert_eq!(
            crate::Settings::load(&path).unwrap().update_proxy,
            settings.update_proxy
        );
    }

    #[test]
    fn checksum_formats() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_checksum(&format!("SHA2-256= {hash}\n")),
            Some(hash.clone())
        );
        assert_eq!(parse_checksum(&format!("{hash}  file.zip")), Some(hash));
        assert_eq!(parse_checksum("not a hash"), None);
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(is_newer("v1.8.24", "1.8.23"));
    }

    #[test]
    fn asset_selection() {
        let (os, arch) = platform_arch();
        let xray_arch = if arch == "arm64" { "arm64-v8a" } else { "64" };
        let sing_arch = if arch == "arm64" { "arm64" } else { "amd64" };
        let xray_name = format!("Xray-{os}-{xray_arch}.zip");
        let sing_name = format!(
            "sing-box-1.10.0-{}-{sing_arch}.tar.gz",
            if os == "macos" { "darwin" } else { os }
        );
        let release = ReleaseInfo {
            tag: "v1.10.0".into(),
            version: "1.10.0".into(),
            notes: String::new(),
            published_at: String::new(),
            prerelease: false,
            assets: vec![
                ReleaseAsset {
                    name: xray_name,
                    url: "x".into(),
                    size: 1,
                },
                ReleaseAsset {
                    name: sing_name,
                    url: "y".into(),
                    size: 1,
                },
            ],
        };
        assert!(pick_asset(CoreKind::Xray, &release).is_some());
        assert!(pick_asset(CoreKind::SingBox, &release).is_some());
    }
}
