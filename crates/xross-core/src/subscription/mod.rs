//! Subscription model, format parsers and share-link/QR helpers.

pub mod fetch;
pub mod links;
pub mod links_export;
pub mod parse;

pub use fetch::{fetch_subscription, FetchRequest, FetchResponse, FetchStatus, DEFAULT_USER_AGENT};
pub use links::parse_share_link;
pub use links_export::node_to_share_link;
pub use parse::{parse_subscription, ParsedGroup, ParsedSubscription, SourceFormat};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::util::{new_id, now_unix, SubscriptionUserInfo};

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
    /// Send through a proxy? Reserved for future use; currently informational.
    #[serde(default)]
    pub user_agent: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "crate::model::default_true")]
    pub enabled: bool,
    #[serde(default = "default_interval")]
    pub update_interval_secs: u64,
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

fn default_interval() -> u64 {
    12 * 3600
}

impl Subscription {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: new_id("sub"),
            name: name.into(),
            url: url.into(),
            user_agent: DEFAULT_USER_AGENT.into(),
            headers: BTreeMap::new(),
            enabled: true,
            update_interval_secs: default_interval(),
            last_updated: None,
            etag: None,
            last_modified: None,
            usage: None,
            last_error: None,
            node_count: 0,
            sort: 0,
        }
    }

    pub fn due_for_update(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_updated {
            None => true,
            Some(last) => now_unix().saturating_sub(last) >= self.update_interval_secs as i64,
        }
    }

    pub fn masked_url(&self) -> String {
        match reqwest::Url::parse(&self.url) {
            Ok(url) => {
                if url.host_str().is_some() {
                    format!(
                        "{}://{}{}",
                        url.scheme(),
                        url.host_str().unwrap_or_default(),
                        if url.path().is_empty() {
                            "/..."
                        } else {
                            "/..."
                        }
                    )
                } else {
                    "（无效地址）".into()
                }
            }
            Err(_) => "（无效地址）".into(),
        }
    }
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
}
