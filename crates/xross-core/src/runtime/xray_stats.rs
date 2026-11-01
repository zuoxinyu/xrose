//! Xray has no Clash API. Traffic counters come from its StatsService via the
//! `xray api statsquery` subcommand, which speaks gRPC internally and prints
//! JSON. Reusing the installed binary keeps this crate free of protobuf codegen.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::process::run_capture;
use crate::error::Result;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct XrayStat {
    pub name: String,
    pub value: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct XrayTraffic {
    pub uplink: u64,
    pub downlink: u64,
    /// Per outbound tag counters (e.g. `node-x`).
    pub per_outbound: BTreeMap<String, (u64, u64)>,
    pub raw: Vec<XrayStat>,
}

impl XrayTraffic {
    pub fn total(&self) -> u64 {
        self.uplink + self.downlink
    }
}

#[derive(Clone)]
pub struct XrayStatsClient {
    binary: PathBuf,
    server: String,
}

impl XrayStatsClient {
    pub fn new(binary: PathBuf, port: u16) -> Self {
        Self {
            binary,
            server: format!("127.0.0.1:{port}"),
        }
    }

    pub async fn query(&self) -> Result<XrayTraffic> {
        let args = vec![
            "api".to_string(),
            "statsquery".to_string(),
            "--server".to_string(),
            self.server.clone(),
            "--pattern".to_string(),
            String::new(),
        ];
        let (code, output) = run_capture(&self.binary, &args, Duration::from_secs(6)).await?;
        if code != 0 {
            return Err(crate::error::Error::other(format!(
                "xray statsquery 失败（退出码 {code}）：{}",
                output.trim()
            )));
        }
        // The command may prefix a banner before the JSON object.
        let json_start = output.find('{').unwrap_or(0);
        let value: serde_json::Value = serde_json::from_str(&output[json_start..])
            .map_err(|e| crate::error::Error::other(format!("解析 xray 统计失败：{e}")))?;
        let mut traffic = XrayTraffic::default();
        if let Some(stats) = value.get("stat").and_then(|v| v.as_array()) {
            for stat in stats {
                let name = stat
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let value = stat
                    .get("value")
                    .map(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                traffic.raw.push(XrayStat {
                    name: name.clone(),
                    value,
                });
                let parts: Vec<&str> = name.split(">>>").collect();
                if parts.len() >= 4 && parts[0] == "outbound" {
                    let tag = parts[1].to_string();
                    let direction = parts[3];
                    let entry = traffic.per_outbound.entry(tag.clone()).or_insert((0, 0));
                    let is_proxy_node = tag.starts_with("node-");
                    if direction == "uplink" {
                        entry.0 += value;
                        if is_proxy_node {
                            traffic.uplink += value;
                        }
                    } else if direction == "downlink" {
                        entry.1 += value;
                        if is_proxy_node {
                            traffic.downlink += value;
                        }
                    }
                }
            }
        }
        Ok(traffic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_add_up() {
        let traffic = XrayTraffic {
            uplink: 10,
            downlink: 20,
            ..Default::default()
        };
        assert_eq!(traffic.total(), 30);
    }
}
