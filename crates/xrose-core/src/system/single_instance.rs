//! Single-instance lock using an advisory file lock.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{io_context, Result};

pub struct SingleInstance {
    file: File,
    path: PathBuf,
}

impl SingleInstance {
    /// Returns `Ok(None)` when another instance already holds the lock.
    pub fn acquire(path: &Path) -> Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| io_context(path, e))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                file.set_len(0).ok();
                let _ = file.seek(SeekFrom::Start(0));
                let _ = write!(file, "{}", std::process::id());
                let _ = file.sync_all();
                Ok(Some(Self {
                    file,
                    path: path.to_path_buf(),
                }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Pid of the running instance, if the lock file records one.
    pub fn existing_pid(path: &Path) -> Option<u32> {
        let mut text = String::new();
        File::open(path).ok()?.read_to_string(&mut text).ok()?;
        text.trim().parse().ok()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xrose.lock");
        let first = SingleInstance::acquire(&path).unwrap();
        assert!(first.is_some());
        let second = SingleInstance::acquire(&path).unwrap();
        assert!(second.is_none());
        assert_eq!(
            SingleInstance::existing_pid(&path),
            Some(std::process::id())
        );
        drop(first);
        let third = SingleInstance::acquire(&path).unwrap();
        assert!(third.is_some());
    }
}
