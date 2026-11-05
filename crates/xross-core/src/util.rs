use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, io_context};

/// SHA-256 of an in-memory buffer, lowercase hex.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Streaming SHA-256 of a file (does not load the file into memory).
pub fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|e| io_context(path, e))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 64];
    loop {
        let n = reader.read(&mut buf).map_err(|e| io_context(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Writes `contents` to `path` via a temporary sibling + rename so a crash
/// never leaves a truncated config or database.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
    }
    let tmp = tmp_sibling(path);
    {
        let mut f = File::create(&tmp).map_err(|e| io_context(&tmp, e))?;
        f.write_all(contents).map_err(|e| io_context(&tmp, e))?;
        f.flush().map_err(|e| io_context(&tmp, e))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|e| io_context(path, e))?;
    Ok(())
}

/// Atomically publish a file containing credentials with owner-only Unix permissions.
pub fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| io_context(parent, e))?;
    file.write_all(contents).map_err(|e| io_context(path, e))?;
    file.as_file().sync_all().map_err(|e| io_context(path, e))?;
    file.persist(path).map_err(|e| io_context(path, e.error))?;
    Ok(())
}

pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &bytes)
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".into());
    name.push_str(".tmp");
    path.with_file_name(name)
}

/// 12-byte random identifier, URL safe. Used for nodes, rules and profiles.
pub fn new_id(prefix: &str) -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &id[..12])
}

pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Human readable byte counts (binary units).
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

pub fn format_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Parses a comma separated port expression: `80,443,1000-2000`.
pub fn parse_ports(input: &str) -> std::result::Result<Vec<(u16, u16)>, String> {
    let mut out = Vec::new();
    for raw in input.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let a: u16 = a
                .trim()
                .parse()
                .map_err(|_| format!("invalid port: {part}"))?;
            let b: u16 = b
                .trim()
                .parse()
                .map_err(|_| format!("invalid port: {part}"))?;
            if a > b || a == 0 {
                return Err(format!("invalid port range: {part}"));
            }
            out.push((a, b));
        } else {
            let p: u16 = part.parse().map_err(|_| format!("invalid port: {part}"))?;
            if p == 0 {
                return Err(format!("invalid port: {part}"));
            }
            out.push((p, p));
        }
    }
    Ok(out)
}

/// Ports are stored as strings in the model so users can type ranges naturally.
pub fn ports_valid(input: &str) -> bool {
    parse_ports(input).is_ok()
}

/// Normalizes a host:port pair for display and de-duplication.
pub fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Basic truthy parser used for query params and YAML values.
pub fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "t"
    )
}

/// Decodes base64 tolerantly: standard, URL-safe, padded or unpadded.
pub fn decode_base64_any(input: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for engine in engines {
        if let Ok(bytes) = engine.decode(cleaned.as_bytes()) {
            return Some(bytes);
        }
    }
    None
}

pub fn encode_base64(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// Looks like a base64 blob (used to decide whether a subscription body is encoded).
pub fn looks_base64(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.len() < 8 || trimmed.contains("://") || trimmed.contains('{') {
        return false;
    }
    trimmed.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '\r' | '\n')
    }) && decode_base64_any(trimmed).is_some()
}

/// Locates an executable inside an extracted directory tree.
pub fn find_executable(root: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut fallback: Option<PathBuf> = None;
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            for wanted in names {
                if name == *wanted {
                    return Some(path);
                }
                if name == format!("{wanted}.exe") {
                    return Some(path);
                }
            }
            if fallback.is_none()
                && !name.contains('.')
                && path
                    .metadata()
                    .map(|m| m.len() > 1_000_000)
                    .unwrap_or(false)
            {
                fallback = Some(path);
            }
        }
    }
    fallback
}

#[cfg(unix)]
pub fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn supports_hyperlinks() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

/// Extracts the first line/column reference from a validator message.
/// Handles `config.json:12:5`, `line 12` and `(line 12, column 3)`.
pub fn extract_line_reference(text: &str) -> Option<u32> {
    let patterns = [
        r"(?:^|[\s(/])(?:[^\s:]+\.json):(\d+)(?::\d+)?",
        r"(?i)\bline\s+(\d+)\b",
    ];
    for pattern in patterns {
        let re = regex::Regex::new(pattern).ok()?;
        if let Some(caps) = re.captures(text) {
            if let Some(m) = caps.get(1) {
                if let Ok(v) = m.as_str().parse() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Removes ANSI escapes and control characters that would corrupt UI output.
pub fn sanitize_line(input: &str) -> String {
    let without_ansi = strip_ansi(input);
    without_ansi
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control() || *c == ' ')
        .collect()
}

/// Strips CSI/OSC escape sequences emitted by both cores when they detect a TTY.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI ... final byte in @..~
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC ... BEL or ESC backslash
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Parses a subscription-userinfo header value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscriptionUserInfo {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    pub expire: Option<i64>,
}

impl SubscriptionUserInfo {
    pub fn parse(header: &str) -> Option<Self> {
        let mut info = SubscriptionUserInfo::default();
        let mut any = false;
        for part in header.split(';') {
            let Some((k, v)) = part.trim().split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"');
            match k.trim().to_ascii_lowercase().as_str() {
                "upload" => {
                    info.upload = v.parse().unwrap_or(0);
                    any = true;
                }
                "download" => {
                    info.download = v.parse().unwrap_or(0);
                    any = true;
                }
                "total" => {
                    info.total = v.parse().unwrap_or(0);
                    any = true;
                    if info.total == 0 {
                        // Some panels use 0 for "unlimited".
                    }
                }
                "expire" => {
                    info.expire = v.parse().ok();
                    any = true;
                }
                _ => {}
            }
        }
        any.then_some(info)
    }

    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    pub fn remaining(&self) -> Option<u64> {
        (self.total > 0).then(|| self.total.saturating_sub(self.used()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_parse_ranges() {
        assert_eq!(
            parse_ports("80, 1000-1002").unwrap(),
            vec![(80, 80), (1000, 1002)]
        );
        assert!(parse_ports("0").is_err());
        assert!(parse_ports("100-99").is_err());
        assert!(parse_ports("abc").is_err());
    }

    #[test]
    fn base64_sniffing() {
        assert!(looks_base64("dm1lc3M6Ly9leGFtcGxl"));
        assert!(!looks_base64("{\"version\": 1}"));
        assert!(!looks_base64("vless://uuid@example.com:443"));
    }

    #[test]
    fn line_references() {
        assert_eq!(
            extract_line_reference("config.json:12:4: unknown field"),
            Some(12)
        );
        assert_eq!(
            extract_line_reference("FATAL decode config: line 9: bad"),
            Some(9)
        );
        assert_eq!(extract_line_reference("no line here"), None);
    }

    #[test]
    fn userinfo_parse() {
        let info =
            SubscriptionUserInfo::parse("upload=100; download=200; total=1000; expire=1700000000")
                .unwrap();
        assert_eq!(info.used(), 300);
        assert_eq!(info.remaining(), Some(700));
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.50 KiB");
    }

    #[test]
    fn sha256_streams_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        fs::write(&path, b"xross").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "cc418624f0299bdb723db901690c7d21bf4b827543804aedfe4ed354f5dc847a"
        );
        assert_eq!(sha256_bytes(b"xross").len(), 64);
    }
}
