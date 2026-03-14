use crate::types::error::Temm1eError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub session_id: Option<String>,
    pub entry_type: MemoryEntryType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemoryEntryType {
    Conversation,
    LongTerm,
    DailyLog,
    Skill,
    Knowledge,
    Blueprint,
}

/// Search options for memory queries
#[derive(Debug, Clone)]
pub struct SearchOpts {
    pub limit: usize,
    pub vector_weight: f32,
    pub keyword_weight: f32,
    pub session_filter: Option<String>,
    pub entry_type_filter: Option<MemoryEntryType>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            limit: 10,
            vector_weight: 0.7,
            keyword_weight: 0.3,
            session_filter: None,
            entry_type_filter: None,
        }
    }
}

/// Memory backend trait — persistence for conversations, long-term memory, and skills
#[async_trait]
pub trait Memory: Send + Sync {
    /// Store a memory entry
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError>;

    /// Hybrid search: vector similarity + keyword matching
    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError>;

    /// Get a specific memory entry by ID
    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError>;

    /// Delete a memory entry
    async fn delete(&self, id: &str) -> Result<(), Temm1eError>;

    /// List all sessions
    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError>;

    /// Get conversation history for a session
    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError>;

    /// Backend name (e.g., "sqlite", "postgres", "markdown")
    fn backend_name(&self) -> &str;
}
