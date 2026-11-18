use std::path::{Path, PathBuf};

use crate::error::{Result, io_context};

/// All on-disk locations Xrose uses. Binaries live outside the app bundle so
/// app updates never replace a working core and vice versa.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl AppPaths {
    /// Platform-correct locations, with an environment override for tests and
    /// portable installs (`XROSE_HOME`).
    pub fn discover() -> Self {
        if let Ok(home) = std::env::var("XROSE_HOME") {
            let root = PathBuf::from(home);
            return Self::from_root(&root);
        }
        #[cfg(windows)]
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                // The portable GUI, the CLI in XroseData/data/bin, and the helper in
                // XroseData/data/helper share a profile.
                for base in [
                    Some(dir),
                    (dir.ends_with("XroseData/data/bin") || dir.ends_with("XroseData/data/helper"))
                        .then(|| dir.ancestors().nth(3))
                        .flatten(),
                ]
                .into_iter()
                .flatten()
                {
                    if base.join("xrose.portable").is_file() {
                        return Self::from_root(&base.join("XroseData"));
                    }
                }
            }
        }
        match directories::ProjectDirs::from("io", "xrose", "Xrose") {
            Some(dirs) => Self {
                config_dir: dirs.config_dir().to_path_buf(),
                data_dir: dirs.data_dir().to_path_buf(),
                cache_dir: dirs.cache_dir().to_path_buf(),
                runtime_dir: runtime_dir(dirs.data_dir()),
            },
            None => Self::from_root(&std::env::temp_dir().join("xrose")),
        }
    }

    pub fn from_root(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("run"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.runtime_dir,
        ] {
            std::fs::create_dir_all(dir).map_err(|e| io_context(dir, e))?;
        }
        Ok(())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }
    pub fn database(&self) -> PathBuf {
        self.data_dir.join("xrose.db")
    }
    pub fn cores_dir(&self) -> PathBuf {
        self.data_dir.join("cores")
    }
    pub fn binaries_manifest(&self) -> PathBuf {
        self.cores_dir().join("manifest.json")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    pub fn rule_sets_dir(&self) -> PathBuf {
        self.data_dir.join("rulesets")
    }
    pub fn secrets_key(&self) -> PathBuf {
        self.config_dir.join("secrets.key")
    }
    pub fn proxy_snapshot(&self) -> PathBuf {
        self.runtime_dir.join("proxy-snapshot.json")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("xrose.lock")
    }
    pub fn generated_config(&self, core: &str) -> PathBuf {
        self.runtime_dir.join(format!("{core}-config.json"))
    }
    pub fn core_pid_file(&self, core: &str) -> PathBuf {
        self.runtime_dir.join(format!("{core}.pid"))
    }
    pub fn pac_file(&self) -> PathBuf {
        self.runtime_dir.join("xrose.pac")
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }

    /// True when this profile lives inside a `XroseData` directory activated by
    /// a sibling `xrose.portable` marker file.
    pub fn is_portable_data(&self) -> bool {
        let Some(root) = self.config_dir.parent() else {
            return false;
        };
        root.file_name() == Some(std::ffi::OsStr::new("XroseData"))
            && root
                .parent()
                .is_some_and(|dir| dir.join("xrose.portable").is_file())
    }

    /// Deterministic identifier for single-instance coordination scoped to this
    /// profile directory, stable before and after [`Self::ensure`] creates it.
    pub fn instance_id(&self) -> String {
        let normalized = normalize_instance_path(&self.config_dir);
        let digest = crate::util::sha256_bytes(normalized.as_bytes());
        digest[..16].to_string()
    }
}

fn resolve_instance_path(path: &Path) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(path) {
        return canon;
    }
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut tail = Vec::new();
    let mut current: &Path = &abs;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            tail.push(name.to_os_string());
        }
        if let Ok(mut canon_parent) = std::fs::canonicalize(parent) {
            for name in tail.into_iter().rev() {
                canon_parent.push(name);
            }
            return canon_parent;
        }
        current = parent;
    }
    abs
}

fn normalize_instance_path(path: &Path) -> String {
    let resolved = resolve_instance_path(path);
    #[cfg(windows)]
    {
        let raw = resolved.to_string_lossy().replace('/', "\\");
        let stripped = if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else if let Some(local) = raw.strip_prefix(r"\\?\") {
            local.to_string()
        } else {
            raw
        };
        stripped.trim_end_matches('\\').to_lowercase()
    }
    #[cfg(not(windows))]
    {
        let raw = resolved.to_string_lossy();
        raw.trim_end_matches('/').to_string()
    }
}

fn runtime_dir(data_dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            if !dir.is_empty() {
                return PathBuf::from(dir).join("xrose");
            }
        }
        data_dir.join("run")
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        std::env::temp_dir().join("xrose")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_stays_stable_before_and_after_directory_creation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profile-a");
        let paths = AppPaths::from_root(&root);
        let before = paths.instance_id();
        paths.ensure().unwrap();
        let after = paths.instance_id();
        assert_eq!(before, after);
        assert_eq!(before.len(), 16);

        let dot_paths = AppPaths::from_root(&root.join("."));
        assert_eq!(before, dot_paths.instance_id());

        let other = AppPaths::from_root(&dir.path().join("profile-b"));
        assert_ne!(before, other.instance_id());
    }

    #[test]
    fn portable_data_marker_detection_matches_layout() {
        let dir = tempfile::tempdir().unwrap();
        let normal = AppPaths::from_root(&dir.path().join("XroseData"));
        assert!(!normal.is_portable_data());

        std::fs::write(dir.path().join("xrose.portable"), "full").unwrap();
        assert!(normal.is_portable_data());

        let custom = AppPaths::from_root(&dir.path().join("custom"));
        assert!(!custom.is_portable_data());
    }
}
