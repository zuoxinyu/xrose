//! Single-instance lock using an advisory file lock.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::error::{Result, io_context};

pub struct SingleInstance {
    file: File,
    path: PathBuf,
    #[cfg(unix)]
    wakeup_socket: Option<PathBuf>,
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
                    #[cfg(unix)]
                    wakeup_socket: None,
                }))
            }
            Err(_) => Ok(None),
        }
    }

    /// Registers a wakeup callback for secondary launches on Unix platforms.
    /// Windows uses a message-window wakeup channel so foreground activation
    /// rights can be transferred across integrity levels.
    pub fn on_wakeup<F>(&mut self, callback: F)
    where
        F: Fn() + Send + 'static,
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::net::UnixListener;

            let sock = self.path.with_extension("sock");
            let _ = std::fs::remove_file(&sock);
            let Ok(listener) = UnixListener::bind(&sock) else {
                return;
            };
            let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
            self.wakeup_socket = Some(sock);
            let _ = std::thread::Builder::new()
                .name("xrose-single-instance".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if stream.is_ok() {
                            callback();
                        }
                    }
                });
        }
        #[cfg(not(unix))]
        {
            let _ = callback;
        }
    }

    /// Signals a running Unix instance holding `lock_path` to show its window.
    pub fn notify_existing(lock_path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            UnixStream::connect(lock_path.with_extension("sock")).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = lock_path;
            false
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
        #[cfg(unix)]
        if let Some(sock) = self.wakeup_socket.take() {
            let _ = std::fs::remove_file(sock);
        }
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
