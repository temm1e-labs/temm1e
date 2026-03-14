use crate::types::message::ChatMessage;
use serde::{Deserialize, Serialize};

/// An active session context
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub channel: String,
    pub chat_id: String,
    pub user_id: String,
    pub history: Vec<ChatMessage>,
    pub workspace_path: std::path::PathBuf,
}

/// Session info for listing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub channel: String,
    pub user_id: String,
    pub last_active: chrono::DateTime<chrono::Utc>,
    pub message_count: u64,
}
