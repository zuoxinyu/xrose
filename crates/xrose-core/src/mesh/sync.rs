//! Versioned discovery and explicitly shared configuration over mesh TCP.
pub mod data;

use crate::{Error, Result};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, Generate, KeyInit},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PORT: u16 = 46973;
const MAGIC: &[u8; 6] = b"XROSE\x01";
const MAX_DATA: usize = 8 * 1024 * 1024;
const MAX_HELLO: usize = 8192;
const TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub version: String,
    pub os: String,
    pub subnets: Vec<String>,
    #[serde(default)]
    pub open_share: bool,
}

impl Hello {
    pub fn local(subnets: Vec<String>) -> Self {
        Self {
            version: crate::service::APP_VERSION.into(),
            os: std::env::consts::OS.into(),
            subnets,
            open_share: false,
        }
    }
    fn validate(&self) -> Result<()> {
        if semver::Version::parse(&self.version).is_err()
            || self.version.len() > 64
            || self.os.len() > 64
            || !self
                .os
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.subnets.len() > 128
            || self
                .subnets
                .iter()
                .any(|s| super::coexist::safe_network(s).is_err())
        {
            return Err(Error::config("对端返回的发现信息无效"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    #[default]
    SharingCode,
    Unauthenticated,
}

pub(crate) struct Offer {
    pub profile: String,
    pub expires: Instant,
    pub mode: ShareMode,
    key: Key,
    payload: Vec<u8>,
}
fn proof_key(key: &Key) -> ChaCha20Poly1305 {
    let mut hash = Sha256::new();
    hash.update(b"xrose mesh sync proof v1");
    hash.update(key);
    ChaCha20Poly1305::new_from_slice(&hash.finalize()).expect("SHA256 is a 32 byte key")
}
impl Offer {
    pub fn new(profile: String, bundle: &data::Bundle, mode: ShareMode) -> Result<Self> {
        let raw = serde_json::to_vec(bundle)?;
        if raw.len() > MAX_DATA {
            return Err(Error::config("共享数据超过 8 MiB，请减少所选类别"));
        }
        let key = Key::generate();
        let nonce = Nonce::generate();
        let mut sealed = nonce.to_vec();
        sealed.extend(
            ChaCha20Poly1305::new(&key)
                .encrypt(&nonce, raw.as_ref())
                .map_err(|_| Error::other("共享数据加密失败"))?,
        );
        Ok(Self {
            profile,
            expires: Instant::now() + TTL,
            mode,
            key,
            payload: sealed,
        })
    }
    pub fn code(&self) -> Option<String> {
        (self.mode == ShareMode::SharingCode).then(|| hex::encode(self.key))
    }
}
async fn read_frame(stream: &mut TcpStream, limit: usize) -> Result<Vec<u8>> {
    let n = stream.read_u32().await? as usize;
    if n == 0 || n > limit {
        return Err(Error::config("对端数据长度无效"));
    }
    let mut data = vec![0; n];
    stream.read_exact(&mut data).await?;
    Ok(data)
}
async fn frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    Ok(())
}
async fn request(s: &mut TcpStream, kind: u8) -> Result<()> {
    s.write_all(MAGIC).await?;
    s.write_u8(kind).await?;
    Ok(())
}
pub(crate) async fn discover(mut s: TcpStream) -> Result<Hello> {
    request(&mut s, 0).await?;
    let mut magic = [0; 6];
    s.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(Error::config("设备未提供兼容的 Xrose 发现服务"));
    }
    let hello: Hello = serde_json::from_slice(&read_frame(&mut s, MAX_HELLO).await?)?;
    hello.validate()?;
    Ok(hello)
}
// The userspace Tailscale endpoint is the only caller allowed on the app's loopback relay.
pub(crate) async fn accept_relay(s: &mut TcpStream, password: &str) -> Result<()> {
    let (command, target) = super::service_gateway::read_request(s, password).await?;
    if command != 1 || !target.ip().is_loopback() || target.port() != PORT {
        s.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Err(Error::config("组网同步转发请求无效"));
    }
    s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).await?;
    Ok(())
}

pub(crate) async fn serve(
    mut s: TcpStream,
    mut hello: Hello,
    offer: impl Fn() -> Option<Arc<Offer>>,
) -> Result<()> {
    let mut magic = [0; 6];
    s.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Ok(());
    }
    let active_offer = || offer().filter(|o| o.expires > Instant::now());
    match s.read_u8().await? {
        0 => {
            hello.open_share = active_offer().is_some_and(|o| o.mode == ShareMode::Unauthenticated);
            s.write_all(MAGIC).await?;
            frame(&mut s, &serde_json::to_vec(&hello)?).await?;
        }
        1 => {
            let challenge = Nonce::generate();
            s.write_all(&challenge).await?;
            let mut tag = [0; 16];
            s.read_exact(&mut tag).await?;
            if let Some(offer) = active_offer()
                && offer.mode == ShareMode::SharingCode
                && proof_key(&offer.key).decrypt(&challenge, &tag[..]).is_ok()
            {
                frame(&mut s, &offer.payload).await?;
            } else {
                s.write_u32(0).await?;
            }
        }
        2 => {
            let init = read_frame(&mut s, 32).await?;
            if let Some(offer) = active_offer().filter(|o| o.mode == ShareMode::Unauthenticated) {
                let mut handshake = open_handshake()?
                    .build_responder()
                    .map_err(handshake_error)?;
                handshake
                    .read_message(&init, &mut [])
                    .map_err(handshake_error)?;
                let mut reply = [0; OPEN_REPLY_LEN];
                let len = handshake
                    .write_message(&offer.key, &mut reply)
                    .map_err(handshake_error)?;
                frame(&mut s, &reply[..len]).await?;
                frame(&mut s, &offer.payload).await?;
            } else {
                s.write_u32(0).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Noise NN encrypts the offer key with ephemeral keys from both peers.
const OPEN_REPLY_LEN: usize = 32 + 32 + 16;
fn handshake_error(_: snow::Error) -> Error {
    Error::other("无认证共享握手失败，请重新接收")
}
fn open_handshake() -> Result<snow::Builder<'static>> {
    let params = "Noise_NN_25519_ChaChaPoly_SHA256"
        .parse()
        .map_err(handshake_error)?;
    snow::Builder::new(params)
        .prologue(b"xrose mesh sync open v1")
        .map_err(handshake_error)
}

async fn open_key(s: &mut TcpStream) -> Result<Key> {
    let mut handshake = open_handshake()?
        .build_initiator()
        .map_err(handshake_error)?;
    let mut init = [0; OPEN_REPLY_LEN];
    let len = handshake
        .write_message(&[], &mut init)
        .map_err(handshake_error)?;
    request(s, 2).await?;
    frame(s, &init[..len]).await?;
    let reply = read_frame(s, OPEN_REPLY_LEN)
        .await
        .map_err(|_| Error::other("未能接收无认证共享，请确认对端已开启共享，或输入共享码重试"))?;
    let mut key = Key::default();
    let len = handshake
        .read_message(&reply, &mut key)
        .map_err(handshake_error)?;
    if len != key.len() {
        return Err(Error::config("无认证共享密钥无效"));
    }
    Ok(key)
}

pub(crate) async fn fetch(mut s: TcpStream, code: &str) -> Result<data::Bundle> {
    let open = code.trim().is_empty();
    let key = if open {
        open_key(&mut s).await?
    } else {
        authenticated_key(&mut s, code).await?
    };
    let sealed = read_frame(&mut s, MAX_DATA + 28).await.map_err(|_| {
        if open {
            Error::other("无认证共享传输中断，请重新接收")
        } else {
            Error::other("共享码无效、已过期或传输中断，请重新获取")
        }
    })?;
    let (nonce, body) = sealed
        .split_at_checked(12)
        .ok_or_else(|| Error::config("共享数据不完整"))?;
    let raw = ChaCha20Poly1305::new(&key)
        .decrypt(&Nonce::try_from(nonce).unwrap(), body)
        .map_err(|_| Error::other("共享数据校验失败"))?;
    let bundle: data::Bundle =
        serde_json::from_slice(&raw).map_err(|_| Error::config("共享配置格式无效"))?;
    bundle.validate()?;
    Ok(bundle)
}

async fn authenticated_key(s: &mut TcpStream, code: &str) -> Result<Key> {
    let key = hex::decode(code.trim())
        .ok()
        .and_then(|v| Key::try_from(v.as_slice()).ok())
        .ok_or_else(|| Error::config("请粘贴发送端生成的完整共享码"))?;
    request(s, 1).await?;
    let mut challenge = [0; 12];
    s.read_exact(&mut challenge).await?;
    let proof = proof_key(&key)
        .encrypt(&Nonce::from(challenge), &[][..])
        .map_err(|_| Error::other("共享验证失败"))?;
    s.write_all(&proof).await?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppModel, Node, Protocol};
    use tokio::net::TcpListener;
    #[tokio::test]
    async fn discovery_and_authenticated_pull_do_not_consume_or_reveal_unselected_data() {
        let mut model = AppModel::default();
        let selected = Node::new("fixture", Protocol::Socks, "example.test", 1080);
        let key = format!("node:{}", selected.id);
        model.add_nodes([
            selected,
            Node::new("omitted", Protocol::Socks, "private.example.test", 1081),
        ]);
        let sub = crate::subscription::Subscription::new("omitted", "https://example.test/fixture");
        let catalog = data::Bundle::capture(&model, &[sub], &[], &data::Category::ALL.into());
        assert_eq!(catalog.catalog().len(), 3);
        let bundle = catalog.selected(&[key].into()).unwrap();
        assert_eq!(bundle.items.len(), 1);
        let offer =
            Arc::new(Offer::new("network".into(), &bundle, ShareMode::SharingCode).unwrap());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let code = offer.code().unwrap();
        let server = tokio::spawn(async move {
            for available in [true, true, true, true, false] {
                let (s, _) = listener.accept().await.unwrap();
                serve(s, Hello::local(vec!["192.0.2.0/24".into()]), || {
                    available.then(|| offer.clone())
                })
                .await
                .unwrap();
            }
        });
        let hello = discover(TcpStream::connect(addr).await.unwrap())
            .await
            .unwrap();
        assert_eq!(hello.subnets, ["192.0.2.0/24"]);
        assert!(!hello.open_share);
        assert!(
            fetch(TcpStream::connect(addr).await.unwrap(), "")
                .await
                .is_err()
        );
        assert!(
            fetch(TcpStream::connect(addr).await.unwrap(), &"00".repeat(32))
                .await
                .is_err()
        );
        assert!(
            fetch(TcpStream::connect(addr).await.unwrap(), &code)
                .await
                .unwrap()
                == bundle
        );
        assert!(
            fetch(TcpStream::connect(addr).await.unwrap(), &code)
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn open_sharing_advertises_and_serves_only_the_current_selection_until_revoked_or_expired()
     {
        let mut model = AppModel::default();
        let node = Node::new("Shared", Protocol::Socks, "shared.example.test", 1080);
        let selected = std::collections::BTreeSet::from([format!("node:{}", node.id)]);
        model.add_nodes([
            node,
            Node::new("Local", Protocol::Socks, "local.example.test", 1080),
        ]);
        let catalog = data::Bundle::capture(&model, &[], &[], &data::Category::ALL.into());
        let bundle = catalog.selected(&selected).unwrap();
        let offer =
            Arc::new(Offer::new("network".into(), &bundle, ShareMode::Unauthenticated).unwrap());
        assert!(offer.code().is_none());
        let mut expired =
            Offer::new("network".into(), &bundle, ShareMode::Unauthenticated).unwrap();
        expired.expires = Instant::now();
        let expired = Arc::new(expired);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let current = parking_lot::Mutex::new(None);
        let server = async {
            for _ in 0..6 {
                let (s, _) = listener.accept().await.unwrap();
                serve(s, Hello::local(vec![]), || current.lock().clone())
                    .await
                    .unwrap();
            }
        };
        let client = async {
            for next in [Some(offer), None, Some(expired)] {
                let available = next.as_ref().is_some_and(|o| o.expires > Instant::now());
                *current.lock() = next;
                let hello = discover(TcpStream::connect(addr).await.unwrap())
                    .await
                    .unwrap();
                assert_eq!(hello.open_share, available);
                let result = fetch(TcpStream::connect(addr).await.unwrap(), "").await;
                if available {
                    assert!(result.unwrap() == bundle);
                } else {
                    assert!(result.is_err());
                }
            }
        };
        tokio::join!(server, client);
    }

    #[tokio::test]
    async fn local_relay_accepts_only_authenticated_sync_connections() {
        let password = uuid::Uuid::new_v4().simple().to_string();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = async {
            for expected in [false, false, true] {
                let (mut s, _) = listener.accept().await.unwrap();
                assert_eq!(accept_relay(&mut s, &password).await.is_ok(), expected);
            }
        };
        let client = async {
            let mut bare = TcpStream::connect(addr).await.unwrap();
            request(&mut bare, 2).await.unwrap();
            drop(bare);
            for port in [PORT - 1, PORT] {
                let mut s = TcpStream::connect(addr).await.unwrap();
                let target = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let auth = async_socks5::Auth::new("xrose", &password);
                assert_eq!(
                    async_socks5::connect(&mut s, target, Some(auth))
                        .await
                        .is_ok(),
                    port == PORT
                );
            }
        };
        tokio::join!(server, client);
    }
}
