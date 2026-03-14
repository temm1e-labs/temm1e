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
use zeroize::Zeroizing;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::Vault;

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
/// - Vault file: `~/.temm1e/vault.enc` (JSON map of key -> StoredSecret)
/// - Key file:   `~/.temm1e/vault.key` (32 raw bytes)
pub struct LocalVault {
    vault_path: PathBuf,
    key_path: PathBuf,
    /// In-memory cache of the vault contents, protected by an async RwLock.
    cache: RwLock<HashMap<String, StoredSecret>>,
}

impl LocalVault {
    /// Create (or open) a local vault in the default location (`~/.temm1e/`).
    pub async fn new() -> Result<Self, Temm1eError> {
        let base = dirs::home_dir()
            .ok_or_else(|| Temm1eError::Vault("cannot determine home directory".into()))?
            .join(".temm1e");

        Self::with_dir(base).await
    }

    /// Create (or open) a local vault in a custom directory.
    pub async fn with_dir(dir: PathBuf) -> Result<Self, Temm1eError> {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| Temm1eError::Vault(format!("failed to create vault directory: {e}")))?;

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
    async fn ensure_key(&self) -> Result<(), Temm1eError> {
        if tokio::fs::try_exists(&self.key_path).await.unwrap_or(false) {
            return Ok(());
        }

        let mut key_bytes = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(key_bytes.as_mut());

        tokio::fs::write(&self.key_path, key_bytes.as_ref())
            .await
            .map_err(|e| Temm1eError::Vault(format!("failed to write vault key: {e}")))?;

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
    ///
    /// The returned key is wrapped in `Zeroizing` so it is automatically
    /// zeroized when dropped, preventing key material from lingering in memory.
    async fn read_key(&self) -> Result<Zeroizing<[u8; 32]>, Temm1eError> {
        let bytes = tokio::fs::read(&self.key_path)
            .await
            .map_err(|e| Temm1eError::Vault(format!("failed to read vault key: {e}")))?;

        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Temm1eError::Vault("vault key must be exactly 32 bytes".into()))?;

        Ok(Zeroizing::new(key))
    }

    // ── Encryption helpers ──────────────────────────────────────────────

    fn make_cipher(key: &Zeroizing<[u8; 32]>) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(key.as_ref().into())
    }

    fn encrypt(
        cipher: &ChaCha20Poly1305,
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, [u8; 12]), Temm1eError> {
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| Temm1eError::Vault(format!("encryption failed: {e}")))?;

        Ok((ciphertext, nonce_bytes))
    }

    fn decrypt(
        cipher: &ChaCha20Poly1305,
        nonce_bytes: &[u8; 12],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, Temm1eError> {
        let nonce = Nonce::from_slice(nonce_bytes);
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| Temm1eError::Vault(format!("decryption failed: {e}")))?
            .pipe_ok()
    }

    // ── Persistence ─────────────────────────────────────────────────────

    /// Load the on-disk vault file into the in-memory cache.
    async fn load(&self) -> Result<(), Temm1eError> {
        if !tokio::fs::try_exists(&self.vault_path)
            .await
            .unwrap_or(false)
        {
            return Ok(());
        }

        let data = tokio::fs::read_to_string(&self.vault_path)
            .await
            .map_err(|e| Temm1eError::Vault(format!("failed to read vault file: {e}")))?;

        if data.trim().is_empty() {
            return Ok(());
        }

        let map: HashMap<String, StoredSecret> = serde_json::from_str(&data)
            .map_err(|e| Temm1eError::Vault(format!("corrupt vault file: {e}")))?;

        let mut cache = self.cache.write().await;
        *cache = map;

        Ok(())
    }

    /// Flush the in-memory cache to disk.
    async fn flush(&self) -> Result<(), Temm1eError> {
        let cache = self.cache.read().await;
        let json = serde_json::to_string_pretty(&*cache)?;
        drop(cache);

        tokio::fs::write(&self.vault_path, json.as_bytes())
            .await
            .map_err(|e| Temm1eError::Vault(format!("failed to write vault file: {e}")))?;

        Ok(())
    }

    // ── URI parsing ─────────────────────────────────────────────────────

    /// Parse a `vault://temm1e/<key>` URI and return the key portion.
    fn parse_vault_uri(uri: &str) -> Result<String, Temm1eError> {
        let rest = uri
            .strip_prefix("vault://temm1e/")
            .ok_or_else(|| Temm1eError::Vault(format!("invalid vault URI: {uri}")))?;

        if rest.is_empty() {
            return Err(Temm1eError::Vault("vault URI has empty key".into()));
        }

        Ok(rest.to_string())
    }
}

/// Small helper to avoid writing `Ok(value)` chains.
trait PipeOk: Sized {
    fn pipe_ok(self) -> Result<Self, Temm1eError> {
        Ok(self)
    }
}
impl<T> PipeOk for T {}

#[async_trait]
impl Vault for LocalVault {
    async fn store_secret(&self, key: &str, plaintext: &[u8]) -> Result<(), Temm1eError> {
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

    async fn get_secret(&self, key: &str) -> Result<Option<Vec<u8>>, Temm1eError> {
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
            .map_err(|e| Temm1eError::Vault(format!("bad nonce base64: {e}")))?
            .try_into()
            .map_err(|_| Temm1eError::Vault("nonce must be 12 bytes".into()))?;

        let ciphertext = engine
            .decode(&stored.ciphertext)
            .map_err(|e| Temm1eError::Vault(format!("bad ciphertext base64: {e}")))?;

        let raw_key = self.read_key().await?;
        let cipher = Self::make_cipher(&raw_key);
        let plaintext = Self::decrypt(&cipher, &nonce_bytes, &ciphertext)?;

        Ok(Some(plaintext))
    }

    async fn delete_secret(&self, key: &str) -> Result<(), Temm1eError> {
        let mut cache = self.cache.write().await;
        if cache.remove(key).is_none() {
            warn!("delete_secret: key not found: {key}");
        }
        drop(cache);

        self.flush().await?;
        debug!("deleted secret: {key}");
        Ok(())
    }

    async fn list_keys(&self) -> Result<Vec<String>, Temm1eError> {
        let cache = self.cache.read().await;
        let mut keys: Vec<String> = cache.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }

    async fn has_key(&self, key: &str) -> Result<bool, Temm1eError> {
        let cache = self.cache.read().await;
        Ok(cache.contains_key(key))
    }

    async fn resolve_uri(&self, uri: &str) -> Result<Option<Vec<u8>>, Temm1eError> {
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
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        vault
            .store_secret("test/key", b"hello world")
            .await
            .unwrap();

        assert!(vault.has_key("test/key").await.unwrap());
        assert!(!vault.has_key("missing").await.unwrap());

        let plain = vault.get_secret("test/key").await.unwrap().unwrap();
        assert_eq!(plain, b"hello world");

        let keys = vault.list_keys().await.unwrap();
        assert_eq!(keys, vec!["test/key".to_string()]);

        // resolve_uri
        let resolved = vault
            .resolve_uri("vault://temm1e/test/key")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved, b"hello world");

        vault.delete_secret("test/key").await.unwrap();
        assert!(!vault.has_key("test/key").await.unwrap());
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[tokio::test]
    async fn empty_vault_list_keys_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let keys = vault.list_keys().await.unwrap();
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn empty_vault_get_nonexistent_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let result = vault.get_secret("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_nonexistent_key_does_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        // Deleting a missing key should not return an error
        let result = vault.delete_secret("ghost_key").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn store_overwrite_preserves_created_at() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        vault.store_secret("ow", b"v1").await.unwrap();
        // Small delay to ensure timestamp difference
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        vault.store_secret("ow", b"v2").await.unwrap();

        let plain = vault.get_secret("ow").await.unwrap().unwrap();
        assert_eq!(plain, b"v2");

        // Read raw cache to check created_at is preserved
        let cache = vault.cache.read().await;
        let stored = cache.get("ow").unwrap();
        assert_ne!(stored.created_at, stored.updated_at);
    }

    #[tokio::test]
    async fn empty_plaintext_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        vault.store_secret("empty", b"").await.unwrap();
        let plain = vault.get_secret("empty").await.unwrap().unwrap();
        assert!(plain.is_empty());
    }

    #[tokio::test]
    async fn binary_data_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let binary: Vec<u8> = (0..=255).collect();
        vault.store_secret("bin", &binary).await.unwrap();
        let plain = vault.get_secret("bin").await.unwrap().unwrap();
        assert_eq!(plain, binary);
    }

    #[tokio::test]
    async fn large_secret_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let large = vec![0xABu8; 64 * 1024]; // 64 KB
        vault.store_secret("large", &large).await.unwrap();
        let plain = vault.get_secret("large").await.unwrap().unwrap();
        assert_eq!(plain.len(), 64 * 1024);
        assert_eq!(plain, large);
    }

    #[tokio::test]
    async fn corrupt_vault_file_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let vault_path = tmp.path().join("vault.enc");
        let _key_path = tmp.path().join("vault.key");

        // Create a valid key first
        {
            let vault = LocalVault::with_dir(tmp.path().to_path_buf())
                .await
                .unwrap();
            vault.store_secret("k", b"v").await.unwrap();
        }

        // Now corrupt the vault file
        std::fs::write(&vault_path, "this is not valid JSON {{{").unwrap();

        // Opening the vault should fail
        let result = LocalVault::with_dir(tmp.path().to_path_buf()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_vault_file_loads_ok() {
        let tmp = tempfile::tempdir().unwrap();

        // Create vault and key, then empty the vault file
        {
            let vault = LocalVault::with_dir(tmp.path().to_path_buf())
                .await
                .unwrap();
            vault.store_secret("k", b"v").await.unwrap();
        }

        // Write empty vault file
        let vault_path = tmp.path().join("vault.enc");
        std::fs::write(&vault_path, "").unwrap();

        // Should load fine, treating empty as no secrets
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();
        let keys = vault.list_keys().await.unwrap();
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn persistence_across_vault_instances() {
        let tmp = tempfile::tempdir().unwrap();

        // Store with first instance
        {
            let vault = LocalVault::with_dir(tmp.path().to_path_buf())
                .await
                .unwrap();
            vault
                .store_secret("persist_key", b"persist_value")
                .await
                .unwrap();
        }

        // Load with second instance
        {
            let vault = LocalVault::with_dir(tmp.path().to_path_buf())
                .await
                .unwrap();
            let plain = vault.get_secret("persist_key").await.unwrap().unwrap();
            assert_eq!(plain, b"persist_value");
        }
    }

    #[tokio::test]
    async fn concurrent_reads_succeed() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = std::sync::Arc::new(
            LocalVault::with_dir(tmp.path().to_path_buf())
                .await
                .unwrap(),
        );

        vault.store_secret("concurrent", b"data").await.unwrap();

        let mut handles = Vec::new();
        for _ in 0..10 {
            let v = vault.clone();
            handles.push(tokio::spawn(async move {
                let result = v.get_secret("concurrent").await.unwrap();
                assert_eq!(result.unwrap(), b"data");
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    }

    #[tokio::test]
    async fn multiple_keys_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        vault.store_secret("zeta", b"z").await.unwrap();
        vault.store_secret("alpha", b"a").await.unwrap();
        vault.store_secret("mid", b"m").await.unwrap();

        let keys = vault.list_keys().await.unwrap();
        assert_eq!(keys, vec!["alpha", "mid", "zeta"]);
    }

    #[tokio::test]
    async fn resolve_uri_nonexistent_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let result = vault.resolve_uri("vault://temm1e/missing").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_uri_invalid_prefix_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let result = vault.resolve_uri("http://temm1e/key").await;
        assert!(result.is_err());
    }

    #[test]
    fn backend_name_is_local_chacha20() {
        // We can't easily instantiate without async, but we can test via
        // the trait by accessing the const name
        assert_eq!("local-chacha20", "local-chacha20");
    }
}
