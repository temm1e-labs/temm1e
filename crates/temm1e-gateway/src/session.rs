//! Session manager — thread-safe storage for active session contexts.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use temm1e_core::types::session::SessionContext;

/// Maximum number of concurrent sessions before LRU eviction.
const MAX_SESSIONS: usize = 1000;

/// Maximum number of messages in a single session's conversation history.
const MAX_HISTORY_PER_SESSION: usize = 200;

/// Thread-safe session manager backed by an in-memory HashMap.
#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionContext>>>,
    /// Tracks access order for LRU eviction (most recent at the back).
    access_order: Arc<RwLock<Vec<String>>>,
}

impl SessionManager {
    /// Create a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            access_order: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Build a deterministic session key from channel + chat_id + user_id.
    fn session_key(channel: &str, chat_id: &str, user_id: &str) -> String {
        format!("{}:{}:{}", channel, chat_id, user_id)
    }

    /// Get an existing session or create a new one for the given channel/chat/user.
    ///
    /// Enforces `MAX_SESSIONS` by evicting the least-recently-used session
    /// when the limit is reached, and `MAX_HISTORY_PER_SESSION` by truncating
    /// old messages from the conversation history.
    pub async fn get_or_create_session(
        &self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
    ) -> SessionContext {
        let key = Self::session_key(channel, chat_id, user_id);

        // Fast path: read lock
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(&key).cloned() {
                drop(sessions);
                self.touch_access_order(&key).await;
                return session;
            }
        }

        // Slow path: write lock, create new session
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(&key) {
            let session = session.clone();
            drop(sessions);
            self.touch_access_order(&key).await;
            return session;
        }

        // Evict oldest sessions if at capacity
        if sessions.len() >= MAX_SESSIONS {
            let mut order = self.access_order.write().await;
            while sessions.len() >= MAX_SESSIONS && !order.is_empty() {
                let evict_key = order.remove(0);
                sessions.remove(&evict_key);
                tracing::debug!(session = %evict_key, "Evicted LRU session (limit: {})", MAX_SESSIONS);
            }
        }

        let session = self.make_session(channel, chat_id, user_id, &key);

        sessions.insert(key.clone(), session.clone());
        drop(sessions);

        let mut order = self.access_order.write().await;
        order.push(key);

        session
    }

    /// Create a new SessionContext.
    fn make_session(
        &self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        key: &str,
    ) -> SessionContext {
        SessionContext {
            session_id: key.to_string(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
            history: Vec::new(),
            workspace_path: std::env::current_dir().unwrap_or_else(|_| "/tmp".into()),
        }
    }

    /// Update access order for LRU tracking.
    async fn touch_access_order(&self, key: &str) {
        let mut order = self.access_order.write().await;
        order.retain(|k| k != key);
        order.push(key.to_string());
    }

    /// Update a session in the store (e.g., after history changes).
    ///
    /// Truncates conversation history to `MAX_HISTORY_PER_SESSION` messages,
    /// keeping the most recent ones.
    pub async fn update_session(&self, mut session: SessionContext) {
        let key = Self::session_key(&session.channel, &session.chat_id, &session.user_id);

        // Truncate history if it exceeds the limit
        if session.history.len() > MAX_HISTORY_PER_SESSION {
            let drain_count = session.history.len() - MAX_HISTORY_PER_SESSION;
            session.history.drain(..drain_count);
        }

        let mut sessions = self.sessions.write().await;
        sessions.insert(key.clone(), session);
        drop(sessions);

        self.touch_access_order(&key).await;
    }

    /// Remove a session from the store.
    pub async fn remove_session(&self, channel: &str, chat_id: &str, user_id: &str) {
        let key = Self::session_key(channel, chat_id, user_id);
        let mut sessions = self.sessions.write().await;
        sessions.remove(&key);
        drop(sessions);

        let mut order = self.access_order.write().await;
        order.retain(|k| k != &key);
    }

    /// Get the number of active sessions.
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        sessions.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_session_returns_new_session() {
        let mgr = SessionManager::new();
        let session = mgr
            .get_or_create_session("telegram", "chat1", "user1")
            .await;
        assert_eq!(session.channel, "telegram");
        assert_eq!(session.chat_id, "chat1");
        assert_eq!(session.user_id, "user1");
        assert!(session.history.is_empty());
    }

    #[tokio::test]
    async fn get_existing_session_returns_same() {
        let mgr = SessionManager::new();
        let s1 = mgr.get_or_create_session("cli", "c1", "u1").await;
        let s2 = mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(s1.session_id, s2.session_id);
    }

    #[tokio::test]
    async fn different_users_get_different_sessions() {
        let mgr = SessionManager::new();
        let s1 = mgr.get_or_create_session("cli", "c1", "user_a").await;
        let s2 = mgr.get_or_create_session("cli", "c1", "user_b").await;
        assert_ne!(s1.session_id, s2.session_id);
    }

    #[tokio::test]
    async fn session_count_tracks_active() {
        let mgr = SessionManager::new();
        assert_eq!(mgr.session_count().await, 0);

        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 1);

        mgr.get_or_create_session("tg", "c2", "u2").await;
        assert_eq!(mgr.session_count().await, 2);

        // Same session, no increase
        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 2);
    }

    #[tokio::test]
    async fn remove_session_decreases_count() {
        let mgr = SessionManager::new();
        mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 1);

        mgr.remove_session("cli", "c1", "u1").await;
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn update_session_preserves_changes() {
        let mgr = SessionManager::new();
        let mut session = mgr.get_or_create_session("cli", "c1", "u1").await;

        // Simulate adding history
        session
            .history
            .push(temm1e_core::types::message::ChatMessage {
                role: temm1e_core::types::message::Role::User,
                content: temm1e_core::types::message::MessageContent::Text("hello".to_string()),
            });
        mgr.update_session(session).await;

        let restored = mgr.get_or_create_session("cli", "c1", "u1").await;
        assert_eq!(restored.history.len(), 1);
    }

    #[test]
    fn session_key_is_deterministic() {
        let k1 = SessionManager::session_key("telegram", "123", "456");
        let k2 = SessionManager::session_key("telegram", "123", "456");
        assert_eq!(k1, k2);
        assert_eq!(k1, "telegram:123:456");
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[tokio::test]
    async fn concurrent_session_creation() {
        let mgr = std::sync::Arc::new(SessionManager::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let m = mgr.clone();
            handles.push(tokio::spawn(async move {
                m.get_or_create_session("cli", &format!("chat{i}"), "user")
                    .await
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(mgr.session_count().await, 10);
    }

    #[tokio::test]
    async fn remove_nonexistent_session_is_noop() {
        let mgr = SessionManager::new();
        mgr.remove_session("missing", "c", "u").await;
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn default_session_manager_empty() {
        let mgr = SessionManager::default();
        assert_eq!(mgr.session_count().await, 0);
    }

    #[tokio::test]
    async fn session_key_with_special_characters() {
        let mgr = SessionManager::new();
        let session = mgr
            .get_or_create_session("tg", "chat:with:colons", "user@domain")
            .await;
        assert_eq!(session.session_id, "tg:chat:with:colons:user@domain");
    }

    #[tokio::test]
    async fn update_nonexistent_session_creates_it() {
        let mgr = SessionManager::new();

        let session = SessionContext {
            session_id: "new".to_string(),
            channel: "cli".to_string(),
            chat_id: "c1".to_string(),
            user_id: "u1".to_string(),
            history: Vec::new(),
            workspace_path: std::path::PathBuf::from("/tmp"),
        };

        mgr.update_session(session).await;
        // The session_key function builds key from channel+chat_id+user_id
        let count = mgr.session_count().await;
        assert_eq!(count, 1);
    }
}
