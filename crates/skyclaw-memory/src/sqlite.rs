//! SQLite-backed memory implementation.

use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use skyclaw_core::{Memory, MemoryEntry, MemoryEntryType, SearchOpts};
use skyclaw_core::error::SkyclawError;
use tracing::{debug, info};

/// A memory backend backed by SQLite via sqlx.
pub struct SqliteMemory {
    pool: SqlitePool,
}

impl SqliteMemory {
    /// Create a new SqliteMemory and initialise the schema.
    ///
    /// `database_url` is a SQLite connection string, e.g. `"sqlite:memory.db"` or
    /// `"sqlite::memory:"` for an in-memory database.
    pub async fn new(database_url: &str) -> Result<Self, SkyclawError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| SkyclawError::Memory(format!("Failed to connect to SQLite: {e}")))?;

        let mem = Self { pool };
        mem.init_tables().await?;
        info!("SQLite memory backend initialised");
        Ok(mem)
    }

    /// Create tables if they don't already exist.
    async fn init_tables(&self) -> Result<(), SkyclawError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id         TEXT PRIMARY KEY,
                content    TEXT NOT NULL,
                metadata   TEXT NOT NULL DEFAULT '{}',
                timestamp  TEXT NOT NULL,
                session_id TEXT,
                entry_type TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to create tables: {e}")))?;

        // Index for session lookups.
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_session ON memory_entries(session_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to create index: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), SkyclawError> {
        let metadata_str =
            serde_json::to_string(&entry.metadata).map_err(SkyclawError::Serialization)?;
        let timestamp_str = entry.timestamp.to_rfc3339();
        let entry_type_str = entry_type_to_str(&entry.entry_type);

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO memory_entries (id, content, metadata, timestamp, session_id, entry_type)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&entry.id)
        .bind(&entry.content)
        .bind(&metadata_str)
        .bind(&timestamp_str)
        .bind(&entry.session_id)
        .bind(entry_type_str)
        .execute(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to store entry: {e}")))?;

        debug!(id = %entry.id, "Stored memory entry");
        Ok(())
    }

    async fn search(
        &self,
        query: &str,
        opts: SearchOpts,
    ) -> Result<Vec<MemoryEntry>, SkyclawError> {
        // Build a LIKE-based keyword search for v0.1.
        let like_pattern = format!("%{query}%");

        let mut sql = String::from(
            "SELECT id, content, metadata, timestamp, session_id, entry_type \
             FROM memory_entries WHERE content LIKE ?",
        );
        let mut bind_values: Vec<String> = vec![like_pattern];

        if let Some(ref session) = opts.session_filter {
            sql.push_str(" AND session_id = ?");
            bind_values.push(session.clone());
        }
        if let Some(ref et) = opts.entry_type_filter {
            sql.push_str(" AND entry_type = ?");
            bind_values.push(entry_type_to_str(et).to_string());
        }

        sql.push_str(" ORDER BY timestamp DESC LIMIT ?");
        bind_values.push(opts.limit.to_string());

        // We have to build the query dynamically because the number of binds
        // varies. sqlx's `query_as` doesn't support that ergonomically for raw
        // SQL, so we use `sqlx::query` and bind manually.
        let mut q = sqlx::query_as::<_, MemoryRow>(&sql);
        for v in &bind_values {
            q = q.bind(v);
        }

        let rows: Vec<MemoryRow> = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SkyclawError::Memory(format!("Search failed: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, SkyclawError> {
        let row = sqlx::query_as::<_, MemoryRow>(
            "SELECT id, content, metadata, timestamp, session_id, entry_type \
             FROM memory_entries WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to get entry: {e}")))?;

        match row {
            Some(r) => Ok(Some(row_to_entry(r)?)),
            None => Ok(None),
        }
    }

    async fn delete(&self, id: &str) -> Result<(), SkyclawError> {
        sqlx::query("DELETE FROM memory_entries WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| SkyclawError::Memory(format!("Failed to delete entry: {e}")))?;

        debug!(id = %id, "Deleted memory entry");
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<String>, SkyclawError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT session_id FROM memory_entries \
             WHERE session_id IS NOT NULL ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to list sessions: {e}")))?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, SkyclawError> {
        let rows: Vec<MemoryRow> = sqlx::query_as::<_, MemoryRow>(
            "SELECT id, content, metadata, timestamp, session_id, entry_type \
             FROM memory_entries WHERE session_id = ? \
             ORDER BY timestamp ASC LIMIT ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SkyclawError::Memory(format!("Failed to get session history: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }

    fn backend_name(&self) -> &str {
        "sqlite"
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Intermediate row type for sqlx deserialization.
#[derive(sqlx::FromRow)]
struct MemoryRow {
    id: String,
    content: String,
    metadata: String,
    timestamp: String,
    session_id: Option<String>,
    entry_type: String,
}

fn row_to_entry(row: MemoryRow) -> Result<MemoryEntry, SkyclawError> {
    let metadata: serde_json::Value =
        serde_json::from_str(&row.metadata).map_err(SkyclawError::Serialization)?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&row.timestamp)
        .map_err(|e| SkyclawError::Memory(format!("Invalid timestamp: {e}")))?
        .with_timezone(&chrono::Utc);
    let entry_type = str_to_entry_type(&row.entry_type)?;

    Ok(MemoryEntry {
        id: row.id,
        content: row.content,
        metadata,
        timestamp,
        session_id: row.session_id,
        entry_type,
    })
}

fn entry_type_to_str(et: &MemoryEntryType) -> &'static str {
    match et {
        MemoryEntryType::Conversation => "conversation",
        MemoryEntryType::LongTerm => "long_term",
        MemoryEntryType::DailyLog => "daily_log",
        MemoryEntryType::Skill => "skill",
    }
}

fn str_to_entry_type(s: &str) -> Result<MemoryEntryType, SkyclawError> {
    match s {
        "conversation" => Ok(MemoryEntryType::Conversation),
        "long_term" => Ok(MemoryEntryType::LongTerm),
        "daily_log" => Ok(MemoryEntryType::DailyLog),
        "skill" => Ok(MemoryEntryType::Skill),
        other => Err(SkyclawError::Memory(format!(
            "Unknown entry type: {other}"
        ))),
    }
}
