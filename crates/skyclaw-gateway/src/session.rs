//! Session manager — thread-safe storage for active session contexts.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use skyclaw_core::types::session::SessionContext;

/// Thread-safe session manager backed by an in-memory HashMap.
#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, SessionContext>>>,
}

impl SessionManager {
    /// Create a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Build a deterministic session key from channel + chat_id + user_id.
    fn session_key(channel: &str, chat_id: &str, user_id: &str) -> String {
        format!("{}:{}:{}", channel, chat_id, user_id)
    }

    /// Get an existing session or create a new one for the given channel/chat/user.
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
            if let Some(session) = sessions.get(&key) {
                return session.clone();
            }
        }

        // Slow path: write lock, create new session
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(&key) {
            return session.clone();
        }

        let session = SessionContext {
            session_id: key.clone(),
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
            history: Vec::new(),
            workspace_path: std::env::current_dir().unwrap_or_else(|_| "/tmp".into()),
        };

        sessions.insert(key, session.clone());
        session
    }

    /// Update a session in the store (e.g., after history changes).
    pub async fn update_session(&self, session: SessionContext) {
        let key = Self::session_key(&session.channel, &session.chat_id, &session.user_id);
        let mut sessions = self.sessions.write().await;
        sessions.insert(key, session);
    }

    /// Remove a session from the store.
    pub async fn remove_session(&self, channel: &str, chat_id: &str, user_id: &str) {
        let key = Self::session_key(channel, chat_id, user_id);
        let mut sessions = self.sessions.write().await;
        sessions.remove(&key);
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
