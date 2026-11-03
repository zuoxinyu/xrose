//! Isolated per-node HTTP probes with bounded streaming downloads.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{CoreKind, Node};

pub const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=52428800";
const DOWNLOAD_LIMIT: u64 = 50 * 1024 * 1024;
const DOWNLOAD_DURATION: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub bytes_per_second: u64,
    pub checked_at: i64,
    pub error: Option<String>,
}

/// Normal completion reaps the child; cancellation also requests termination.
struct Probe {
    child: tokio::process::Child,
    _directory: tempfile::TempDir,
    client: reqwest::Client,
}

impl Probe {
    async fn start(binary: &Path, core: CoreKind, node: &Node) -> Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let config = crate::adapters::compile_probe(core, node, port)?;
        let directory = tempfile::Builder::new().prefix("xross-probe-").tempdir()?;
        let path = directory.path().join("config.json");
        crate::util::atomic_write(&path, &serde_json::to_vec(&config)?)?;
        let mut command = tokio::process::Command::new(std::fs::canonicalize(binary)?);
        command
            .args(crate::core::run_args(core, &path))
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        drop(listener);
        let client = probe_client(port)?;
        let child = command.spawn()?;
        #[cfg(windows)]
        if let Some(handle) = child.raw_handle() {
            let _ = crate::core::job::adopt(handle);
        }
        let mut probe = Self {
            child,
            _directory: directory,
            client,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if probe.child.try_wait()?.is_some() {
                return Err(Error::process("测试内核启动失败，请检查节点配置与内核版本"));
            }
            if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
            {
                return Ok(probe);
            }
            if Instant::now() >= deadline {
                probe.stop().await;
                return Err(Error::process("测试内核启动超时"));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

fn probe_client(port: u16) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))?)
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?)
}

pub async fn http_latency(binary: &Path, core: CoreKind, node: &Node, url: &str) -> Result<u32> {
    let mut probe = Probe::start(binary, core, node).await?;
    let started = Instant::now();
    let result = probe
        .client
        .get(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    let elapsed = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    probe.stop().await;
    let response =
        result.map_err(|_| Error::other("节点 HTTP 延迟测试失败，请检查节点或测试地址"))?;
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "延迟测试地址返回 HTTP {}",
            response.status()
        )));
    }
    Ok(elapsed)
}

pub async fn download(binary: &Path, core: CoreKind, node: &Node) -> Result<DownloadResult> {
    let mut probe = Probe::start(binary, core, node).await?;
    let result = measure_download(
        &probe.client,
        DOWNLOAD_URL,
        DOWNLOAD_DURATION,
        DOWNLOAD_LIMIT,
    )
    .await;
    probe.stop().await;
    result
}

async fn measure_download(
    client: &reqwest::Client,
    url: &str,
    duration: Duration,
    limit: u64,
) -> Result<DownloadResult> {
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(|_| Error::other("下载测试连接失败，请检查节点"))?;
    if !response.status().is_success() {
        return Err(Error::other(format!(
            "下载测试地址返回 HTTP {}",
            response.status()
        )));
    }
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + duration;
    let mut bytes = 0;
    while bytes < limit {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(Some(chunk))) => bytes += (chunk.len() as u64).min(limit - bytes),
            Ok(Ok(None)) => break,
            Ok(Err(_)) => return Err(Error::other("下载测试连接中断，请重试")),
            Err(_) => break,
        }
    }
    if bytes == 0 {
        return Err(Error::other("下载测试未收到数据"));
    }
    let elapsed = started.elapsed();
    Ok(DownloadResult {
        bytes,
        elapsed_ms: elapsed.as_millis().max(1) as u64,
        bytes_per_second: (bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64,
        checked_at: crate::util::now_unix(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn fake_proxy(
        response: &'static [u8],
        stall: bool,
    ) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(response).await.unwrap();
            if stall {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            String::from_utf8(request).unwrap()
        });
        (port, task)
    }

    #[tokio::test]
    async fn download_uses_explicit_proxy_and_caps_bytes() {
        let (port, task) = fake_proxy(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nabcdefgh",
            false,
        )
        .await;
        let result = measure_download(
            &probe_client(port).unwrap(),
            "http://node-probe.invalid/file",
            Duration::from_secs(1),
            4,
        )
        .await
        .unwrap();
        assert_eq!(result.bytes, 4);
        assert!(result.bytes_per_second > 0);
        let request = task.await.unwrap();
        assert!(request.starts_with("GET http://node-probe.invalid/file HTTP/1.1"));
        assert!(request.to_lowercase().contains("accept-encoding: identity"));
    }

    #[tokio::test]
    async fn stalled_download_finishes_at_deadline_with_received_bytes() {
        let (port, task) =
            fake_proxy(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n1234", true).await;
        let started = Instant::now();
        let result = measure_download(
            &probe_client(port).unwrap(),
            "http://node-probe.invalid/file",
            Duration::from_millis(80),
            1000,
        )
        .await
        .unwrap();
        assert_eq!(result.bytes, 4);
        assert!(started.elapsed() < Duration::from_millis(800));
        assert!(result.elapsed_ms >= 70);
        task.abort();
    }

    #[tokio::test]
    async fn errors_empty_bodies_redirects_and_truncation_are_not_speed_results() {
        for response in [
            &b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere.invalid\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n1234"[..],
        ] {
            let (port, task) = fake_proxy(response, false).await;
            assert!(measure_download(&probe_client(port).unwrap(), "http://node-probe.invalid/file", Duration::from_secs(1), 1000).await.is_err());
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn failed_proxy_never_falls_back_to_direct() {
        let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = proxy.local_addr().unwrap().port();
        drop(proxy);
        let url = format!("http://{}/", origin.local_addr().unwrap());
        assert!(probe_client(port).unwrap().get(url).send().await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), origin.accept())
                .await
                .is_err()
        );
    }
}
