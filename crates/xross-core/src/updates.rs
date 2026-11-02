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
pub const APP_RELEASES: &str = "https://api.github.com/repos/xross-app/xross/releases/latest";

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
    pub tag: String,
    pub status: String,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub error: Option<String>,
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("Xross/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?)
}

async fn fetch_release(url: &str) -> Result<ReleaseInfo> {
    let response = client()?
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
) -> Result<Option<UpdateCandidate>> {
    let url = match core {
        CoreKind::Xray => XRAY_RELEASES,
        CoreKind::SingBox => SINGBOX_RELEASES,
    };
    let release = fetch_release(url).await?;
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

pub async fn download_checksum(url: &str) -> Result<Option<String>> {
    let response = client()?.get(url).send().await?;
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

pub async fn check_app_update(current: &str) -> Result<Option<AppUpdate>> {
    let release = fetch_release(APP_RELEASES).await?;
    if !is_newer(&release.version, current) {
        return Ok(None);
    }
    let asset = release
        .assets
        .iter()
        .find(|a| {
            a.name.ends_with(".zip")
                || a.name.ends_with(".AppImage")
                || a.name.ends_with(".dmg")
                || a.name.ends_with(".exe")
        })
        .cloned()
        .unwrap_or_else(|| ReleaseAsset {
            name: "release".into(),
            url: format!(
                "https://github.com/xross-app/xross/releases/tag/{}",
                release.tag
            ),
            size: 0,
        });
    Ok(Some(AppUpdate {
        version: release.version,
        url: asset.url,
        notes: release.notes,
        published_at: release.published_at,
    }))
}

/// Downloads rule-set files, honoring cached checksums. Returns one result per
/// rule-set; failures are reported rather than aborting the whole batch.
pub async fn sync_rule_sets(rule_sets: &[crate::model::RuleSet], dir: &Path) -> Vec<RuleSetUpdate> {
    let mut results = Vec::new();
    let _ = std::fs::create_dir_all(dir);
    let client = match client() {
        Ok(client) => client,
        Err(err) => {
            return rule_sets
                .iter()
                .map(|set| RuleSetUpdate {
                    tag: set.tag.clone(),
                    status: "failed".into(),
                    error: Some(err.to_string()),
                    ..Default::default()
                })
                .collect()
        }
    };
    for set in rule_sets {
        let target = dir.join(format!("{}.srs", set.tag));
        let meta_path = dir.join(format!("{}.meta.json", set.tag));
        let mut meta: BTreeMap<String, String> = std::fs::read(&meta_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut request = client.get(&set.url);
        if target.exists() {
            if let Some(etag) = meta.get("etag") {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(last_modified) = meta.get("last_modified") {
                request = request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
            }
        }
        match request.send().await {
            Ok(response) if response.status() == reqwest::StatusCode::NOT_MODIFIED => {
                results.push(RuleSetUpdate {
                    tag: set.tag.clone(),
                    status: "not-modified".into(),
                    bytes: std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0),
                    sha256: meta.get("sha256").cloned(),
                    error: None,
                });
            }
            Ok(response) if response.status().is_success() => {
                let etag = response
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let last_modified = response
                    .headers()
                    .get(reqwest::header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                match response.bytes().await {
                    Ok(bytes) => {
                        let hash = sha256_bytes(&bytes);
                        if let Err(err) = atomic_write(&target, &bytes) {
                            results.push(RuleSetUpdate {
                                tag: set.tag.clone(),
                                status: "failed".into(),
                                error: Some(err.to_string()),
                                ..Default::default()
                            });
                            continue;
                        }
                        meta.insert("sha256".into(), hash.clone());
                        meta.insert("updated_at".into(), now_unix().to_string());
                        if let Some(etag) = etag {
                            meta.insert("etag".into(), etag);
                        }
                        if let Some(last_modified) = last_modified {
                            meta.insert("last_modified".into(), last_modified);
                        }
                        let _ = crate::util::atomic_write_json(&meta_path, &meta);
                        results.push(RuleSetUpdate {
                            tag: set.tag.clone(),
                            status: "updated".into(),
                            bytes: bytes.len() as u64,
                            sha256: Some(hash),
                            error: None,
                        });
                    }
                    Err(err) => results.push(RuleSetUpdate {
                        tag: set.tag.clone(),
                        status: "failed".into(),
                        error: Some(format!("读取失败：{err}")),
                        ..Default::default()
                    }),
                }
            }
            Ok(response) => results.push(RuleSetUpdate {
                tag: set.tag.clone(),
                status: "failed".into(),
                error: Some(format!("HTTP {}", response.status())),
                ..Default::default()
            }),
            Err(err) => results.push(RuleSetUpdate {
                tag: set.tag.clone(),
                status: "failed".into(),
                error: Some(err.to_string()),
                ..Default::default()
            }),
        }
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
