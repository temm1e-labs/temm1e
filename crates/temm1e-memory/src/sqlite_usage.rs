//! SQLite-backed usage store implementation.

use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use temm1e_core::error::Temm1eError;
use temm1e_core::{UsageRecord, UsageStore, UsageSummary};
use tracing::info;

/// SQLite-backed persistence for per-turn usage records.
pub struct SqliteUsageStore {
    pool: SqlitePool,
}

impl SqliteUsageStore {
    /// Create a new SqliteUsageStore, reusing an existing pool.
    pub async fn from_pool(pool: SqlitePool) -> Result<Self, Temm1eError> {
        let store = Self { pool };
        store.init_tables().await?;
        info!("SQLite usage store initialised");
        Ok(store)
    }

    /// Create a new SqliteUsageStore from a connection URL.
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to connect to SQLite: {e}")))?;

        Self::from_pool(pool).await
    }

    async fn init_tables(&self) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS usage_log (
                id             TEXT PRIMARY KEY,
                chat_id        TEXT NOT NULL,
                session_id     TEXT NOT NULL,
                timestamp      TEXT NOT NULL,
                api_calls      INTEGER NOT NULL,
                input_tokens   INTEGER NOT NULL,
                output_tokens  INTEGER NOT NULL,
                tools_used     INTEGER NOT NULL,
                total_cost_usd REAL NOT NULL,
                provider       TEXT NOT NULL,
                model          TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create usage_log table: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_usage_chat ON usage_log(chat_id, timestamp DESC)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create usage index: {e}")))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS usage_display (
                chat_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 1
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create usage_display table: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl UsageStore for SqliteUsageStore {
    async fn record_usage(&self, record: UsageRecord) -> Result<(), Temm1eError> {
        let timestamp_str = record.timestamp.to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO usage_log (id, chat_id, session_id, timestamp, api_calls,
                                   input_tokens, output_tokens, tools_used,
                                   total_cost_usd, provider, model)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&record.id)
        .bind(&record.chat_id)
        .bind(&record.session_id)
        .bind(&timestamp_str)
        .bind(record.api_calls as i64)
        .bind(record.input_tokens as i64)
        .bind(record.output_tokens as i64)
        .bind(record.tools_used as i64)
        .bind(record.total_cost_usd)
        .bind(&record.provider)
        .bind(&record.model)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to insert usage record: {e}")))?;
        Ok(())
    }

    async fn query_usage(
        &self,
        chat_id: &str,
        limit: Option<u32>,
    ) -> Result<Vec<UsageRecord>, Temm1eError> {
        let limit = limit.unwrap_or(10) as i64;
        let rows = sqlx::query_as::<_, UsageRow>(
            r#"
            SELECT id, chat_id, session_id, timestamp, api_calls,
                   input_tokens, output_tokens, tools_used,
                   total_cost_usd, provider, model
            FROM usage_log
            WHERE chat_id = ?
            ORDER BY timestamp DESC
            LIMIT ?
            "#,
        )
        .bind(chat_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to query usage: {e}")))?;

        rows.into_iter().map(|r| r.into_record()).collect()
    }

    async fn usage_summary(&self, chat_id: &str) -> Result<UsageSummary, Temm1eError> {
        let row = sqlx::query_as::<_, SummaryRow>(
            r#"
            SELECT
                COALESCE(SUM(api_calls), 0)      AS total_api_calls,
                COALESCE(SUM(input_tokens), 0)   AS total_input_tokens,
                COALESCE(SUM(output_tokens), 0)  AS total_output_tokens,
                COALESCE(SUM(tools_used), 0)     AS total_tools_used,
                COALESCE(SUM(total_cost_usd), 0.0) AS total_cost_usd,
                COUNT(*)                          AS turn_count
            FROM usage_log
            WHERE chat_id = ?
            "#,
        )
        .bind(chat_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to get usage summary: {e}")))?;

        Ok(UsageSummary {
            total_api_calls: row.total_api_calls as u64,
            total_input_tokens: row.total_input_tokens as u64,
            total_output_tokens: row.total_output_tokens as u64,
            total_tools_used: row.total_tools_used as u64,
            total_cost_usd: row.total_cost_usd,
            turn_count: row.turn_count as u64,
        })
    }

    async fn set_usage_display(&self, chat_id: &str, enabled: bool) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            INSERT INTO usage_display (chat_id, enabled)
            VALUES (?, ?)
            ON CONFLICT(chat_id) DO UPDATE SET enabled = excluded.enabled
            "#,
        )
        .bind(chat_id)
        .bind(enabled as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to set usage display preference: {e}")))?;
        Ok(())
    }

    async fn is_usage_display_enabled(&self, chat_id: &str) -> Result<bool, Temm1eError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT enabled FROM usage_display WHERE chat_id = ?")
                .bind(chat_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    Temm1eError::Memory(format!("Failed to query usage display preference: {e}"))
                })?;

        Ok(row.is_none_or(|(v,)| v != 0))
    }
}

#[derive(sqlx::FromRow)]
struct UsageRow {
    id: String,
    chat_id: String,
    session_id: String,
    timestamp: String,
    api_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    tools_used: i64,
    total_cost_usd: f64,
    provider: String,
    model: String,
}

impl UsageRow {
    fn into_record(self) -> Result<UsageRecord, Temm1eError> {
        let ts = chrono::DateTime::parse_from_rfc3339(&self.timestamp)
            .map_err(|e| Temm1eError::Memory(format!("Invalid timestamp: {e}")))?
            .with_timezone(&chrono::Utc);
        Ok(UsageRecord {
            id: self.id,
            chat_id: self.chat_id,
            session_id: self.session_id,
            timestamp: ts,
            api_calls: self.api_calls as u32,
            input_tokens: self.input_tokens as u32,
            output_tokens: self.output_tokens as u32,
            tools_used: self.tools_used as u32,
            total_cost_usd: self.total_cost_usd,
            provider: self.provider,
            model: self.model,
        })
    }
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    total_api_calls: i64,
    total_input_tokens: i64,
    total_output_tokens: i64,
    total_tools_used: i64,
    total_cost_usd: f64,
    turn_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> SqliteUsageStore {
        SqliteUsageStore::new("sqlite::memory:").await.unwrap()
    }

    fn sample_record(chat_id: &str, id: &str) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            chat_id: chat_id.to_string(),
            session_id: "s1".to_string(),
            timestamp: chrono::Utc::now(),
            api_calls: 2,
            input_tokens: 5000,
            output_tokens: 1000,
            tools_used: 1,
            total_cost_usd: 0.03,
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-6".to_string(),
        }
    }

    #[tokio::test]
    async fn record_and_query() {
        let store = test_store().await;
        store
            .record_usage(sample_record("chat-1", "r1"))
            .await
            .unwrap();
        store
            .record_usage(sample_record("chat-1", "r2"))
            .await
            .unwrap();
        store
            .record_usage(sample_record("chat-2", "r3"))
            .await
            .unwrap();

        let records = store.query_usage("chat-1", None).await.unwrap();
        assert_eq!(records.len(), 2);

        let records = store.query_usage("chat-2", None).await.unwrap();
        assert_eq!(records.len(), 1);
    }

    #[tokio::test]
    async fn query_with_limit() {
        let store = test_store().await;
        for i in 0..5 {
            store
                .record_usage(sample_record("chat-1", &format!("r{i}")))
                .await
                .unwrap();
        }
        let records = store.query_usage("chat-1", Some(3)).await.unwrap();
        assert_eq!(records.len(), 3);
    }

    #[tokio::test]
    async fn summary_empty() {
        let store = test_store().await;
        let summary = store.usage_summary("no-such-chat").await.unwrap();
        assert_eq!(summary.turn_count, 0);
        assert_eq!(summary.total_input_tokens, 0);
    }

    #[tokio::test]
    async fn summary_aggregation() {
        let store = test_store().await;
        store
            .record_usage(sample_record("chat-1", "r1"))
            .await
            .unwrap();
        store
            .record_usage(sample_record("chat-1", "r2"))
            .await
            .unwrap();

        let summary = store.usage_summary("chat-1").await.unwrap();
        assert_eq!(summary.turn_count, 2);
        assert_eq!(summary.total_api_calls, 4); // 2 per record
        assert_eq!(summary.total_input_tokens, 10_000);
        assert_eq!(summary.total_output_tokens, 2_000);
        assert_eq!(summary.combined_tokens(), 12_000);
    }

    #[tokio::test]
    async fn display_default_enabled() {
        let store = test_store().await;
        assert!(store.is_usage_display_enabled("chat-1").await.unwrap());
    }

    #[tokio::test]
    async fn display_toggle() {
        let store = test_store().await;
        store.set_usage_display("chat-1", false).await.unwrap();
        assert!(!store.is_usage_display_enabled("chat-1").await.unwrap());

        store.set_usage_display("chat-1", true).await.unwrap();
        assert!(store.is_usage_display_enabled("chat-1").await.unwrap());
    }

    #[tokio::test]
    async fn chat_isolation() {
        let store = test_store().await;
        store.set_usage_display("chat-1", false).await.unwrap();
        // chat-2 should still be default (enabled)
        assert!(store.is_usage_display_enabled("chat-2").await.unwrap());
    }
}
