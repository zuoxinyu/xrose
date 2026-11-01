use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::settings::LogLevel;
use crate::util::{now_rfc3339, sanitize_line};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    #[default]
    Core,
    Stdout,
    Stderr,
    App,
}

impl LogStream {
    pub fn label(&self) -> &'static str {
        match self {
            LogStream::Core => "core",
            LogStream::Stdout => "stdout",
            LogStream::Stderr => "stderr",
            LogStream::App => "app",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub unix_ms: i64,
    pub level: LogLevel,
    #[serde(default)]
    pub stream: LogStream,
    pub message: String,
}

impl LogEntry {
    pub fn matches(&self, min_level: LogLevel) -> bool {
        self.level >= min_level
    }
}

/// In-memory ring buffer. The UI polls a snapshot; core processes append from
/// reader tasks. No lossy broadcast channel means a slow UI never stalls the
/// core's stdout pipe.
#[derive(Clone)]
pub struct LogBus {
    inner: Arc<Mutex<LogInner>>,
    file: Arc<Mutex<Option<RollingFile>>>,
    max_entries: usize,
}

struct LogInner {
    entries: VecDeque<LogEntry>,
    dropped: u64,
}

impl LogBus {
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogInner {
                entries: VecDeque::new(),
                dropped: 0,
            })),
            file: Arc::new(Mutex::new(None)),
            max_entries,
        }
    }

    pub fn with_file(dir: PathBuf, base: &str, max_bytes: u64, keep: usize) -> Self {
        let bus = Self::new(5000);
        let rolling = RollingFile::new(dir, base, max_bytes, keep);
        *bus.file.lock() = Some(rolling);
        bus
    }

    pub fn push(&self, stream: LogStream, level: LogLevel, message: impl Into<String>) {
        let entry = LogEntry {
            timestamp: now_rfc3339(),
            unix_ms: chrono::Utc::now().timestamp_millis(),
            level,
            stream,
            message: message.into(),
        };
        self.push_entry(entry, true);
    }

    fn push_entry(&self, entry: LogEntry, persist: bool) {
        let message = sanitize_line(&entry.message);
        if message.trim().is_empty() {
            return;
        }
        let entry = LogEntry { message, ..entry };
        {
            let mut inner = self.inner.lock();
            inner.entries.push_back(entry.clone());
            while inner.entries.len() > self.max_entries {
                inner.entries.pop_front();
                inner.dropped += 1;
            }
        }
        if persist {
            if let Some(file) = self.file.lock().as_mut() {
                let _ = file.write(&entry);
            }
        }
    }

    pub fn core_line(&self, stream: LogStream, line: &str) {
        let level = infer_level(line);
        self.push(stream, level, line);
    }

    pub fn recent(&self, limit: usize, min_level: LogLevel, query: &str) -> Vec<LogEntry> {
        let query = query.trim().to_ascii_lowercase();
        let inner = self.inner.lock();
        let mut out: Vec<LogEntry> = inner
            .entries
            .iter()
            .rev()
            .filter(|e| e.matches(min_level))
            .filter(|e| query.is_empty() || e.message.to_ascii_lowercase().contains(&query))
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }

    /// Loads the tail of an existing log file into the ring buffer so the log
    /// viewer is useful right after a restart.
    pub fn load_tail(&self, path: &std::path::Path, max_lines: usize) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let start = bytes.len().saturating_sub(256 * 1024);
        let text = String::from_utf8_lossy(&bytes[start..]);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let skip = lines.len().saturating_sub(max_lines);
        for line in &lines[skip..] {
            let (timestamp, level, stream, message) = parse_log_line(line);
            self.push_entry(
                LogEntry {
                    unix_ms: chrono::Utc::now().timestamp_millis(),
                    timestamp,
                    level,
                    stream,
                    message,
                },
                false,
            );
        }
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.entries.clear();
        inner.dropped = 0;
    }

    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dropped(&self) -> u64 {
        self.inner.lock().dropped
    }

    pub fn export_to(&self, path: &std::path::Path) -> crate::Result<()> {
        let mut out = String::new();
        for entry in self.inner.lock().entries.iter() {
            out.push_str(&format!(
                "{} [{}] {} {}\n",
                entry.timestamp,
                entry.level.as_str(),
                entry.stream.label(),
                entry.message
            ));
        }
        crate::util::atomic_write(path, out.as_bytes())
    }
}

/// Extracts a level from a core log line.
pub fn infer_level(line: &str) -> LogLevel {
    let upper = line.to_ascii_uppercase();
    let token = upper.split(|c: char| !c.is_ascii_alphanumeric()).find(|t| {
        matches!(
            *t,
            "TRACE" | "DEBUG" | "INFO" | "WARN" | "WARNING" | "ERROR" | "FATAL" | "PANIC"
        )
    });
    match token {
        Some("TRACE") => LogLevel::Trace,
        Some("DEBUG") => LogLevel::Debug,
        Some("WARN") | Some("WARNING") => LogLevel::Warn,
        Some("ERROR") | Some("FATAL") | Some("PANIC") => LogLevel::Error,
        Some("INFO") => LogLevel::Info,
        _ => {
            if upper.contains("ERROR") || upper.contains("FAILED") {
                LogLevel::Error
            } else {
                LogLevel::Info
            }
        }
    }
}

/// Parses a line previously written by [`RollingFile`]:
/// `2026-...Z [info] app message`.
fn parse_log_line(line: &str) -> (String, LogLevel, LogStream, String) {
    let level = infer_level(line);
    let (timestamp, rest) = match line.split_once(' ') {
        Some((ts, rest)) if ts.contains('T') || ts.contains('-') => (ts.to_string(), rest),
        _ => (now_rfc3339(), line),
    };
    let rest = rest
        .strip_prefix('[')
        .and_then(|r| r.split_once("] "))
        .map(|(_, r)| r)
        .unwrap_or(rest);
    let (stream, message) = match rest.split_once(' ') {
        Some((stream, msg)) => match stream {
            "app" => (LogStream::App, msg),
            "core" => (LogStream::Core, msg),
            "stdout" => (LogStream::Stdout, msg),
            "stderr" => (LogStream::Stderr, msg),
            _ => (LogStream::App, rest),
        },
        None => (LogStream::App, rest),
    };
    (timestamp, level, stream, message.to_string())
}

/// Size-rotated log files: `xross.log`, `xross.1.log`, ...
pub struct RollingFile {
    dir: PathBuf,
    base: String,
    max_bytes: u64,
    keep: usize,
    current: File,
    current_size: u64,
}

impl RollingFile {
    pub fn new(dir: PathBuf, base: &str, max_bytes: u64, keep: usize) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{base}.log"));
        let current = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .or_else(|_| File::create(&path))
            .expect("cannot create log file");
        let current_size = current.metadata().map(|m| m.len()).unwrap_or(0);
        Self {
            dir,
            base: base.into(),
            max_bytes: max_bytes.max(1024),
            keep: keep.max(1),
            current,
            current_size,
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.dir.join(format!("{}.log", self.base))
        } else {
            self.dir.join(format!("{}.{}.log", self.base, index))
        }
    }

    fn write(&mut self, entry: &LogEntry) -> std::io::Result<()> {
        let line = format!(
            "{} [{}] {} {}\n",
            entry.timestamp,
            entry.level.as_str(),
            entry.stream.label(),
            entry.message
        );
        if self.current_size + line.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        self.current.write_all(line.as_bytes())?;
        self.current.flush()?;
        self.current_size += line.len() as u64;
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        for index in (1..self.keep).rev() {
            let from = self.path(index);
            let to = self.path(index + 1);
            if from.exists() {
                let _ = std::fs::rename(from, to);
            }
        }
        let _ = std::fs::rename(self.path(0), self.path(1));
        self.current = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.path(0))?;
        self.current_size = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_inference() {
        assert_eq!(infer_level("INFO [123] inbound started"), LogLevel::Info);
        assert_eq!(infer_level("[Warning] something"), LogLevel::Warn);
        assert_eq!(infer_level("FATAL failed to start"), LogLevel::Error);
        assert_eq!(infer_level("plain message"), LogLevel::Info);
    }

    #[test]
    fn ring_buffer_filters() {
        let bus = LogBus::new(10);
        bus.push(LogStream::Core, LogLevel::Info, "hello");
        bus.push(LogStream::Core, LogLevel::Error, "boom");
        let all = bus.recent(10, LogLevel::Info, "");
        assert_eq!(all.len(), 2);
        let errors = bus.recent(10, LogLevel::Error, "");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "boom");
        assert!(bus.recent(10, LogLevel::Info, "hell")[0]
            .message
            .contains("hello"));
    }

    #[test]
    fn ring_buffer_is_bounded() {
        let bus = LogBus::new(3);
        for i in 0..5 {
            bus.push(LogStream::Core, LogLevel::Info, format!("line {i}"));
        }
        assert_eq!(bus.len(), 3);
        assert_eq!(bus.dropped(), 2);
    }

    #[test]
    fn rolling_file_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = RollingFile::new(dir.path().to_path_buf(), "xross", 1024, 3);
        for i in 0..20 {
            let entry = LogEntry {
                timestamp: "t".into(),
                unix_ms: i,
                level: LogLevel::Info,
                stream: LogStream::Core,
                message: "x".repeat(120),
            };
            file.write(&entry).unwrap();
        }
        assert!(dir.path().join("xross.log").exists());
        assert!(dir.path().join("xross.1.log").exists());
    }
}
