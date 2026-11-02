//! Asynchronous server-IP geolocation for the selected-node summary.

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::settings::Language;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeLocation {
    pub label: Option<String>,
    pub pending: bool,
    pub checked_at: i64,
}

impl NodeLocation {
    pub(crate) fn needs_refresh(&self, now: i64) -> bool {
        let ttl = if self.label.is_some() {
            24 * 60 * 60
        } else {
            5 * 60
        };
        !self.pending && now.saturating_sub(self.checked_at) >= ttl
    }
}

pub(crate) fn cache_key(server: &str, language: Language) -> String {
    format!("{}:{}", language.code(), server.trim().to_ascii_lowercase())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LocationResponse {
    success: bool,
    country: String,
    region: String,
    city: String,
}

impl LocationResponse {
    fn label(self) -> Option<String> {
        if !self.success {
            return None;
        }
        let mut parts = Vec::new();
        for part in [&self.country, &self.region, &self.city] {
            let part = part.trim();
            if !part.is_empty() && !parts.contains(&part) {
                parts.push(part);
            }
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 240
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (18..=19).contains(&b))
        }
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || {
                !ip.is_loopback()
                    && !ip.is_unspecified()
                    && !ip.is_unique_local()
                    && !ip.is_unicast_link_local()
                    && !ip.is_multicast()
                    && ip.segments()[..2] != [0x2001, 0x0db8]
            },
            |ip| is_public(IpAddr::V4(ip)),
        ),
    }
}

pub(crate) async fn lookup(server: &str, language: Language) -> Option<String> {
    // Bound DNS and HTTP together. Query only the server IP, never credentials.
    tokio::time::timeout(Duration::from_secs(8), async {
        let server = server.trim().trim_start_matches('[').trim_end_matches(']');
        let ip = if let Ok(ip) = server.parse::<IpAddr>() {
            is_public(ip).then_some(ip)?
        } else {
            tokio::net::lookup_host((server, 0))
                .await
                .ok()?
                .map(|addr| addr.ip())
                .find(|ip| is_public(*ip))?
        };
        // Bypass the system proxy so this also works while the core is stopped.
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(6))
            .build()
            .ok()?;
        client
            .get(format!("https://ipwho.is/{ip}"))
            .query(&[
                ("fields", "success,country,region,city"),
                ("lang", language.code()),
            ])
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<LocationResponse>()
            .await
            .ok()?
            .label()
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_location_without_empty_or_duplicate_parts() {
        let response: LocationResponse = serde_json::from_str(
            r#"{"success":true,"country":"新加坡","region":"新加坡","city":" "}"#,
        )
        .unwrap();
        assert_eq!(response.label().as_deref(), Some("新加坡"));
        let response: LocationResponse = serde_json::from_str(
            r#"{"success":true,"country":"美国","region":"加利福尼亚州","city":"洛杉矶"}"#,
        )
        .unwrap();
        assert_eq!(
            response.label().as_deref(),
            Some("美国 · 加利福尼亚州 · 洛杉矶")
        );
        assert!(LocationResponse::default().label().is_none());
        let response: LocationResponse =
            serde_json::from_str(r#"{"success":false,"country":"美国"}"#).unwrap();
        assert!(response.label().is_none());
    }

    #[test]
    fn excludes_local_and_reserved_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.2.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700:4700::1111", "::ffff:8.8.8.8"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn cache_retries_failures_and_expires_successes() {
        let mut entry = NodeLocation {
            checked_at: 100,
            ..Default::default()
        };
        assert!(!entry.needs_refresh(399));
        assert!(entry.needs_refresh(400));
        entry.label = Some("新加坡".into());
        assert!(!entry.needs_refresh(400));
        assert!(entry.needs_refresh(86_500));
        entry.pending = true;
        assert!(!entry.needs_refresh(100_000));
        assert_eq!(
            cache_key("EXAMPLE.COM", Language::Zh),
            cache_key("example.com", Language::Zh)
        );
        assert_ne!(
            cache_key("example.com", Language::Zh),
            cache_key("example.com", Language::En)
        );
    }
}
