//! Minimal-privilege IPC boundary for operations that need root/admin.
//!
//! The GUI never runs as root. A small helper can be installed as a Windows
//! service / macOS LaunchDaemon / Linux setcap binary. They talk over a local
//! socket (Unix domain socket or Windows named pipe) using newline-delimited
//! JSON. The server side is provided for the helper binary; the GUI only ever
//! sends the operations it needs.

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const SOCKET_NAME: &str = "io.xross.helper.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HelperRequest {
    Ping,
    Status,
    /// Create the TUN device ahead of time (Windows wintun helper).
    PrepareTun {
        interface: String,
    },
    RemoveTun {
        interface: String,
    },
    SetSystemDns {
        servers: Vec<String>,
    },
    RestoreSystemDns,
    /// Install nftables/iptables redirect rules for tproxy mode.
    ApplyTransparentRules {
        port: u16,
    },
    ClearTransparentRules,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HelperResponse {
    Ok { message: String },
    Unsupported { message: String },
    Error { message: String },
}

impl HelperResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self::Ok {
            message: message.into(),
        }
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported {
            message: message.into(),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
    pub fn is_ok(&self) -> bool {
        matches!(self, HelperResponse::Ok { .. })
    }
    pub fn message(&self) -> &str {
        match self {
            HelperResponse::Ok { message }
            | HelperResponse::Unsupported { message }
            | HelperResponse::Error { message } => message,
        }
    }
}

fn socket_name() -> Result<interprocess::local_socket::Name<'static>> {
    SOCKET_NAME
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| Error::other(format!("无效的 helper socket 名：{e}")))
}

/// Calls the privileged helper. Fails fast when it is not installed.
pub fn call(request: &HelperRequest) -> Result<HelperResponse> {
    let name = socket_name()?;
    let mut stream = Stream::connect(name)
        .map_err(|e| Error::other(format!("无法连接特权助手（未安装或未运行）：{e}")))?;
    // The interprocess stream uses recv/send naming on all platforms.
    stream.set_recv_timeout(Some(Duration::from_secs(10))).ok();
    let line = serde_json::to_string(request)?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| Error::other(format!("发送请求失败：{e}")))?;
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| Error::other(format!("读取助手响应失败：{e}")))?;
    serde_json::from_str(response.trim()).map_err(|e| Error::other(format!("助手响应无效：{e}")))
}

/// Runs the helper server. In the real helper this is the privileged binary;
/// it is also used by tests.
pub fn serve<F>(handler: F) -> Result<()>
where
    F: FnMut(HelperRequest) -> HelperResponse + Send + 'static,
{
    let name = socket_name()?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()
        .map_err(|e| Error::other(format!("创建 helper socket 失败：{e}")))?;
    let handler = Arc::new(Mutex::new(handler));
    for connection in listener {
        let Ok(stream) = connection else { continue };
        let handler = handler.clone();
        std::thread::spawn(move || {
            let mut stream = stream;
            let mut line = String::new();
            {
                let mut reader = BufReader::new(&mut stream);
                if reader.read_line(&mut line).is_err() {
                    return;
                }
            }
            let response = match serde_json::from_str::<HelperRequest>(line.trim()) {
                Ok(request) => (handler.lock().unwrap())(request),
                Err(err) => HelperResponse::error(format!("请求解析失败：{err}")),
            };
            let _ = stream.write_all(
                format!("{}\n", serde_json::to_string(&response).unwrap_or_default()).as_bytes(),
            );
        });
    }
    Ok(())
}

/// True when the helper answers `Ping`.
pub fn helper_available() -> bool {
    matches!(call(&HelperRequest::Ping), Ok(HelperResponse::Ok { .. }))
}

pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        libc_geteuid() == 0
    }
    #[cfg(windows)]
    {
        std::process::Command::new("net")
            .arg("session")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(unix)]
fn libc_geteuid() -> u32 {
    // Avoid pulling in libc as a direct dependency for this one call.
    extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}
