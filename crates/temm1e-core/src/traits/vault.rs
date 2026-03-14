use crate::types::error::Temm1eError;
use async_trait::async_trait;

/// A secret entry in the vault
#[derive(Debug, Clone)]
pub struct SecretEntry {
    pub key: String,
    pub encrypted_value: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Vault trait — encrypted secrets management
#[async_trait]
pub trait Vault: Send + Sync {
    /// Store a secret (encrypts before storage)
    async fn store_secret(&self, key: &str, plaintext: &[u8]) -> Result<(), Temm1eError>;

    /// Retrieve a secret (decrypts on read)
    async fn get_secret(&self, key: &str) -> Result<Option<Vec<u8>>, Temm1eError>;

    /// Delete a secret
    async fn delete_secret(&self, key: &str) -> Result<(), Temm1eError>;

    /// List secret keys (names only, not values)
    async fn list_keys(&self) -> Result<Vec<String>, Temm1eError>;

    /// Check if a key exists
    async fn has_key(&self, key: &str) -> Result<bool, Temm1eError>;

    /// Resolve a vault:// URI to its plaintext value
    async fn resolve_uri(&self, uri: &str) -> Result<Option<Vec<u8>>, Temm1eError>;

    /// Vault backend name (e.g., "local-chacha20", "aws-kms")
    fn backend_name(&self) -> &str;
}
