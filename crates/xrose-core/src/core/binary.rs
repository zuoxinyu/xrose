use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_context};
use crate::model::CoreKind;
use crate::paths::AppPaths;
use crate::util::{atomic_write_json, find_executable, now_unix, set_executable, sha256_file};

/// Availability cached for the settings view. Runtime launches resolve the executable again.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LocalBinaryStatus {
    pub path: Option<PathBuf>,
    pub custom_available: bool,
}

pub(crate) fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        path.extension()
            .and_then(|s| s.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

fn find_in_search_path(core: CoreKind, search: &std::ffi::OsStr) -> Option<PathBuf> {
    let exe = format!("{}{}", core.binary_name(), std::env::consts::EXE_SUFFIX);
    std::env::split_paths(search)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(&exe))
        .find(|candidate| is_executable(candidate))
        .and_then(|path| std::path::absolute(path).ok())
}

pub(crate) fn path_binary(core: CoreKind) -> Option<PathBuf> {
    find_in_search_path(core, &std::env::var_os("PATH")?)
}

/// `(platform, arch)` in the naming used by Xray and sing-box releases.
pub fn platform_arch() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else if cfg!(target_arch = "x86") {
        "32"
    } else {
        "unknown"
    };
    (os, arch)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinaryRecord {
    pub core: CoreKind,
    pub version: String,
    pub platform: String,
    pub arch: String,
    /// Path relative to the binary store root, so the store stays portable.
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
    pub installed_at: i64,
    #[serde(default)]
    pub source_url: Option<String>,
}

impl BinaryRecord {
    pub fn absolute_path(&self, manager: &BinaryManager) -> PathBuf {
        manager.root.join(&self.relative_path)
    }

    pub fn matches_platform(&self) -> bool {
        let (platform, arch) = platform_arch();
        self.platform == platform && self.arch == arch
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    binaries: Vec<BinaryRecord>,
}

/// Owns downloaded cores. Binaries are never bundled with the desktop app:
/// this is what makes "app update" and "core update" independent.
pub struct BinaryManager {
    root: PathBuf,
    manifest_file: PathBuf,
}

impl BinaryManager {
    pub fn new(paths: &AppPaths) -> Self {
        let root = paths.cores_dir();
        let manifest_file = paths.binaries_manifest();
        let _ = fs::create_dir_all(&root);
        Self {
            root,
            manifest_file,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn load(&self) -> Manifest {
        fs::read(&self.manifest_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, manifest: &Manifest) -> Result<()> {
        atomic_write_json(&self.manifest_file, manifest)
    }

    pub fn list(&self) -> Vec<BinaryRecord> {
        let mut list = self.load().binaries;
        list.sort_by(|a, b| {
            a.core
                .as_str()
                .cmp(b.core.as_str())
                .then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
        });
        list
    }

    pub fn for_core(&self, core: CoreKind) -> Vec<BinaryRecord> {
        self.list().into_iter().filter(|b| b.core == core).collect()
    }

    /// Chooses the active binary: explicit version > latest platform build.
    pub fn resolve(&self, core: CoreKind, preferred_version: Option<&str>) -> Option<BinaryRecord> {
        let mut candidates: Vec<BinaryRecord> = self
            .for_core(core)
            .into_iter()
            .filter(|b| b.matches_platform())
            .collect();
        candidates.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
        if let Some(version) = preferred_version {
            return candidates.into_iter().find(|b| b.version == version);
        }
        candidates.into_iter().next()
    }

    pub fn verify(&self, record: &BinaryRecord) -> Result<bool> {
        let path = record.absolute_path(self);
        if !path.exists() {
            return Ok(false);
        }
        Ok(sha256_file(&path)? == record.sha256)
    }

    /// Installs a downloaded archive or raw binary into the versioned store.
    pub fn install_file(
        &self,
        core: CoreKind,
        version: &str,
        archive: &Path,
        expected_sha256: Option<&str>,
        source_url: Option<String>,
    ) -> Result<BinaryRecord> {
        let actual = sha256_file(archive)?;
        if let Some(expected) = expected_sha256 {
            if !expected.is_empty() && !expected.eq_ignore_ascii_case(&actual) {
                return Err(Error::binary(format!(
                    "SHA-256 校验失败：期望 {expected}，实际 {actual}"
                )));
            }
        }
        let version = version.trim().trim_start_matches('v');
        if version.is_empty() {
            return Err(Error::binary("版本号不能为空"));
        }
        let (platform, arch) = platform_arch();
        let target_dir = self.root.join(core.as_str()).join(version);
        if target_dir.exists() {
            fs::remove_dir_all(&target_dir).map_err(|e| io_context(&target_dir, e))?;
        }
        fs::create_dir_all(&target_dir).map_err(|e| io_context(&target_dir, e))?;
        extract_archive(archive, &target_dir)?;

        let exec_name = core.binary_name();
        let exe = find_executable(&target_dir, &[exec_name])
            .or_else(|| {
                let direct = target_dir.join("binary");
                direct.exists().then_some(direct)
            })
            .ok_or_else(|| {
                Error::binary(format!("压缩包中未找到 {} 可执行文件", core.binary_name()))
            })?;
        set_executable(&exe).map_err(|e| io_context(&exe, e))?;

        // Normalize the executable to a predictable location.
        let final_path = target_dir.join(if cfg!(windows) {
            format!("{exec_name}.exe")
        } else {
            exec_name.to_string()
        });
        if exe != final_path {
            fs::copy(&exe, &final_path).map_err(|e| io_context(&final_path, e))?;
            set_executable(&final_path).map_err(|e| io_context(&final_path, e))?;
        }
        let relative = final_path
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| final_path.to_string_lossy().to_string());
        let record = BinaryRecord {
            core,
            version: version.to_string(),
            platform: platform.into(),
            arch: arch.into(),
            relative_path: relative,
            sha256: sha256_file(&final_path)?,
            size: fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0),
            installed_at: now_unix(),
            source_url,
        };
        let mut manifest = self.load();
        manifest.binaries.retain(|b| {
            !(b.core == core
                && b.version == record.version
                && b.platform == record.platform
                && b.arch == record.arch)
        });
        manifest.binaries.push(record.clone());
        self.save(&manifest)?;
        Ok(record)
    }

    /// Registers an already-installed binary without copying (advanced users).
    pub fn adopt_existing(
        &self,
        core: CoreKind,
        version: &str,
        path: &Path,
    ) -> Result<BinaryRecord> {
        if !path.exists() {
            return Err(Error::binary(format!(
                "找不到可执行文件：{}",
                path.display()
            )));
        }
        let (platform, arch) = platform_arch();
        let record = BinaryRecord {
            core,
            version: version.trim_start_matches('v').to_string(),
            platform: platform.into(),
            arch: arch.into(),
            relative_path: path.to_string_lossy().to_string(),
            sha256: sha256_file(path)?,
            size: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            installed_at: now_unix(),
            source_url: None,
        };
        let mut manifest = self.load();
        manifest.binaries.push(record.clone());
        self.save(&manifest)?;
        Ok(record)
    }

    pub fn remove(&self, core: CoreKind, version: &str) -> Result<()> {
        let mut manifest = self.load();
        manifest
            .binaries
            .retain(|b| !(b.core == core && b.version == version));
        self.save(&manifest)?;
        let dir = self.root.join(core.as_str()).join(version);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| io_context(&dir, e))?;
        }
        Ok(())
    }

    /// Downloads and installs a release asset. `sha_expected` may come from a
    /// `.dgst` sidecar published next to the archive.
    pub async fn install_from_url(
        &self,
        core: CoreKind,
        version: &str,
        url: &str,
        sha_expected: Option<String>,
        network: &crate::updates::UpdateNetwork,
    ) -> Result<BinaryRecord> {
        let client = network.client(std::time::Duration::from_secs(300))?;
        let response = client.get(url).send().await?;
        if !response.status().is_success() {
            return Err(Error::binary(format!(
                "下载失败：HTTP {}",
                response.status()
            )));
        }
        let bytes = response.bytes().await?;
        let extension = if url.to_ascii_lowercase().contains(".tar.gz") {
            ".tar.gz"
        } else if url.to_ascii_lowercase().contains(".tgz") {
            ".tgz"
        } else if url.to_ascii_lowercase().contains(".zip") {
            ".zip"
        } else {
            ""
        };
        let archive = self.root.join(format!(
            ".download-{}-{}{}",
            core.as_str(),
            now_unix(),
            extension
        ));
        crate::util::atomic_write(&archive, &bytes)?;
        let result = self.install_file(
            core,
            version,
            &archive,
            sha_expected.as_deref(),
            Some(url.into()),
        );
        let _ = fs::remove_file(&archive);
        result
    }
}

/// Loose comparison of version strings (`1.8.24`, `v1.9.0`, `1.13.0-beta.1`).
fn version_key(version: &str) -> (u64, u64, u64, String) {
    let trimmed = version.trim().trim_start_matches('v');
    let core = trimmed.split(['-', '+']).next().unwrap_or(trimmed);
    let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        trimmed.to_string(),
    )
}

fn extract_archive(archive: &Path, target: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    // Prefer magic bytes: download URLs do not always carry a usable extension.
    let mut magic = [0u8; 4];
    let is_zip = std::fs::File::open(archive)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic).map(|_| magic == *b"PK\x03\x04")
        })
        .unwrap_or(false);
    let is_gzip = magic[0] == 0x1f && magic[1] == 0x8b;
    if is_zip || name.ends_with(".zip") {
        let file = fs::File::open(archive).map_err(|e| io_context(archive, e))?;
        let mut zip =
            zip::ZipArchive::new(file).map_err(|e| Error::binary(format!("解压 zip 失败：{e}")))?;
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| Error::binary(format!("读取 zip 条目失败：{e}")))?;
            let Some(rel) = entry.enclosed_name() else {
                continue;
            };
            let out = target.join(rel);
            if entry.is_dir() {
                fs::create_dir_all(&out).map_err(|e| io_context(&out, e))?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
                }
                let mut f = fs::File::create(&out).map_err(|e| io_context(&out, e))?;
                std::io::copy(&mut entry, &mut f).map_err(|e| io_context(&out, e))?;
            }
        }
    } else if is_gzip || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let file = fs::File::open(archive).map_err(|e| io_context(archive, e))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        if let Err(tar_err) = tar.unpack(target) {
            // Some releases ship a bare gzip-compressed executable.
            let out = target.join("binary");
            let file = fs::File::open(archive).map_err(|e| io_context(archive, e))?;
            let mut decoder = flate2::read::GzDecoder::new(file);
            let mut outfile = fs::File::create(&out).map_err(|e| io_context(&out, e))?;
            std::io::copy(&mut decoder, &mut outfile)
                .map_err(|e| Error::binary(format!("解压失败（tar: {tar_err}; gzip: {e}）")))?;
            set_executable(&out).map_err(|e| io_context(&out, e))?;
        }
    } else {
        // Raw executable.
        let out = target.join("binary");
        fs::copy(archive, &out).map_err(|e| io_context(&out, e))?;
        set_executable(&out).map_err(|e| io_context(&out, e))?;
    }
    Ok(())
}

/// Runs `version` and extracts a version string. Errors are soft: an unknown
/// version string is still usable.
pub async fn probe_version(binary: &Path) -> Option<String> {
    let (code, text) = super::process::run_capture(
        binary,
        &["version".into()],
        std::time::Duration::from_secs(5),
    )
    .await
    .ok()?;
    if code != 0 {
        return None;
    }
    let re = regex::Regex::new(r"(?i)\b(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.\-]+)?)").ok()?;
    re.captures(&text).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_is_known() {
        let (platform, arch) = platform_arch();
        assert_ne!(platform, "unknown");
        assert_ne!(arch, "unknown");
    }

    #[test]
    fn path_lookup_uses_the_first_executable_for_each_core() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let search = std::env::join_paths([&first, &second]).unwrap();
        for core in CoreKind::all() {
            let name = format!("{}{}", core.binary_name(), std::env::consts::EXE_SUFFIX);
            let candidate = first.join(&name);
            fs::create_dir(&candidate).unwrap();
            assert_eq!(find_in_search_path(core, &search), None);
            let fallback = second.join(&name);
            fs::write(&fallback, b"fixture").unwrap();
            set_executable(&fallback).unwrap();
            assert_eq!(find_in_search_path(core, &search), Some(fallback.clone()));
            fs::remove_dir(&candidate).unwrap();
            fs::write(&candidate, b"fixture").unwrap();
            #[cfg(unix)]
            assert_eq!(find_in_search_path(core, &search), Some(fallback));
            set_executable(&candidate).unwrap();
            assert_eq!(find_in_search_path(core, &search), Some(candidate));
        }
    }

    #[test]
    fn version_ordering() {
        assert!(version_key("v1.9.0") > version_key("1.8.24"));
        assert!(version_key("1.10.0") > version_key("1.9.9"));
    }

    #[test]
    fn adopts_and_verifies_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path());
        paths.ensure().unwrap();
        let manager = BinaryManager::new(&paths);
        let file = paths.cores_dir().join("xray-test");
        fs::write(&file, b"fake-binary").unwrap();
        let record = manager
            .adopt_existing(CoreKind::Xray, "1.8.24", &file)
            .unwrap();
        assert!(manager.verify(&record).unwrap());
        assert_eq!(manager.for_core(CoreKind::Xray).len(), 1);
    }

    #[test]
    fn rejects_bad_sha() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(tmp.path());
        paths.ensure().unwrap();
        let manager = BinaryManager::new(&paths);
        let file = tmp.path().join("raw");
        fs::write(&file, b"binary").unwrap();
        let err = manager
            .install_file(CoreKind::Xray, "1.0.0", &file, Some("deadbeef"), None)
            .unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use crate::paths::AppPaths;

    #[test]
    fn extracts_tar_gz_without_url_extension() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path());
        paths.ensure().unwrap();
        let manager = BinaryManager::new(&paths);
        let archive = dir.path().join("download-raw");
        {
            let file = fs::File::create(&archive).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            let payload = b"#!/bin/sh\necho hi\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(
                &mut header,
                "sing-box-1.0.0-linux-amd64/sing-box",
                &payload[..],
            )
            .unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let record = manager
            .install_file(CoreKind::SingBox, "1.0.0", &archive, None, None)
            .unwrap();
        let path = record.absolute_path(&manager);
        assert!(path.exists());
        assert_eq!(fs::read(&path).unwrap(), b"#!/bin/sh\necho hi\n");
    }
}
