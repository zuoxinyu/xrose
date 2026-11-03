use std::path::{Path, PathBuf};

use crate::error::{io_context, Result};

/// All on-disk locations Xross uses. Binaries live outside the app bundle so
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
    /// portable installs (`XROSS_HOME`).
    pub fn discover() -> Self {
        if let Ok(home) = std::env::var("XROSS_HOME") {
            let root = PathBuf::from(home);
            return Self::from_root(&root);
        }
        match directories::ProjectDirs::from("io", "xross", "Xross") {
            Some(dirs) => Self {
                config_dir: dirs.config_dir().to_path_buf(),
                data_dir: dirs.data_dir().to_path_buf(),
                cache_dir: dirs.cache_dir().to_path_buf(),
                runtime_dir: runtime_dir(dirs.data_dir()),
            },
            None => Self::from_root(&std::env::temp_dir().join("xross")),
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
        self.data_dir.join("xross.db")
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
        self.runtime_dir.join("xross.lock")
    }
    pub fn generated_config(&self, core: &str) -> PathBuf {
        self.runtime_dir.join(format!("{core}-config.json"))
    }
    pub fn core_pid_file(&self, core: &str) -> PathBuf {
        self.runtime_dir.join(format!("{core}.pid"))
    }
    pub fn pac_file(&self) -> PathBuf {
        self.runtime_dir.join("xross.pac")
    }
    pub fn backup_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
}

fn runtime_dir(data_dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            if !dir.is_empty() {
                return PathBuf::from(dir).join("xross");
            }
        }
        data_dir.join("run")
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        std::env::temp_dir().join("xross")
    }
}
