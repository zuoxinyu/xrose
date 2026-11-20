//! Subscription model, format parsers and share-link/QR helpers.

pub mod fetch;
pub mod links;
pub mod links_export;
mod openvpn;
pub mod parse;

pub use fetch::{DEFAULT_USER_AGENT, FetchRequest, FetchResponse, FetchStatus, fetch_subscription};
pub use links::parse_share_link;
pub use links_export::node_to_share_link;
pub use parse::{ParsedGroup, ParsedSubscription, SourceFormat, parse_subscription};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util::{SubscriptionUserInfo, new_id, now_unix};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SubscriptionUsage {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    /// Unix timestamp.
    pub expire: Option<i64>,
}

impl SubscriptionUsage {
    pub fn from_userinfo(info: &SubscriptionUserInfo) -> Self {
        Self {
            upload: info.upload,
            download: info.download,
            total: info.total,
            expire: info.expire,
        }
    }

    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    pub fn remaining(&self) -> Option<u64> {
        (self.total > 0).then(|| self.total.saturating_sub(self.used()))
    }

    pub fn percent_used(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.used() as f64 / self.total as f64 * 100.0) as f32
        }
    }

    pub fn expired(&self) -> bool {
        self.expire
            .map(|e| e > 0 && e < now_unix())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Empty inherits the global update User-Agent.
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub last_user_agent: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "crate::model::default_true")]
    pub enabled: bool,
    #[serde(default = "default_interval")]
    pub update_interval_secs: u64,
    /// User-configured per-subscription interval in seconds. When set, takes
    /// precedence over both the global default and server `profile-update-interval`.
    #[serde(default)]
    pub custom_update_interval_secs: Option<u64>,
    /// Server-suggested interval parsed from `profile-update-interval`, if any.
    #[serde(default)]
    pub server_update_interval_secs: Option<u64>,
    #[serde(default)]
    pub last_updated: Option<i64>,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub usage: Option<SubscriptionUsage>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub sort: i64,
}

pub const DEFAULT_UPDATE_INTERVAL_HOURS: u64 = 12;
pub const MIN_UPDATE_INTERVAL_HOURS: u64 = 1;
pub const MAX_UPDATE_INTERVAL_HOURS: u64 = 24 * 30;
pub const DEFAULT_UPDATE_INTERVAL_SECS: u64 = DEFAULT_UPDATE_INTERVAL_HOURS * 3600;

fn default_interval() -> u64 {
    DEFAULT_UPDATE_INTERVAL_SECS
}

impl Subscription {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: new_id("sub"),
            name: name.into(),
            url: url.into(),
            user_agent: String::new(),
            last_user_agent: None,
            headers: BTreeMap::new(),
            enabled: true,
            update_interval_secs: default_interval(),
            custom_update_interval_secs: None,
            server_update_interval_secs: None,
            last_updated: None,
            etag: None,
            last_modified: None,
            usage: None,
            last_error: None,
            node_count: 0,
            sort: 0,
        }
    }

    pub fn clamp_interval_hours(hours: u64) -> u64 {
        hours.clamp(MIN_UPDATE_INTERVAL_HOURS, MAX_UPDATE_INTERVAL_HOURS)
    }

    pub fn parse_interval_hours(raw: &str) -> crate::Result<u64> {
        let trimmed = raw.trim();
        let hours = trimmed.parse::<u64>().map_err(|_| {
            crate::Error::config(format!(
                "更新间隔须为 {MIN_UPDATE_INTERVAL_HOURS} 至 {MAX_UPDATE_INTERVAL_HOURS} 之间的整数小时"
            ))
        })?;
        if !(MIN_UPDATE_INTERVAL_HOURS..=MAX_UPDATE_INTERVAL_HOURS).contains(&hours) {
            return Err(crate::Error::config(format!(
                "更新间隔须为 {MIN_UPDATE_INTERVAL_HOURS} 至 {MAX_UPDATE_INTERVAL_HOURS} 之间的整数小时"
            )));
        }
        Ok(hours)
    }

    pub fn effective_update_interval_secs(&self, global_default_hours: u64) -> u64 {
        let min_secs = MIN_UPDATE_INTERVAL_HOURS * 3600;
        let max_secs = MAX_UPDATE_INTERVAL_HOURS * 3600;
        if let Some(custom) = self.custom_update_interval_secs.filter(|&secs| secs > 0) {
            return custom.clamp(min_secs, max_secs);
        }
        if let Some(server) = self.server_update_interval_secs.filter(|&secs| secs > 0) {
            return server.clamp(min_secs, max_secs);
        }
        let hours = if global_default_hours == 0 {
            DEFAULT_UPDATE_INTERVAL_HOURS
        } else {
            Self::clamp_interval_hours(global_default_hours)
        };
        hours * 3600
    }

    pub fn effective_update_interval_hours(&self, global_default_hours: u64) -> u64 {
        (self.effective_update_interval_secs(global_default_hours) / 3600)
            .max(MIN_UPDATE_INTERVAL_HOURS)
    }

    pub fn is_expired(&self) -> bool {
        self.usage.as_ref().is_some_and(SubscriptionUsage::expired)
    }

    pub fn due_for_update_with(&self, global_default_hours: u64) -> bool {
        if !self.enabled {
            return false;
        }
        let interval = self.effective_update_interval_secs(global_default_hours);
        match self.last_updated {
            None => true,
            Some(last) => now_unix().saturating_sub(last) >= interval as i64,
        }
    }

    pub fn due_for_update(&self) -> bool {
        self.due_for_update_with(DEFAULT_UPDATE_INTERVAL_HOURS)
    }

    pub fn masked_url(&self) -> String {
        match reqwest::Url::parse(&self.url) {
            Ok(url) => {
                if url.host_str().is_some() {
                    format!(
                        "{}://{}{}",
                        url.scheme(),
                        url.host_str().unwrap_or_default(),
                        "/..."
                    )
                } else {
                    "（无效地址）".into()
                }
            }
            Err(_) => "（无效地址）".into(),
        }
    }
}

/// Dark modules of a graphical QR code, without its quiet zone.
pub fn qr_matrix(text: &str) -> crate::Result<Vec<Vec<bool>>> {
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|err| crate::Error::other(format!("无法生成二维码：{err}")))?;
    let width = code.width();
    Ok((0..width)
        .map(|y| {
            (0..width)
                .map(|x| code[(x, y)] == qrcode::Color::Dark)
                .collect()
        })
        .collect())
}

/// Renders a QR code as block characters, suitable for a monospace UI.
pub fn qr_ascii(text: &str) -> crate::Result<String> {
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|e| crate::Error::other(format!("生成二维码失败：{e}")))?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2usize;
    let mut output = String::new();
    for _ in 0..quiet {
        output.push_str(&"  ".repeat(width + quiet * 2));
        output.push('\n');
    }
    for row in 0..width {
        output.push_str(&"  ".repeat(quiet));
        for col in 0..width {
            let dark = colors[row * width + col] == qrcode::Color::Dark;
            output.push_str(if dark { "██" } else { "  " });
        }
        output.push_str(&"  ".repeat(quiet));
        output.push('\n');
    }
    for _ in 0..quiet {
        output.push_str(&"  ".repeat(width + quiet * 2));
        output.push('\n');
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_due_and_masked() {
        let mut sub = Subscription::new("test", "https://example.com/sub?token=secret");
        assert!(sub.due_for_update());
        sub.last_updated = Some(now_unix());
        assert!(!sub.due_for_update());
        assert!(!sub.masked_url().contains("secret"));
    }

    #[test]
    fn usage_math() {
        let usage = SubscriptionUsage {
            upload: 100,
            download: 200,
            total: 1000,
            expire: None,
        };
        assert_eq!(usage.used(), 300);
        assert_eq!(usage.remaining(), Some(700));
        assert!((usage.percent_used() - 30.0).abs() < 0.01);
    }

    #[test]
    fn qr_renders() {
        let qr = qr_ascii("vless://example").unwrap();
        assert!(qr.lines().count() > 10);
    }

    #[test]
    fn interval_precedence_and_expiry_rules() {
        let mut sub = Subscription::new("feed", "https://example.com/sub");
        assert_eq!(sub.effective_update_interval_hours(6), 6);
        assert_eq!(
            sub.effective_update_interval_hours(0),
            DEFAULT_UPDATE_INTERVAL_HOURS
        );

        // Server hint takes effect while subscription follows defaults.
        sub.server_update_interval_secs = Some(24 * 3600);
        assert_eq!(sub.effective_update_interval_hours(6), 24);

        // User per-subscription override takes precedence over server hint and global default.
        sub.custom_update_interval_secs = Some(4 * 3600);
        assert_eq!(sub.effective_update_interval_hours(6), 4);

        sub.last_updated = Some(now_unix() - 5 * 3600);
        assert!(sub.due_for_update_with(24));
        sub.last_updated = Some(now_unix() - 2 * 3600);
        assert!(!sub.due_for_update_with(1));

        assert_eq!(Subscription::parse_interval_hours("24").unwrap(), 24);
        assert!(Subscription::parse_interval_hours("0").is_err());
        assert!(Subscription::parse_interval_hours("721").is_err());
        assert!(Subscription::parse_interval_hours("abc").is_err());

        assert!(!sub.is_expired());
        sub.usage = Some(SubscriptionUsage {
            expire: Some(now_unix() - 60),
            ..Default::default()
        });
        assert!(sub.is_expired());
        sub.usage = Some(SubscriptionUsage {
            expire: Some(now_unix() + 3600),
            ..Default::default()
        });
        assert!(!sub.is_expired());
    }
}
