//! Encryption-at-rest for profile and subscription payloads.
//!
//! A random 32-byte key is generated on first launch and stored next to the
//! config with 0600 permissions (or via `XROSE_SECRET_KEY` for headless use).
//! This is not a substitute for an OS keychain, but it keeps tokens and node
//! passwords out of plaintext database files and backups.

use std::fs;
use std::io::Write;
use std::path::Path;

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Generate, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, io_context};

const PREFIX: &str = "enc:v1:";

#[derive(Clone)]
pub struct SecretBox {
    cipher: ChaCha20Poly1305,
    hash_key: [u8; 32],
}

impl SecretBox {
    pub fn load_or_create(key_path: &Path) -> Result<Self> {
        let key = if let Ok(encoded) = std::env::var("XROSE_SECRET_KEY") {
            decode_key(&encoded)?
        } else if key_path.exists() {
            let text = fs::read_to_string(key_path).map_err(|e| io_context(key_path, e))?;
            decode_key(text.trim())?
        } else {
            let key = Key::generate();
            if let Some(parent) = key_path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_context(parent, e))?;
            }
            let mut options = fs::OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(key_path)
                .map_err(|e| io_context(key_path, e))?;
            file.write_all(hex::encode(key).as_bytes())
                .map_err(|e| io_context(key_path, e))?;
            key
        };
        let mut hasher = Sha256::new();
        hasher.update(b"xrose-keyed-hash-v1:");
        hasher.update(key.as_slice());
        let hash_key: [u8; 32] = hasher.finalize().into();
        let cipher = ChaCha20Poly1305::new(&key);
        Ok(Self { cipher, hash_key })
    }

    /// Computes a domain-separated, deterministic keyed hash without leaking `value`.
    pub fn keyed_hash(&self, namespace: &str, value: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.hash_key);
        hasher.update(namespace.as_bytes());
        hasher.update(b":");
        hasher.update(value.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn encrypt(&self, plaintext: &str) -> String {
        let nonce = Nonce::generate();
        match self.cipher.encrypt(&nonce, plaintext.as_bytes()) {
            Ok(ciphertext) => {
                let mut payload = nonce.to_vec();
                payload.extend_from_slice(&ciphertext);
                format!(
                    "{PREFIX}{}",
                    base64::engine::general_purpose::STANDARD.encode(payload)
                )
            }
            Err(_) => plaintext.to_string(),
        }
    }

    pub fn decrypt(&self, value: &str) -> Result<String> {
        let Some(encoded) = value.strip_prefix(PREFIX) else {
            return Ok(value.to_string());
        };
        let payload = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| Error::other(format!("解密失败：{e}")))?;
        if payload.len() < 12 {
            return Err(Error::other("加密数据长度无效"));
        }
        let (nonce, ciphertext) = payload.split_at(12);
        let nonce = Nonce::try_from(nonce).map_err(|_| Error::other("加密数据长度无效"))?;
        let plaintext = self
            .cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| Error::other("解密失败：密钥不匹配或数据损坏"))?;
        String::from_utf8(plaintext).map_err(|e| Error::other(format!("解密后不是 UTF-8：{e}")))
    }

    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with(PREFIX)
    }
}

fn decode_key(encoded: &str) -> Result<Key> {
    let bytes = hex::decode(encoded.trim())
        .map_err(|_| Error::other("密钥格式错误（需要 64 位十六进制）"))?;
    Key::try_from(bytes.as_slice()).map_err(|_| Error::other("密钥长度错误（需要 32 字节）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let secret = SecretBox::load_or_create(&dir.path().join("key")).unwrap();
        let encrypted = secret.encrypt("top secret");
        assert!(SecretBox::is_encrypted(&encrypted));
        assert!(!encrypted.contains("top secret"));
        assert_eq!(secret.decrypt(&encrypted).unwrap(), "top secret");
        assert_eq!(secret.decrypt("plain").unwrap(), "plain");
    }

    #[test]
    fn key_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let first = SecretBox::load_or_create(&path).unwrap();
        let second = SecretBox::load_or_create(&path).unwrap();
        let value = first.encrypt("x");
        assert_eq!(second.decrypt(&value).unwrap(), "x");
        let h1 = first.keyed_hash("egress-v1", "198.51.100.24");
        let h2 = second.keyed_hash("egress-v1", "198.51.100.24");
        let h_other_ns = second.keyed_hash("other-ns", "198.51.100.24");
        assert_eq!(h1, h2);
        assert_ne!(h1, h_other_ns);
        assert!(!h1.contains("198.51.100.24"));
    }
}
