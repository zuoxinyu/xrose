use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::util::SubscriptionUserInfo;

#[derive(Debug, Clone, Default)]
pub struct FetchRequest {
    pub network: crate::updates::UpdateNetwork,
    pub url: String,
    pub user_agent: String,
    pub headers: BTreeMap<String, String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchStatus {
    Ok,
    NotModified,
}

#[derive(Debug, Clone, Default)]
pub struct FetchResponse {
    pub status: FetchStatus,
    pub body: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub userinfo: Option<SubscriptionUserInfo>,
    pub title: Option<String>,
    pub update_interval_hours: Option<u64>,
}

impl Default for FetchStatus {
    fn default() -> Self {
        FetchStatus::Ok
    }
}

pub const DEFAULT_USER_AGENT: &str = "Xross/0.1.0 (v2rayN; sing-box)";

/// Fetches a subscription with ETag / If-Modified-Since and custom headers.
pub async fn fetch_subscription(request: FetchRequest) -> Result<FetchResponse> {
    let url = request.url.trim();
    if url.is_empty() {
        return Err(Error::Subscription("订阅地址为空".into()));
    }
    let parsed =
        reqwest::Url::parse(url).map_err(|e| Error::Subscription(format!("订阅地址无效：{e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::Subscription("订阅地址必须是 http(s)".into()));
    }

    let mut client = request.network.builder(DEFAULT_USER_AGENT)?;
    if !request.user_agent.trim().is_empty() {
        client = client.user_agent(&request.user_agent);
    }
    let client = client
        .timeout(std::time::Duration::from_secs(40))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?;

    let mut builder = client.get(parsed);
    for (key, value) in &request.headers {
        if key.trim().is_empty() {
            continue;
        }
        builder = builder.header(key, value);
    }
    if let Some(etag) = request.etag.clone().filter(|e| !e.is_empty()) {
        builder = builder.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = request.last_modified.clone().filter(|e| !e.is_empty()) {
        builder = builder.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
    }

    let response = builder
        .send()
        .await
        .map_err(|e| Error::Subscription(format!("请求订阅失败：{}", e.without_url())))?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchResponse {
            status: FetchStatus::NotModified,
            etag: request.etag.clone(),
            last_modified: request.last_modified.clone(),
            ..Default::default()
        });
    }
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "（token 无效或无权限）",
            404 => "（订阅地址不存在）",
            429 => "（请求过于频繁）",
            _ => "",
        };
        return Err(Error::Subscription(format!(
            "订阅服务器返回 HTTP {}{hint}",
            status.as_u16()
        )));
    }

    let headers = response.headers().clone();
    let header_string = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let userinfo = header_string("subscription-userinfo")
        .as_deref()
        .and_then(SubscriptionUserInfo::parse);
    let title = header_string("profile-title").map(|value| {
        if let Some(decoded) = crate::util::decode_base64_any(value.trim_start_matches("base64:")) {
            String::from_utf8(decoded).unwrap_or(value.clone())
        } else {
            value
        }
    });
    let update_interval_hours =
        header_string("profile-update-interval").and_then(|v| v.parse::<u64>().ok());

    let body = response
        .text()
        .await
        .map_err(|e| Error::Subscription(format!("读取订阅内容失败：{}", e.without_url())))?;

    Ok(FetchResponse {
        status: FetchStatus::Ok,
        body,
        etag: header_string("etag"),
        last_modified: header_string("last-modified"),
        userinfo,
        title,
        update_interval_hours,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_url() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(fetch_subscription(FetchRequest::default()));
        assert!(result.is_err());
    }
}
