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
const MAGIC: &[u8; 6] = b"XROSS\x01";
const MAX_DATA: usize = 8 * 1024 * 1024;
const MAX_HELLO: usize = 8192;
const TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub version: String,
    pub os: String,
    pub subnets: Vec<String>,
}

impl Hello {
    pub fn local(subnets: Vec<String>) -> Self {
        Self {
            version: crate::service::APP_VERSION.into(),
            os: std::env::consts::OS.into(),
            subnets,
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

pub(crate) struct Offer {
    pub profile: String,
    pub expires: Instant,
    code: String,
    proof: ChaCha20Poly1305,
    payload: Arc<Vec<u8>>,
}
fn proof_key(key: &Key) -> ChaCha20Poly1305 {
    let mut hash = Sha256::new();
    hash.update(b"xross mesh sync proof v1");
    hash.update(key);
    ChaCha20Poly1305::new_from_slice(&hash.finalize()).expect("SHA256 is a 32 byte key")
}
impl Offer {
    pub fn new(profile: String, bundle: &data::Bundle) -> Result<Self> {
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
            code: hex::encode(key),
            proof: proof_key(&key),
            payload: Arc::new(sealed),
        })
    }
    pub fn code(&self) -> String {
        self.code.clone()
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
        return Err(Error::config("设备未提供兼容的 Xross 发现服务"));
    }
    let hello: Hello = serde_json::from_slice(&read_frame(&mut s, MAX_HELLO).await?)?;
    hello.validate()?;
    Ok(hello)
}
pub(crate) async fn serve(
    mut s: TcpStream,
    hello: Hello,
    offer: impl Fn() -> Option<Arc<Offer>>,
) -> Result<()> {
    let mut magic = [0; 6];
    s.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Ok(());
    }
    match s.read_u8().await? {
        0 => {
            s.write_all(MAGIC).await?;
            frame(&mut s, &serde_json::to_vec(&hello)?).await?;
        }
        1 => {
            let challenge = Nonce::generate();
            s.write_all(&challenge).await?;
            let mut tag = [0; 16];
            s.read_exact(&mut tag).await?;
            if let Some(offer) = offer().filter(|o| o.expires > Instant::now())
                && offer.proof.decrypt(&challenge, &tag[..]).is_ok()
            {
                frame(&mut s, &offer.payload).await?;
            } else {
                s.write_u32(0).await?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(crate) async fn fetch(mut s: TcpStream, code: &str) -> Result<data::Bundle> {
    let key = hex::decode(code.trim())
        .ok()
        .and_then(|v| Key::try_from(v.as_slice()).ok())
        .ok_or_else(|| Error::config("请粘贴发送端生成的完整共享码"))?;
    request(&mut s, 1).await?;
    let mut challenge = [0; 12];
    s.read_exact(&mut challenge).await?;
    let proof = proof_key(&key)
        .encrypt(&Nonce::from(challenge), &[][..])
        .map_err(|_| Error::other("共享验证失败"))?;
    s.write_all(&proof).await?;
    let sealed = read_frame(&mut s, MAX_DATA + 28)
        .await
        .map_err(|_| Error::other("共享码无效、已过期或传输中断，请重新获取"))?;
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
        let offer = Arc::new(Offer::new("network".into(), &bundle).unwrap());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let code = offer.code();
        let server = tokio::spawn(async move {
            for available in [true, true, true, false] {
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
}
