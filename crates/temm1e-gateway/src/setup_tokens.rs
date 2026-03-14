//! One-Time Key (OTK) setup token store for secure API key collection.
//!
//! Each token is a 256-bit random key tied to a specific `chat_id`,
//! with a 10-minute TTL and single-use consumption. The OTK is sent
//! to the user's browser via a URL fragment (never hits any server)
//! and used for client-side AES-256-GCM encryption of API keys.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use temm1e_core::SetupLinkGenerator;
use tokio::sync::RwLock;

/// A single OTK setup token.
pub struct SetupToken {
    /// 256-bit random key for AES-256-GCM encryption.
    pub otk: [u8; 32],
    /// When this token was created.
    pub created_at: Instant,
}

/// In-memory store for OTK setup tokens, keyed by chat_id.
#[derive(Clone)]
pub struct SetupTokenStore {
    tokens: Arc<RwLock<HashMap<String, SetupToken>>>,
    ttl: Duration,
}

impl SetupTokenStore {
    /// Create a new store with the default 10-minute TTL.
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            ttl: Duration::from_secs(600),
        }
    }

    /// Create a store with a custom TTL (useful for testing).
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            ttl,
        }
    }

    /// Generate a new OTK for a chat. Replaces any existing token for that chat.
    pub async fn generate(&self, chat_id: &str) -> [u8; 32] {
        use rand::RngCore;
        let mut otk = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut otk);
        let token = SetupToken {
            otk,
            created_at: Instant::now(),
        };
        self.tokens.write().await.insert(chat_id.to_string(), token);
        tracing::info!(chat_id = %chat_id, "OTK generated for secure key setup");
        otk
    }

    /// Consume the OTK for a chat. Returns the key bytes if valid and not expired.
    /// The token is removed regardless (one-time use).
    pub async fn consume(&self, chat_id: &str) -> Option<[u8; 32]> {
        let token = self.tokens.write().await.remove(chat_id)?;
        if token.created_at.elapsed() > self.ttl {
            tracing::warn!(chat_id = %chat_id, "OTK expired — setup link is no longer valid");
            None
        } else {
            tracing::info!(chat_id = %chat_id, "OTK consumed for decryption");
            Some(token.otk)
        }
    }

    /// Remove all expired tokens.
    pub async fn cleanup_expired(&self) {
        let ttl = self.ttl;
        let before = self.tokens.read().await.len();
        self.tokens
            .write()
            .await
            .retain(|_, t| t.created_at.elapsed() <= ttl);
        let after = self.tokens.read().await.len();
        if before > after {
            tracing::debug!(removed = before - after, "Cleaned up expired OTK tokens");
        }
    }
}

impl Default for SetupTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SetupLinkGenerator for SetupTokenStore {
    async fn generate_link(&self, chat_id: &str) -> String {
        let otk = self.generate(chat_id).await;
        let otk_hex = hex::encode(otk);
        format!("https://nagisanzenin.github.io/temm1e/setup#{}", otk_hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_and_consume() {
        let store = SetupTokenStore::new();
        let otk = store.generate("chat1").await;
        assert_eq!(otk.len(), 32);

        let consumed = store.consume("chat1").await;
        assert_eq!(consumed, Some(otk));

        // Second consume returns None (one-time use)
        assert!(store.consume("chat1").await.is_none());
    }

    #[tokio::test]
    async fn test_chat_id_isolation() {
        let store = SetupTokenStore::new();
        let otk1 = store.generate("chat1").await;
        let otk2 = store.generate("chat2").await;

        assert_ne!(otk1, otk2);

        assert_eq!(store.consume("chat1").await, Some(otk1));
        assert_eq!(store.consume("chat2").await, Some(otk2));
    }

    #[tokio::test]
    async fn test_expired_token() {
        let store = SetupTokenStore::with_ttl(Duration::from_millis(1));
        let _otk = store.generate("chat1").await;

        tokio::time::sleep(Duration::from_millis(10)).await;

        assert!(store.consume("chat1").await.is_none());
    }

    #[tokio::test]
    async fn test_replace_existing() {
        let store = SetupTokenStore::new();
        let otk1 = store.generate("chat1").await;
        let otk2 = store.generate("chat1").await;

        assert_ne!(otk1, otk2);
        assert_eq!(store.consume("chat1").await, Some(otk2));
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let store = SetupTokenStore::with_ttl(Duration::from_millis(1));
        store.generate("chat1").await;
        store.generate("chat2").await;

        tokio::time::sleep(Duration::from_millis(10)).await;
        store.cleanup_expired().await;

        assert!(store.consume("chat1").await.is_none());
        assert!(store.consume("chat2").await.is_none());
    }

    #[tokio::test]
    async fn test_consume_nonexistent() {
        let store = SetupTokenStore::new();
        assert!(store.consume("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_generate_returns_random_bytes() {
        let store = SetupTokenStore::new();
        let otk1 = store.generate("a").await;
        let otk2 = store.generate("b").await;
        // Extremely unlikely to be equal
        assert_ne!(otk1, otk2);
        // Should not be all zeros
        assert!(otk1.iter().any(|&b| b != 0));
    }
}
