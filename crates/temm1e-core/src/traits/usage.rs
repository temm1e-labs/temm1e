//! Usage tracking trait — persistent storage for per-turn usage records.

use async_trait::async_trait;

use crate::types::error::Temm1eError;
use serde::{Deserialize, Serialize};

/// A single per-turn usage record for persistence and auditing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: String,
    pub chat_id: String,
    pub session_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub api_calls: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub tools_used: u32,
    pub total_cost_usd: f64,
    pub provider: String,
    pub model: String,
}

/// Aggregated usage summary across multiple turns.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageSummary {
    pub total_api_calls: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tools_used: u64,
    pub total_cost_usd: f64,
    pub turn_count: u64,
}

impl UsageSummary {
    pub fn combined_tokens(&self) -> u64 {
        self.total_input_tokens + self.total_output_tokens
    }
}

/// Trait for persisting and querying usage records.
#[async_trait]
pub trait UsageStore: Send + Sync {
    /// Record a single turn's usage.
    async fn record_usage(&self, record: UsageRecord) -> Result<(), Temm1eError>;

    /// Query recent usage records for a chat, ordered by timestamp descending.
    async fn query_usage(
        &self,
        chat_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<UsageRecord>, Temm1eError>;

    /// Get aggregated usage summary for a chat.
    async fn usage_summary(&self, chat_id: &str) -> Result<UsageSummary, Temm1eError>;

    /// Set whether usage display is enabled for a chat.
    async fn set_usage_display(&self, chat_id: &str, enabled: bool) -> Result<(), Temm1eError>;

    /// Check if usage display is enabled for a chat.
    async fn is_usage_display_enabled(&self, chat_id: &str) -> Result<bool, Temm1eError>;
}
