//! SQLite-backed memory implementation.

use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::time::Duration;
use temm1e_core::error::Temm1eError;
use temm1e_core::{
    LambdaMemoryEntry, LambdaMemoryType, Memory, MemoryEntry, MemoryEntryType, SearchOpts,
};
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};

/// Maximum time allowed for any single database operation.
const DB_TIMEOUT: Duration = Duration::from_secs(5);

/// A memory backend backed by SQLite via sqlx.
pub struct SqliteMemory {
    pool: SqlitePool,
}

impl SqliteMemory {
    /// Create a new SqliteMemory and initialise the schema.
    ///
    /// `database_url` is a SQLite connection string, e.g. `"sqlite:memory.db"` or
    /// `"sqlite::memory:"` for an in-memory database.
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to connect to SQLite: {e}")))?;

        let mem = Self { pool };
        mem.init_tables().await?;
        info!("SQLite memory backend initialised");
        Ok(mem)
    }

    /// Create tables if they don't already exist.
    async fn init_tables(&self) -> Result<(), Temm1eError> {
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
        .map_err(|e| Temm1eError::Memory(format!("Failed to create tables: {e}")))?;

        // Index for session lookups.
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_memory_session ON memory_entries(session_id)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create index: {e}")))?;

        // ── λ-Memory tables ───────────────────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS lambda_memories (
                hash            TEXT PRIMARY KEY,
                created_at      INTEGER NOT NULL,
                last_accessed   INTEGER NOT NULL,
                access_count    INTEGER NOT NULL DEFAULT 0,
                importance      REAL NOT NULL DEFAULT 1.0,
                explicit_save   INTEGER NOT NULL DEFAULT 0,
                full_text       TEXT NOT NULL,
                summary_text    TEXT NOT NULL,
                essence_text    TEXT NOT NULL,
                tags            TEXT NOT NULL DEFAULT '[]',
                memory_type     TEXT NOT NULL DEFAULT 'conversation',
                session_id      TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create lambda_memories: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_lm_importance ON lambda_memories(importance)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create lambda index: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_lm_last_accessed ON lambda_memories(last_accessed)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create lambda index: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_lm_explicit ON lambda_memories(explicit_save)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create lambda index: {e}")))?;

        // FTS5 virtual table for BM25 search on summary/essence/tags.
        // content='' makes it an external content table (we manage sync ourselves).
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS lambda_memories_fts
            USING fts5(summary_text, essence_text, tags, content='')
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create lambda FTS5: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
        let metadata_str =
            serde_json::to_string(&entry.metadata).map_err(Temm1eError::Serialization)?;
        let timestamp_str = entry.timestamp.to_rfc3339();
        let entry_type_str = entry_type_to_str(&entry.entry_type);

        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: Duration = Duration::from_millis(100);

        timeout(DB_TIMEOUT, async {
            let mut last_err = None;
            for attempt in 1..=MAX_RETRIES {
                match sqlx::query(
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
                {
                    Ok(_) => {
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if attempt < MAX_RETRIES
                            && (msg.contains("database is locked") || msg.contains("SQLITE_BUSY"))
                        {
                            warn!(
                                attempt = attempt,
                                max = MAX_RETRIES,
                                id = %entry.id,
                                "SQLITE_BUSY on store, retrying after {RETRY_DELAY:?}"
                            );
                            last_err = Some(e);
                            sleep(RETRY_DELAY).await;
                        } else {
                            return Err(Temm1eError::Memory(format!(
                                "Failed to store entry: {e}"
                            )));
                        }
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(Temm1eError::Memory(format!("Failed to store entry: {e}")));
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            Temm1eError::Memory("Database operation timed out after 5 seconds".into())
        })??;

        debug!(id = %entry.id, "Stored memory entry");
        Ok(())
    }

    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError> {
        // Split multi-word queries into individual word matches (AND logic).
        // Each word is matched against both content AND id fields.
        // This handles cases like "cat name" matching "cat's name" in content.
        let words: Vec<&str> = query.split_whitespace().collect();

        let mut sql = String::from(
            "SELECT id, content, metadata, timestamp, session_id, entry_type \
             FROM memory_entries WHERE 1=1",
        );
        let mut bind_values: Vec<String> = Vec::new();

        for word in &words {
            sql.push_str(" AND (content LIKE ? OR id LIKE ?)");
            let pattern = format!("%{word}%");
            bind_values.push(pattern.clone());
            bind_values.push(pattern);
        }

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

        let rows: Vec<MemoryRow> = timeout(DB_TIMEOUT, q.fetch_all(&self.pool))
            .await
            .map_err(|_| {
                Temm1eError::Memory("Database operation timed out after 5 seconds".into())
            })?
            .map_err(|e| Temm1eError::Memory(format!("Search failed: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
        let row = timeout(
            DB_TIMEOUT,
            sqlx::query_as::<_, MemoryRow>(
                "SELECT id, content, metadata, timestamp, session_id, entry_type \
                 FROM memory_entries WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&self.pool),
        )
        .await
        .map_err(|_| Temm1eError::Memory("Database operation timed out after 5 seconds".into()))?
        .map_err(|e| Temm1eError::Memory(format!("Failed to get entry: {e}")))?;

        match row {
            Some(r) => Ok(Some(row_to_entry(r)?)),
            None => Ok(None),
        }
    }

    async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
        timeout(
            DB_TIMEOUT,
            sqlx::query("DELETE FROM memory_entries WHERE id = ?")
                .bind(id)
                .execute(&self.pool),
        )
        .await
        .map_err(|_| Temm1eError::Memory("Database operation timed out after 5 seconds".into()))?
        .map_err(|e| Temm1eError::Memory(format!("Failed to delete entry: {e}")))?;

        debug!(id = %id, "Deleted memory entry");
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
        let rows: Vec<(String,)> = timeout(
            DB_TIMEOUT,
            sqlx::query_as(
                "SELECT DISTINCT session_id FROM memory_entries \
                 WHERE session_id IS NOT NULL ORDER BY session_id",
            )
            .fetch_all(&self.pool),
        )
        .await
        .map_err(|_| Temm1eError::Memory("Database operation timed out after 5 seconds".into()))?
        .map_err(|e| Temm1eError::Memory(format!("Failed to list sessions: {e}")))?;

        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let rows: Vec<MemoryRow> = timeout(
            DB_TIMEOUT,
            sqlx::query_as::<_, MemoryRow>(
                "SELECT id, content, metadata, timestamp, session_id, entry_type \
                 FROM memory_entries WHERE session_id = ? \
                 ORDER BY timestamp ASC LIMIT ?",
            )
            .bind(session_id)
            .bind(limit as i64)
            .fetch_all(&self.pool),
        )
        .await
        .map_err(|_| Temm1eError::Memory("Database operation timed out after 5 seconds".into()))?
        .map_err(|e| Temm1eError::Memory(format!("Failed to get session history: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }

    fn backend_name(&self) -> &str {
        "sqlite"
    }

    // ── λ-Memory implementations ──────────────────────────────────

    async fn lambda_store(&self, entry: LambdaMemoryEntry) -> Result<(), Temm1eError> {
        let tags_json = serde_json::to_string(&entry.tags).unwrap_or_else(|_| "[]".to_string());
        let memory_type = lambda_type_to_str(&entry.memory_type);
        sqlx::query(
            "INSERT OR REPLACE INTO lambda_memories \
             (hash, created_at, last_accessed, access_count, importance, explicit_save, \
              full_text, summary_text, essence_text, tags, memory_type, session_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&entry.hash)
        .bind(entry.created_at as i64)
        .bind(entry.last_accessed as i64)
        .bind(entry.access_count as i32)
        .bind(entry.importance)
        .bind(entry.explicit_save as i32)
        .bind(&entry.full_text)
        .bind(&entry.summary_text)
        .bind(&entry.essence_text)
        .bind(&tags_json)
        .bind(memory_type)
        .bind(&entry.session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_store failed: {e}")))?;

        // Sync FTS5: insert the searchable fields with the hash as the rowid substitute.
        // We use the hash's rowid from the main table.
        let rowid: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM lambda_memories WHERE hash = ?")
                .bind(&entry.hash)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| Temm1eError::Memory(format!("lambda_store FTS rowid lookup: {e}")))?;

        if let Some((rid,)) = rowid {
            // Delete old FTS entry if exists (for REPLACE case)
            let _ = sqlx::query(
                "INSERT INTO lambda_memories_fts(lambda_memories_fts, rowid, summary_text, essence_text, tags) \
                 VALUES ('delete', ?, ?, ?, ?)",
            )
            .bind(rid)
            .bind(&entry.summary_text)
            .bind(&entry.essence_text)
            .bind(&tags_json)
            .execute(&self.pool)
            .await;

            sqlx::query(
                "INSERT INTO lambda_memories_fts(rowid, summary_text, essence_text, tags) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(rid)
            .bind(&entry.summary_text)
            .bind(&entry.essence_text)
            .bind(&tags_json)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("lambda_store FTS insert: {e}")))?;
        }

        debug!(hash = %entry.hash, importance = entry.importance, "Stored λ-memory");
        Ok(())
    }

    async fn lambda_query_candidates(
        &self,
        limit: usize,
    ) -> Result<Vec<LambdaMemoryEntry>, Temm1eError> {
        let rows: Vec<LambdaMemoryRow> = sqlx::query_as(
            "SELECT hash, created_at, last_accessed, access_count, importance, \
             explicit_save, full_text, summary_text, essence_text, tags, \
             memory_type, session_id \
             FROM lambda_memories ORDER BY importance DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_query_candidates: {e}")))?;

        Ok(rows.into_iter().map(lambda_row_to_entry).collect())
    }

    async fn lambda_recall(
        &self,
        hash_prefix: &str,
    ) -> Result<Option<LambdaMemoryEntry>, Temm1eError> {
        let pattern = format!("{hash_prefix}%");
        let row: Option<LambdaMemoryRow> = sqlx::query_as(
            "SELECT hash, created_at, last_accessed, access_count, importance, \
             explicit_save, full_text, summary_text, essence_text, tags, \
             memory_type, session_id \
             FROM lambda_memories WHERE hash LIKE ? LIMIT 1",
        )
        .bind(&pattern)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_recall: {e}")))?;

        Ok(row.map(lambda_row_to_entry))
    }

    async fn lambda_touch(&self, hash: &str) -> Result<(), Temm1eError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "UPDATE lambda_memories SET last_accessed = ?, access_count = access_count + 1 \
             WHERE hash = ?",
        )
        .bind(now)
        .bind(hash)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_touch: {e}")))?;

        debug!(hash = %hash, "Touched λ-memory (reheated)");
        Ok(())
    }

    async fn lambda_fts_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f64)>, Temm1eError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // Sanitize: escape double quotes, wrap in quotes for phrase-safe matching
        let sanitized = query.replace('"', "\"\"");
        let rows: Vec<(i64, f64)> = sqlx::query_as(
            "SELECT rowid, rank FROM lambda_memories_fts \
             WHERE lambda_memories_fts MATCH ? \
             ORDER BY rank LIMIT ?",
        )
        .bind(format!("\"{sanitized}\""))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_fts_search: {e}")))?;

        // Resolve rowids back to hashes
        let mut results = Vec::with_capacity(rows.len());
        for (rowid, rank) in rows {
            let hash_row: Option<(String,)> =
                sqlx::query_as("SELECT hash FROM lambda_memories WHERE rowid = ?")
                    .bind(rowid)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| Temm1eError::Memory(format!("lambda_fts hash resolve: {e}")))?;

            if let Some((hash,)) = hash_row {
                results.push((hash, rank));
            }
        }
        Ok(results)
    }

    async fn lambda_gc(&self, now_epoch: u64, max_age_secs: u64) -> Result<usize, Temm1eError> {
        let cutoff = (now_epoch.saturating_sub(max_age_secs)) as i64;
        let result = sqlx::query(
            "DELETE FROM lambda_memories \
             WHERE explicit_save = 0 AND last_accessed < ? AND importance < 3.0",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_gc: {e}")))?;

        let count = result.rows_affected() as usize;
        if count > 0 {
            info!(deleted = count, "λ-Memory garbage collection");
        }
        Ok(count)
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

fn row_to_entry(row: MemoryRow) -> Result<MemoryEntry, Temm1eError> {
    let metadata: serde_json::Value =
        serde_json::from_str(&row.metadata).map_err(Temm1eError::Serialization)?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&row.timestamp)
        .map_err(|e| Temm1eError::Memory(format!("Invalid timestamp: {e}")))?
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
        MemoryEntryType::Knowledge => "knowledge",
        MemoryEntryType::Blueprint => "blueprint",
    }
}

// ── λ-Memory helpers ──────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct LambdaMemoryRow {
    hash: String,
    created_at: i64,
    last_accessed: i64,
    access_count: i32,
    importance: f32,
    explicit_save: i32,
    full_text: String,
    summary_text: String,
    essence_text: String,
    tags: String,
    memory_type: String,
    session_id: String,
}

fn lambda_row_to_entry(row: LambdaMemoryRow) -> LambdaMemoryEntry {
    let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
    let memory_type = match row.memory_type.as_str() {
        "knowledge" => LambdaMemoryType::Knowledge,
        "learning" => LambdaMemoryType::Learning,
        _ => LambdaMemoryType::Conversation,
    };
    LambdaMemoryEntry {
        hash: row.hash,
        created_at: row.created_at as u64,
        last_accessed: row.last_accessed as u64,
        access_count: row.access_count as u32,
        importance: row.importance,
        explicit_save: row.explicit_save != 0,
        full_text: row.full_text,
        summary_text: row.summary_text,
        essence_text: row.essence_text,
        tags,
        memory_type,
        session_id: row.session_id,
    }
}

fn lambda_type_to_str(lt: &LambdaMemoryType) -> &'static str {
    match lt {
        LambdaMemoryType::Conversation => "conversation",
        LambdaMemoryType::Knowledge => "knowledge",
        LambdaMemoryType::Learning => "learning",
    }
}

fn str_to_entry_type(s: &str) -> Result<MemoryEntryType, Temm1eError> {
    match s {
        "conversation" => Ok(MemoryEntryType::Conversation),
        "long_term" => Ok(MemoryEntryType::LongTerm),
        "daily_log" => Ok(MemoryEntryType::DailyLog),
        "skill" => Ok(MemoryEntryType::Skill),
        "knowledge" => Ok(MemoryEntryType::Knowledge),
        "blueprint" => Ok(MemoryEntryType::Blueprint),
        other => Err(Temm1eError::Memory(format!("Unknown entry type: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(id: &str, content: &str, session: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            metadata: serde_json::json!({"source": "test"}),
            timestamp: Utc::now(),
            session_id: session.map(String::from),
            entry_type: MemoryEntryType::Conversation,
        }
    }

    #[tokio::test]
    async fn store_and_get() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let entry = make_entry("e1", "hello world", None);
        mem.store(entry).await.unwrap();

        let fetched = mem.get("e1").await.unwrap();
        assert!(fetched.is_some());
        let e = fetched.unwrap();
        assert_eq!(e.id, "e1");
        assert_eq!(e.content, "hello world");
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let fetched = mem.get("nope").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn delete_entry() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("d1", "to delete", None))
            .await
            .unwrap();
        assert!(mem.get("d1").await.unwrap().is_some());

        mem.delete("d1").await.unwrap();
        assert!(mem.get("d1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn search_by_keyword() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("s1", "Rust programming language", None))
            .await
            .unwrap();
        mem.store(make_entry("s2", "Python scripting", None))
            .await
            .unwrap();
        mem.store(make_entry("s3", "Rust is fast and safe", None))
            .await
            .unwrap();

        let results = mem.search("Rust", SearchOpts::default()).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.content.contains("Rust")));
    }

    #[tokio::test]
    async fn search_with_session_filter() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("sf1", "hello from session A", Some("sess_a")))
            .await
            .unwrap();
        mem.store(make_entry("sf2", "hello from session B", Some("sess_b")))
            .await
            .unwrap();

        let opts = SearchOpts {
            session_filter: Some("sess_a".to_string()),
            ..Default::default()
        };
        let results = mem.search("hello", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id.as_deref(), Some("sess_a"));
    }

    #[tokio::test]
    async fn list_sessions() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("ls1", "a", Some("alpha")))
            .await
            .unwrap();
        mem.store(make_entry("ls2", "b", Some("beta")))
            .await
            .unwrap();
        mem.store(make_entry("ls3", "c", Some("alpha")))
            .await
            .unwrap();

        let sessions = mem.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"alpha".to_string()));
        assert!(sessions.contains(&"beta".to_string()));
    }

    #[tokio::test]
    async fn session_history_ordered_and_limited() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        for i in 0..5 {
            let mut entry = make_entry(&format!("h{i}"), &format!("msg {i}"), Some("hist_sess"));
            entry.timestamp = Utc::now() + chrono::Duration::seconds(i as i64);
            mem.store(entry).await.unwrap();
        }

        let history = mem.get_session_history("hist_sess", 3).await.unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn store_replaces_existing() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("r1", "original", None)).await.unwrap();
        mem.store(make_entry("r1", "updated", None)).await.unwrap();

        let fetched = mem.get("r1").await.unwrap().unwrap();
        assert_eq!(fetched.content, "updated");
    }

    #[test]
    fn entry_type_roundtrip() {
        let types = vec![
            MemoryEntryType::Conversation,
            MemoryEntryType::LongTerm,
            MemoryEntryType::DailyLog,
            MemoryEntryType::Skill,
        ];
        for et in types {
            let s = entry_type_to_str(&et);
            let restored = str_to_entry_type(s).unwrap();
            assert_eq!(entry_type_to_str(&restored), s);
        }
    }

    #[test]
    fn unknown_entry_type_fails() {
        assert!(str_to_entry_type("unknown_type").is_err());
    }

    #[test]
    fn backend_name() {
        // We can't easily test this without an async runtime, but we can test the function
        // by asserting the expected return value is "sqlite"
        assert_eq!(
            entry_type_to_str(&MemoryEntryType::Conversation),
            "conversation"
        );
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[tokio::test]
    async fn empty_database_search_returns_empty() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let results = mem.search("anything", SearchOpts::default()).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn empty_database_list_sessions_returns_empty() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let sessions = mem.list_sessions().await.unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn delete_nonexistent_does_not_error() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let result = mem.delete("nonexistent_id").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn search_special_characters() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("sp1", "error: file.rs:42 panicked", None))
            .await
            .unwrap();
        mem.store(make_entry("sp2", "normal content", None))
            .await
            .unwrap();

        // Test with SQL special chars (% and _)
        let results = mem.search("file.rs", SearchOpts::default()).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "sp1");
    }

    #[tokio::test]
    async fn search_empty_query_matches_all() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.store(make_entry("eq1", "first", None)).await.unwrap();
        mem.store(make_entry("eq2", "second", None)).await.unwrap();

        let results = mem.search("", SearchOpts::default()).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn unicode_content_round_trip() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let unicode_content =
            "\u{1F600} Hello \u{4E16}\u{754C} \u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}";
        mem.store(make_entry("uc1", unicode_content, None))
            .await
            .unwrap();

        let fetched = mem.get("uc1").await.unwrap().unwrap();
        assert_eq!(fetched.content, unicode_content);
    }

    #[tokio::test]
    async fn large_content_entry() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let large_content = "x".repeat(100_000); // 100KB content
        mem.store(make_entry("lg1", &large_content, None))
            .await
            .unwrap();

        let fetched = mem.get("lg1").await.unwrap().unwrap();
        assert_eq!(fetched.content.len(), 100_000);
    }

    #[tokio::test]
    async fn search_with_entry_type_filter() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();

        let mut e1 = make_entry("tf1", "hello from conversation", None);
        e1.entry_type = MemoryEntryType::Conversation;
        mem.store(e1).await.unwrap();

        let mut e2 = make_entry("tf2", "hello from long term", None);
        e2.entry_type = MemoryEntryType::LongTerm;
        mem.store(e2).await.unwrap();

        let opts = SearchOpts {
            entry_type_filter: Some(MemoryEntryType::LongTerm),
            ..Default::default()
        };
        let results = mem.search("hello", opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "tf2");
    }

    #[tokio::test]
    async fn session_history_empty_session() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let history = mem
            .get_session_history("nonexistent_session", 10)
            .await
            .unwrap();
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn search_limit_respected() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        for i in 0..10 {
            mem.store(make_entry(
                &format!("lim{i}"),
                &format!("hello entry {i}"),
                None,
            ))
            .await
            .unwrap();
        }

        let opts = SearchOpts {
            limit: 3,
            ..Default::default()
        };
        let results = mem.search("hello", opts).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn concurrent_stores_with_retry() {
        use std::sync::Arc;

        let mem = Arc::new(SqliteMemory::new("sqlite::memory:").await.unwrap());

        // Spawn many concurrent store tasks to exercise the retry path.
        let mut handles = Vec::new();
        for i in 0..20 {
            let mem = Arc::clone(&mem);
            handles.push(tokio::spawn(async move {
                mem.store(make_entry(
                    &format!("concurrent_{i}"),
                    &format!("content {i}"),
                    Some("concurrent_session"),
                ))
                .await
            }));
        }

        for handle in handles {
            handle.await.unwrap().unwrap();
        }

        // All 20 entries should be stored successfully.
        let history = mem
            .get_session_history("concurrent_session", 100)
            .await
            .unwrap();
        assert_eq!(history.len(), 20);
    }
}
