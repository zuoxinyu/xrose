//! Clash-compatible API client used for sing-box (and Clash-family cores).

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClashConnectionMetadata {
    #[serde(default)]
    pub network: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "sourceIP", default)]
    pub source_ip: String,
    #[serde(rename = "destinationIP", default)]
    pub destination_ip: String,
    #[serde(rename = "sourcePort", default)]
    pub source_port: String,
    #[serde(rename = "destinationPort", default)]
    pub destination_port: String,
    #[serde(default)]
    pub host: String,
    #[serde(rename = "processPath", default)]
    pub process_path: String,
    #[serde(default)]
    pub process: String,
    #[serde(rename = "sniffHost", default)]
    pub sniff_host: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClashConnection {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub metadata: ClashConnectionMetadata,
    #[serde(default)]
    pub upload: u64,
    #[serde(default)]
    pub download: u64,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub chains: Vec<String>,
    #[serde(default)]
    pub rule: String,
    #[serde(rename = "rulePayload", default)]
    pub rule_payload: String,
}

impl ClashConnection {
    pub fn host(&self) -> String {
        if !self.metadata.host.is_empty() {
            self.metadata.host.clone()
        } else if !self.metadata.sniff_host.is_empty() {
            self.metadata.sniff_host.clone()
        } else {
            self.metadata.destination_ip.clone()
        }
    }

    pub fn process_name(&self) -> String {
        if !self.metadata.process.is_empty() {
            return self.metadata.process.clone();
        }
        self.metadata
            .process_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionsResponse {
    #[serde(rename = "downloadTotal", default)]
    pub download_total: u64,
    #[serde(rename = "uploadTotal", default)]
    pub upload_total: u64,
    #[serde(default)]
    pub connections: Vec<ClashConnection>,
    #[serde(rename = "memory", default)]
    pub memory: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClashProxy {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub all: Vec<String>,
    #[serde(default)]
    pub udp: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxiesResponse {
    #[serde(default)]
    pub proxies: BTreeMap<String, ClashProxy>,
}

#[derive(Clone)]
pub struct ClashClient {
    base: String,
    secret: String,
    client: reqwest::Client,
}

impl ClashClient {
    pub fn new(port: u16, secret: impl Into<String>) -> Self {
        Self {
            base: format!("http://127.0.0.1:{port}"),
            secret: secret.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .no_proxy()
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, format!("{}{path}", self.base));
        if !self.secret.is_empty() {
            builder = builder.bearer_auth(&self.secret);
        }
        builder
    }

    pub async fn connections(&self) -> Result<ConnectionsResponse> {
        let response = self
            .request(reqwest::Method::GET, "/connections")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "Clash API /connections 返回 {}",
                response.status()
            )));
        }
        Ok(response.json::<ConnectionsResponse>().await?)
    }

    pub async fn proxies(&self) -> Result<ProxiesResponse> {
        let response = self
            .request(reqwest::Method::GET, "/proxies")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "Clash API /proxies 返回 {}",
                response.status()
            )));
        }
        Ok(response.json::<ProxiesResponse>().await?)
    }

    /// Latency through the proxy, measured by the core itself.
    pub async fn delay(&self, proxy: &str, url: &str, timeout_ms: u64) -> Option<u32> {
        let path = format!(
            "/proxies/{}/delay?timeout={}&url={}",
            urlencoding(proxy),
            timeout_ms,
            urlencoding(url)
        );
        let response = self
            .request(reqwest::Method::GET, &path)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let value: serde_json::Value = response.json().await.ok()?;
        value
            .get("delay")
            .and_then(|d| d.as_u64())
            .map(|d| d as u32)
    }

    pub async fn select(&self, group: &str, proxy: &str) -> Result<()> {
        let path = format!("/proxies/{}", urlencoding(group));
        let response = self
            .request(reqwest::Method::PUT, &path)
            .json(&serde_json::json!({ "name": proxy }))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "切换节点失败：HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    pub async fn close_connection(&self, id: &str) -> Result<()> {
        let path = format!("/connections/{}", urlencoding(id));
        let response = self.request(reqwest::Method::DELETE, &path).send().await?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "断开连接失败：HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    pub async fn close_all_connections(&self) -> Result<()> {
        let response = self
            .request(reqwest::Method::DELETE, "/connections")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "断开全部连接失败：HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    pub async fn version(&self) -> Option<String> {
        let response = self
            .request(reqwest::Method::GET, "/version")
            .send()
            .await
            .ok()?;
        let value: serde_json::Value = response.json().await.ok()?;
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

fn urlencoding(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_host_prefers_domain() {
        let mut connection = ClashConnection::default();
        connection.metadata.host = "example.com".into();
        connection.metadata.destination_ip = "1.2.3.4".into();
        assert_eq!(connection.host(), "example.com");
        connection.metadata.host.clear();
        assert_eq!(connection.host(), "1.2.3.4");
    }

    #[test]
    fn parses_connections_json() {
        let json = r#"{"downloadTotal":100,"uploadTotal":50,"connections":[{"id":"abc","metadata":{"network":"tcp","type":"HTTP","host":"example.com","destinationIP":"1.2.3.4","destinationPort":"443"},"upload":10,"download":20,"chains":["node-a"],"rule":"RuleSet","rulePayload":"geosite-cn"}]}"#;
        let parsed: ConnectionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.download_total, 100);
        assert_eq!(parsed.connections[0].host(), "example.com");
        assert_eq!(parsed.connections[0].chains[0], "node-a");
    }
}
