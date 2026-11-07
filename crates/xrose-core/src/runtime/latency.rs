//! TCP ping, concurrent testing and through-proxy delay helpers.

use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyResult {
    pub ok: bool,
    pub delay_ms: Option<u32>,
    pub error: Option<String>,
}

impl LatencyResult {
    pub fn ok(delay_ms: u32) -> Self {
        Self {
            ok: true,
            delay_ms: Some(delay_ms),
            error: None,
        }
    }
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            delay_ms: None,
            error: Some(error.into()),
        }
    }
    pub fn label(&self) -> String {
        match self.delay_ms {
            Some(ms) => format!("{ms} ms"),
            None => "超时".into(),
        }
    }
}

/// Measures TCP connect time (DNS resolution included).
pub async fn tcping(host: &str, port: u16, timeout: Duration) -> LatencyResult {
    let target = crate::util::host_port(host, port);
    let started = Instant::now();
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&target)).await {
        Ok(Ok(_stream)) => {
            let elapsed = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
            LatencyResult::ok(elapsed)
        }
        Ok(Err(err)) => LatencyResult::failed(format!("连接失败：{err}")),
        Err(_) => LatencyResult::failed("连接超时"),
    }
}

/// Tests many endpoints concurrently. `targets` is `(key, host, port)`.
pub async fn test_many(
    targets: Vec<(String, String, u16)>,
    concurrency: usize,
    timeout: Duration,
) -> Vec<(String, LatencyResult)> {
    stream::iter(targets)
        .map(|(key, host, port)| async move { (key, tcping(&host, port, timeout).await) })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn localhost_tcping_works() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let result = tcping("127.0.0.1", port, Duration::from_secs(2)).await;
        assert!(result.ok);
        assert!(result.delay_ms.unwrap() < 1000);
    }

    #[tokio::test]
    async fn unreachable_is_reported() {
        let result = tcping("127.0.0.1", 1, Duration::from_millis(200)).await;
        assert!(!result.ok);
        assert!(result.error.is_some());
    }
}
