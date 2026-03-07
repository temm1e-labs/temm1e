//! LocalVault — file-backed encrypted vault using ChaCha20-Poly1305.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use skyclaw_core::Vault;
use skyclaw_core::types::error::SkyclawError;

/// On-disk representation of a single encrypted secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSecret {
    /// 12-byte nonce, base64-encoded
    nonce: String,
    /// Ciphertext, base64-encoded
    ciphertext: String,
    /// ISO-8601 creation timestamp
    created_at: String,
    /// ISO-8601 last-update timestamp
    updated_at: String,
}

/// A local, file-backed vault that encrypts secrets with ChaCha20-Poly1305.
///
/// - Vault file: `~/.skyclaw/vault.enc` (JSON map of key -> StoredSecret)
/// - Key file:   `~/.skyclaw/vault.key` (32 raw bytes)
pub struct LocalVault {
    vault_path: PathBuf,
    key_path: PathBuf,
    /// In-memory cache of the vault contents, protected by an async RwLock.
    cache: RwLock<HashMap<String, StoredSecret>>,
}

impl LocalVault {
    /// Create (or open) a local vault in the default location (`~/.skyclaw/`).
    pub async fn new() -> Result<Self, SkyclawError> {
        let base = dirs::home_dir()
            .ok_or_else(|| SkyclawError::Vault("cannot determine home directory".into()))?
            .join(".skyclaw");

        Self::with_dir(base).await
    }

    /// Create (or open) a local vault in a custom directory.
    pub async fn with_dir(dir: PathBuf) -> Result<Self, SkyclawError> {
        tokio::fs::create_dir_all(&dir).await.map_err(|e| {
            SkyclawError::Vault(format!("failed to create vault directory: {e}"))
        })?;

        let vault_path = dir.join("vault.enc");
        let key_path = dir.join("vault.key");

        let vault = Self {
            vault_path,
            key_path,
            cache: RwLock::new(HashMap::new()),
        };

        // Ensure the encryption key exists (generate on first use).
        vault.ensure_key().await?;

        // Load existing secrets into cache.
        vault.load().await?;

        Ok(vault)
    }

    // ── Key management ──────────────────────────────────────────────────

    /// Ensure `vault.key` exists; generate a random 32-byte key if not.
    async fn ensure_key(&self) -> Result<(), SkyclawError> {
        if tokio::fs::try_exists(&self.key_path).await.unwrap_or(false) {
            return Ok(());
        }

        let mut key_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut key_bytes);

        tokio::fs::write(&self.key_path, &key_bytes).await.map_err(|e| {
            SkyclawError::Vault(format!("failed to write vault key: {e}"))
        })?;

        // Best-effort: restrict permissions to owner-only on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = tokio::fs::set_permissions(&self.key_path, perms).await;
        }

        debug!("generated new vault key at {:?}", self.key_path);
        Ok(())
    }

    /// Read the raw 32-byte key from disk.
    async fn read_key(&self) -> Result<[u8; 32], SkyclawError> {
        let bytes = tokio::fs::read(&self.key_path).await.map_err(|e| {
            SkyclawError::Vault(format!("failed to read vault key: {e}"))
        })?;

        let key: [u8; 32] = bytes.try_into().map_err(|_| {
            SkyclawError::Vault("vault key must be exactly 32 bytes".into())
        })?;

        Ok(key)
    }

    // ── Encryption helpers ──────────────────────────────────────────────

    fn make_cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(key.into())
    }

    fn encrypt(cipher: &ChaCha20Poly1305, plaintext: &[u8]) -> Result<(Vec<u8>, [u8; 12]), SkyclawError> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| SkyclawError::Vault(format!("encryption failed: {e}")))?;

        Ok((ciphertext, nonce_bytes))
    }

    fn decrypt(
        cipher: &ChaCha20Poly1305,
        nonce_bytes: &[u8; 12],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, SkyclawError> {
        let nonce = Nonce::from_slice(nonce_bytes);
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| SkyclawError::Vault(format!("decryption failed: {e}")))?
            .pipe_ok()
    }

    // ── Persistence ─────────────────────────────────────────────────────

    /// Load the on-disk vault file into the in-memory cache.
    async fn load(&self) -> Result<(), SkyclawError> {
        if !tokio::fs::try_exists(&self.vault_path).await.unwrap_or(false) {
            return Ok(());
        }

        let data = tokio::fs::read_to_string(&self.vault_path).await.map_err(|e| {
            SkyclawError::Vault(format!("failed to read vault file: {e}"))
        })?;

        if data.trim().is_empty() {
            return Ok(());
        }

        let map: HashMap<String, StoredSecret> = serde_json::from_str(&data)
            .map_err(|e| SkyclawError::Vault(format!("corrupt vault file: {e}")))?;

        let mut cache = self.cache.write().await;
        *cache = map;

        Ok(())
    }

    /// Flush the in-memory cache to disk.
    async fn flush(&self) -> Result<(), SkyclawError> {
        let cache = self.cache.read().await;
        let json = serde_json::to_string_pretty(&*cache)?;
        drop(cache);

        tokio::fs::write(&self.vault_path, json.as_bytes()).await.map_err(|e| {
            SkyclawError::Vault(format!("failed to write vault file: {e}"))
        })?;

        Ok(())
    }

    // ── URI parsing ─────────────────────────────────────────────────────

    /// Parse a `vault://skyclaw/<key>` URI and return the key portion.
    fn parse_vault_uri(uri: &str) -> Result<String, SkyclawError> {
        let rest = uri
            .strip_prefix("vault://skyclaw/")
            .ok_or_else(|| SkyclawError::Vault(format!("invalid vault URI: {uri}")))?;

        if rest.is_empty() {
            return Err(SkyclawError::Vault("vault URI has empty key".into()));
        }

        Ok(rest.to_string())
    }
}

/// Small helper to avoid writing `Ok(value)` chains.
trait PipeOk: Sized {
    fn pipe_ok(self) -> Result<Self, SkyclawError> {
        Ok(self)
    }
}
impl<T> PipeOk for T {}

#[async_trait]
impl Vault for LocalVault {
    async fn store_secret(&self, key: &str, plaintext: &[u8]) -> Result<(), SkyclawError> {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;

        let raw_key = self.read_key().await?;
        let cipher = Self::make_cipher(&raw_key);
        let (ciphertext, nonce_bytes) = Self::encrypt(&cipher, plaintext)?;

        let now = chrono::Utc::now().to_rfc3339();

        let mut cache = self.cache.write().await;

        let created_at = cache
            .get(key)
            .map(|s| s.created_at.clone())
            .unwrap_or_else(|| now.clone());

        cache.insert(
            key.to_string(),
            StoredSecret {
                nonce: engine.encode(nonce_bytes),
                ciphertext: engine.encode(&ciphertext),
                created_at,
                updated_at: now,
            },
        );
        drop(cache);

        self.flush().await?;
        debug!("stored secret: {key}");
        Ok(())
    }

    async fn get_secret(&self, key: &str) -> Result<Option<Vec<u8>>, SkyclawError> {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;

        let cache = self.cache.read().await;
        let stored = match cache.get(key) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };
        drop(cache);

        let nonce_bytes: [u8; 12] = engine
            .decode(&stored.nonce)
            .map_err(|e| SkyclawError::Vault(format!("bad nonce base64: {e}")))?
            .try_into()
            .map_err(|_| SkyclawError::Vault("nonce must be 12 bytes".into()))?;

        let ciphertext = engine
            .decode(&stored.ciphertext)
            .map_err(|e| SkyclawError::Vault(format!("bad ciphertext base64: {e}")))?;

        let raw_key = self.read_key().await?;
        let cipher = Self::make_cipher(&raw_key);
        let plaintext = Self::decrypt(&cipher, &nonce_bytes, &ciphertext)?;

        Ok(Some(plaintext))
    }

    async fn delete_secret(&self, key: &str) -> Result<(), SkyclawError> {
        let mut cache = self.cache.write().await;
        if cache.remove(key).is_none() {
            warn!("delete_secret: key not found: {key}");
        }
        drop(cache);

        self.flush().await?;
        debug!("deleted secret: {key}");
        Ok(())
    }

    async fn list_keys(&self) -> Result<Vec<String>, SkyclawError> {
        let cache = self.cache.read().await;
        let mut keys: Vec<String> = cache.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }

    async fn has_key(&self, key: &str) -> Result<bool, SkyclawError> {
        let cache = self.cache.read().await;
        Ok(cache.contains_key(key))
    }

    async fn resolve_uri(&self, uri: &str) -> Result<Option<Vec<u8>>, SkyclawError> {
        let key = Self::parse_vault_uri(uri)?;
        self.get_secret(&key).await
    }

    fn backend_name(&self) -> &str {
        "local-chacha20"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf()).await.unwrap();

        vault.store_secret("test/key", b"hello world").await.unwrap();

        assert!(vault.has_key("test/key").await.unwrap());
        assert!(!vault.has_key("missing").await.unwrap());

        let plain = vault.get_secret("test/key").await.unwrap().unwrap();
        assert_eq!(plain, b"hello world");

        let keys = vault.list_keys().await.unwrap();
        assert_eq!(keys, vec!["test/key".to_string()]);

        // resolve_uri
        let resolved = vault
            .resolve_uri("vault://skyclaw/test/key")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved, b"hello world");

        vault.delete_secret("test/key").await.unwrap();
        assert!(!vault.has_key("test/key").await.unwrap());
    }
}
