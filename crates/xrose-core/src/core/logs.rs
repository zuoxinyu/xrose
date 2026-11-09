use std::collections::{BTreeSet, VecDeque};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSourceFilter {
    All,
    App,
    Core,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    pub source: LogSourceFilter,
    pub levels: BTreeSet<LogLevel>,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self {
            source: LogSourceFilter::All,
            levels: [LogLevel::Info, LogLevel::Warn, LogLevel::Error].into(),
        }
    }
}

impl LogFilter {
    pub fn matches(&self, entry: &LogEntry) -> bool {
        let source_matches = match self.source {
            LogSourceFilter::All => true,
            LogSourceFilter::App => entry.stream == LogStream::App,
            LogSourceFilter::Core => matches!(
                entry.stream,
                LogStream::Core | LogStream::Stdout | LogStream::Stderr
            ),
        };
        source_matches && self.levels.contains(&entry.level)
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
    revision: u64,
    entries: VecDeque<LogEntry>,
    dropped: u64,
}

impl LogBus {
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogInner {
                revision: 0,
                entries: VecDeque::new(),
                dropped: 0,
            })),
            file: Arc::new(Mutex::new(None)),
            max_entries,
        }
    }

    /// Disk logging is best-effort: a read-only or otherwise unwritable data
    /// directory must never abort startup, so a failure degrades to in-memory
    /// logs plus a warning in the log viewer.
    pub fn with_file(dir: PathBuf, base: &str, max_bytes: u64, keep: usize) -> Self {
        let bus = Self::new(5000);
        match RollingFile::new(dir.clone(), base, max_bytes, keep) {
            Ok(rolling) => *bus.file.lock() = Some(rolling),
            Err(err) => {
                let path = dir.join(format!("{base}.log"));
                let message = format!(
                    "无法写入日志文件 {}：{err}；本次运行仅保留内存日志",
                    path.display()
                );
                if std::env::var_os("XROSE_DEBUG").is_some() {
                    eprintln!("[debug] {message}");
                }
                bus.push(LogStream::App, LogLevel::Warn, message);
            }
        }
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
            inner.revision = inner.revision.wrapping_add(1);
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
        self.recent_matching(limit, query, |entry| entry.matches(min_level))
    }

    pub(crate) fn recent_matching(
        &self,
        limit: usize,
        query: &str,
        matches: impl Fn(&LogEntry) -> bool,
    ) -> Vec<LogEntry> {
        let query = query.trim().to_ascii_lowercase();
        let inner = self.inner.lock();
        let mut out: Vec<LogEntry> = inner
            .entries
            .iter()
            .rev()
            .filter(|e| matches(e))
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
        inner.revision = inner.revision.wrapping_add(1);
        inner.entries.clear();
        inner.dropped = 0;
    }

    /// Changes on append, eviction or clear, even when the ring length is unchanged.
    pub fn revision(&self) -> u64 {
        self.inner.lock().revision
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

/// Size-rotated log files: `xrose.log`, `xrose.1.log`, ...
pub struct RollingFile {
    dir: PathBuf,
    base: String,
    max_bytes: u64,
    keep: usize,
    current: File,
    current_size: u64,
}

impl RollingFile {
    pub fn new(dir: PathBuf, base: &str, max_bytes: u64, keep: usize) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{base}.log"));
        let current = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .or_else(|_| File::create(&path))?;
        let current_size = current.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir,
            base: base.into(),
            max_bytes: max_bytes.max(1024),
            keep: keep.max(1),
            current,
            current_size,
        })
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
    fn revision_tracks_replacements_and_clear() {
        let bus = LogBus::new(1);
        bus.push(LogStream::Core, LogLevel::Info, "one");
        let revision = bus.revision();
        bus.push(LogStream::Core, LogLevel::Info, "two");
        assert_eq!(bus.len(), 1);
        assert_ne!(revision, bus.revision());
        let revision = bus.revision();
        bus.clear();
        assert_ne!(revision, bus.revision());
    }

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
        assert!(
            bus.recent(10, LogLevel::Info, "hell")[0]
                .message
                .contains("hello")
        );
    }

    #[test]
    fn source_levels_and_query_filter_live_and_restored_entries_before_limit() {
        use LogLevel::{Debug, Error, Info, Trace, Warn};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xrose.log");
        std::fs::write(
            &path,
            "2026-01-01T00:00:00Z [debug] stdout Probe restored\n",
        )
        .unwrap();
        let bus = LogBus::new(20);
        bus.load_tail(&path, 10);
        bus.push(LogStream::Core, Error, "probe core");
        bus.push(LogStream::Stderr, Debug, "probe stderr");
        bus.push(LogStream::App, Error, "probe app");
        bus.push(LogStream::Stdout, Warn, "probe warning");
        bus.push(LogStream::Stderr, Error, "other error");
        bus.push(LogStream::App, Info, "probe info");
        bus.push(LogStream::App, Trace, "probe trace");

        let messages = |filter: &LogFilter, limit| {
            bus.recent_matching(limit, "  PROBE  ", |entry| filter.matches(entry))
                .into_iter()
                .map(|entry| entry.message)
                .collect::<Vec<_>>()
        };
        let mut filter = LogFilter::default();
        assert_eq!(
            messages(&filter, 10),
            ["probe core", "probe app", "probe warning", "probe info"]
        );
        filter.levels = [Debug, Error].into();
        filter.source = LogSourceFilter::Core;
        assert_eq!(
            messages(&filter, 3),
            ["Probe restored", "probe core", "probe stderr"]
        );
        assert_eq!(messages(&filter, 2), ["probe core", "probe stderr"]);
        filter.source = LogSourceFilter::App;
        assert_eq!(messages(&filter, 2), ["probe app"]);
        filter.levels = [Trace].into();
        assert_eq!(messages(&filter, 2), ["probe trace"]);
        filter.levels.clear();
        assert!(messages(&filter, 2).is_empty());
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
    fn unwritable_log_dir_degrades_to_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the log directory should be makes
        // `create_dir_all` fail, mimicking a read-only data directory.
        let blocked = dir.path().join("logs");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let bus = LogBus::with_file(blocked, "xrose", 4096, 2);
        bus.push(LogStream::App, LogLevel::Info, "still works");
        assert_eq!(bus.len(), 2);
        assert_eq!(
            bus.recent(10, LogLevel::Warn, "")
                .iter()
                .filter(|e| e.message.contains("无法写入日志文件"))
                .count(),
            1
        );
    }

    #[test]
    fn rolling_file_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = RollingFile::new(dir.path().to_path_buf(), "xrose", 1024, 3).unwrap();
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
        assert!(dir.path().join("xrose.log").exists());
        assert!(dir.path().join("xrose.1.log").exists());
    }
}
