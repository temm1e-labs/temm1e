//! SQLite-backed memory implementation.

use async_trait::async_trait;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::time::Duration;
use temm1e_core::error::Temm1eError;
use temm1e_core::{
    EngramFact, FactType, LambdaMemoryEntry, LambdaMemoryType, Memory, MemoryEntry,
    MemoryEntryType, MemoryScope, PinnedBy, SearchOpts,
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

        // Enable WAL mode for concurrent read/write safety and set busy timeout
        // to avoid SQLITE_BUSY errors under contention.
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to set WAL mode: {e}")))?;
        sqlx::query("PRAGMA busy_timeout=5000")
            .execute(&pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to set busy_timeout: {e}")))?;

        let mem = Self { pool };
        mem.init_tables().await?;
        info!("SQLite memory backend initialised (WAL mode)");
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

        // Migration: add recall_boost column (v4.6.0 self-learning enhancement).
        // ALTER TABLE ADD COLUMN is a no-op if column already exists (safe to re-run).
        let _ = sqlx::query(
            "ALTER TABLE lambda_memories ADD COLUMN recall_boost REAL NOT NULL DEFAULT 0.0",
        )
        .execute(&self.pool)
        .await;

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

        // ── Engram permanent-memory table (v5.7.0) ───────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS engram_facts (
                id            TEXT PRIMARY KEY,
                content       TEXT NOT NULL,
                summary       TEXT NOT NULL,
                essence       TEXT NOT NULL,
                fact_type     TEXT NOT NULL DEFAULT 'reference',
                scope         TEXT NOT NULL DEFAULT 'global',
                pinned_by     TEXT NOT NULL DEFAULT 'none',
                subject_key   TEXT,
                importance    REAL NOT NULL DEFAULT 1.0,
                created_at    INTEGER NOT NULL,
                last_accessed INTEGER NOT NULL,
                tags          TEXT NOT NULL DEFAULT '[]',
                links         TEXT NOT NULL DEFAULT '[]'
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create engram_facts: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_eg_scope ON engram_facts(scope)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create engram index: {e}")))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_eg_subject ON engram_facts(subject_key)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create engram index: {e}")))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_eg_importance ON engram_facts(importance)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("Failed to create engram index: {e}")))?;

        // ── Tool reliability table (v4.6.0) ──────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_reliability (
                tool_name TEXT NOT NULL,
                task_type TEXT NOT NULL,
                successes INTEGER NOT NULL DEFAULT 0,
                failures INTEGER NOT NULL DEFAULT 0,
                last_updated INTEGER NOT NULL,
                PRIMARY KEY (tool_name, task_type)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create tool_reliability: {e}")))?;

        // ── Classification outcomes table (v4.6.0) ───────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS classification_outcomes (
                category TEXT NOT NULL,
                difficulty TEXT NOT NULL,
                rounds INTEGER NOT NULL,
                tools_used INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                success INTEGER NOT NULL,
                prompt_tier TEXT NOT NULL DEFAULT '',
                had_whisper INTEGER NOT NULL DEFAULT 0,
                timestamp INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| {
            Temm1eError::Memory(format!("Failed to create classification_outcomes: {e}"))
        })?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_co_timestamp ON classification_outcomes(timestamp)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| {
            Temm1eError::Memory(format!(
                "Failed to create classification_outcomes index: {e}"
            ))
        })?;

        // ── Skill usage table (v4.6.0) ───────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS skill_usage (
                skill_name TEXT PRIMARY KEY,
                invocations INTEGER NOT NULL DEFAULT 0,
                last_invoked_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create skill_usage: {e}")))?;

        // ── Model discipline table (GH-62, v5.6.0) ───────────────────
        // Per-(provider, model) Self-Audit Pass telemetry. Observability
        // only in v5.6.0; feeds adaptive auto-disable in v5.7.0+.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS model_discipline (
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                text_only_exits INTEGER NOT NULL DEFAULT 0,
                audit_done_responses INTEGER NOT NULL DEFAULT 0,
                audit_tool_call_responses INTEGER NOT NULL DEFAULT 0,
                audit_failed_responses INTEGER NOT NULL DEFAULT 0,
                audit_skipped INTEGER NOT NULL DEFAULT 0,
                last_updated INTEGER NOT NULL,
                PRIMARY KEY (provider, model)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("Failed to create model_discipline: {e}")))?;

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

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Temm1eError::Memory(format!("lambda_store begin tx: {e}")))?;

        sqlx::query(
            "INSERT OR REPLACE INTO lambda_memories \
             (hash, created_at, last_accessed, access_count, importance, explicit_save, \
              full_text, summary_text, essence_text, tags, memory_type, session_id, recall_boost) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(entry.recall_boost)
        .execute(&mut *tx)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_store failed: {e}")))?;

        // Sync FTS5: insert the searchable fields with the hash as the rowid substitute.
        let rowid: Option<(i64,)> =
            sqlx::query_as("SELECT rowid FROM lambda_memories WHERE hash = ?")
                .bind(&entry.hash)
                .fetch_optional(&mut *tx)
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
            .execute(&mut *tx)
            .await;

            sqlx::query(
                "INSERT INTO lambda_memories_fts(rowid, summary_text, essence_text, tags) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(rid)
            .bind(&entry.summary_text)
            .bind(&entry.essence_text)
            .bind(&tags_json)
            .execute(&mut *tx)
            .await
            .map_err(|e| Temm1eError::Memory(format!("lambda_store FTS insert: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| Temm1eError::Memory(format!("lambda_store commit: {e}")))?;

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
             memory_type, session_id, recall_boost \
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
             memory_type, session_id, recall_boost \
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
            "UPDATE lambda_memories \
             SET last_accessed = ?, access_count = access_count + 1, \
                 recall_boost = MIN(recall_boost + 0.3, 2.0) \
             WHERE hash = ?",
        )
        .bind(now)
        .bind(hash)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_touch: {e}")))?;

        debug!(hash = %hash, "Touched λ-memory (reheated + recall_boost)");
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

        // Weaken recall_boost for entries not accessed since cutoff (weak negative evidence).
        // Only affects entries that HAVE boost — entries with boost=0 are untouched.
        let _ = sqlx::query(
            "UPDATE lambda_memories \
             SET recall_boost = MAX(recall_boost - 0.1, 0.0) \
             WHERE explicit_save = 0 AND recall_boost > 0.0 AND last_accessed < ?",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await;

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

    async fn lambda_update_entry(&self, entry: &LambdaMemoryEntry) -> Result<(), Temm1eError> {
        let tags_json = serde_json::to_string(&entry.tags).unwrap_or_else(|_| "[]".to_string());
        sqlx::query(
            "UPDATE lambda_memories SET \
             created_at = ?, last_accessed = ?, access_count = ?, \
             importance = ?, recall_boost = ?, \
             explicit_save = ?, tags = ? \
             WHERE hash = ?",
        )
        .bind(entry.created_at as i64)
        .bind(entry.last_accessed as i64)
        .bind(entry.access_count as i32)
        .bind(entry.importance)
        .bind(entry.recall_boost)
        .bind(entry.explicit_save as i32)
        .bind(&tags_json)
        .bind(&entry.hash)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("lambda_update_entry: {e}")))?;
        Ok(())
    }

    async fn lambda_delete(&self, hash: &str) -> Result<(), Temm1eError> {
        sqlx::query("DELETE FROM lambda_memories WHERE hash = ?")
            .bind(hash)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("lambda_delete: {e}")))?;
        debug!(hash = %hash, "Deleted λ-memory entry");
        Ok(())
    }

    // ── Engram (permanent memory) ─────────────────────────────────

    async fn engram_store(&self, fact: EngramFact) -> Result<(), Temm1eError> {
        let tags = serde_json::to_string(&fact.tags).unwrap_or_else(|_| "[]".into());
        let links = serde_json::to_string(&fact.links).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "INSERT OR REPLACE INTO engram_facts \
             (id, content, summary, essence, fact_type, scope, pinned_by, subject_key, \
              importance, created_at, last_accessed, tags, links) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fact.id)
        .bind(&fact.content)
        .bind(&fact.summary)
        .bind(&fact.essence)
        .bind(fact_type_to_str(&fact.fact_type))
        .bind(fact.scope.as_key())
        .bind(pinned_by_to_str(&fact.pinned_by))
        .bind(&fact.subject_key)
        .bind(fact.importance)
        .bind(fact.created_at as i64)
        .bind(fact.last_accessed as i64)
        .bind(&tags)
        .bind(&links)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_store: {e}")))?;
        debug!(id = %fact.id, "Stored Engram fact");
        Ok(())
    }

    async fn engram_list(
        &self,
        user_id: &str,
        chat_id: &str,
        limit: usize,
    ) -> Result<Vec<EngramFact>, Temm1eError> {
        let user_key = format!("user:{user_id}");
        let chat_key = format!("chat:{chat_id}");
        let rows: Vec<EngramRow> = sqlx::query_as(
            "SELECT id, content, summary, essence, fact_type, scope, pinned_by, subject_key, \
             importance, created_at, last_accessed, tags, links \
             FROM engram_facts WHERE scope IN ('global', ?, ?) \
             ORDER BY importance DESC LIMIT ?",
        )
        .bind(&user_key)
        .bind(&chat_key)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_list: {e}")))?;
        Ok(rows.into_iter().map(engram_row_to_fact).collect())
    }

    async fn engram_get(&self, id: &str) -> Result<Option<EngramFact>, Temm1eError> {
        let row: Option<EngramRow> = sqlx::query_as(
            "SELECT id, content, summary, essence, fact_type, scope, pinned_by, subject_key, \
             importance, created_at, last_accessed, tags, links \
             FROM engram_facts WHERE id = ? LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_get: {e}")))?;
        Ok(row.map(engram_row_to_fact))
    }

    async fn engram_by_subject(
        &self,
        subject_key: &str,
        user_id: &str,
        chat_id: &str,
    ) -> Result<Option<EngramFact>, Temm1eError> {
        let user_key = format!("user:{user_id}");
        let chat_key = format!("chat:{chat_id}");
        let row: Option<EngramRow> = sqlx::query_as(
            "SELECT id, content, summary, essence, fact_type, scope, pinned_by, subject_key, \
             importance, created_at, last_accessed, tags, links \
             FROM engram_facts WHERE subject_key = ? AND scope IN ('global', ?, ?) \
             ORDER BY last_accessed DESC LIMIT 1",
        )
        .bind(subject_key)
        .bind(&user_key)
        .bind(&chat_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_by_subject: {e}")))?;
        Ok(row.map(engram_row_to_fact))
    }

    async fn engram_forget(&self, id: &str) -> Result<(), Temm1eError> {
        sqlx::query("DELETE FROM engram_facts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("engram_forget: {e}")))?;
        debug!(id = %id, "Forgot Engram fact");
        Ok(())
    }

    async fn engram_recall(
        &self,
        query: &str,
        user_id: &str,
        chat_id: &str,
        limit: usize,
    ) -> Result<Vec<EngramFact>, Temm1eError> {
        let user_key = format!("user:{user_id}");
        let chat_key = format!("chat:{chat_id}");
        // Sanitize LIKE wildcards from the query, then substring-match.
        let pat = format!("%{}%", query.replace(['%', '_'], ""));
        let rows: Vec<EngramRow> = sqlx::query_as(
            "SELECT id, content, summary, essence, fact_type, scope, pinned_by, subject_key, \
             importance, created_at, last_accessed, tags, links \
             FROM engram_facts WHERE scope IN ('global', ?, ?) \
             AND (summary LIKE ? OR essence LIKE ? OR content LIKE ? OR tags LIKE ?) \
             ORDER BY importance DESC LIMIT ?",
        )
        .bind(&user_key)
        .bind(&chat_key)
        .bind(&pat)
        .bind(&pat)
        .bind(&pat)
        .bind(&pat)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_recall: {e}")))?;
        Ok(rows.into_iter().map(engram_row_to_fact).collect())
    }

    async fn engram_gc(&self, now_epoch: u64) -> Result<usize, Temm1eError> {
        // Storage hygiene only: drop non-user-pinned facts unused >180 days with
        // low importance. Tier/visibility is derived at read time, so this is safe.
        let cutoff = now_epoch.saturating_sub(180 * 86_400) as i64;
        let res = sqlx::query(
            "DELETE FROM engram_facts WHERE pinned_by != 'user' \
             AND last_accessed < ? AND importance < 1.0",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("engram_gc: {e}")))?;
        Ok(res.rows_affected() as usize)
    }

    // ── Tool reliability (v4.6.0) ─────────────────────────────────

    async fn record_tool_outcome(
        &self,
        tool_name: &str,
        task_type: &str,
        success: bool,
    ) -> Result<(), Temm1eError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let (succ_inc, fail_inc) = if success { (1, 0) } else { (0, 1) };
        sqlx::query(
            "INSERT INTO tool_reliability (tool_name, task_type, successes, failures, last_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(tool_name, task_type) DO UPDATE SET \
             successes = successes + ?3, failures = failures + ?4, last_updated = ?5",
        )
        .bind(tool_name)
        .bind(task_type)
        .bind(succ_inc)
        .bind(fail_inc)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("record_tool_outcome: {e}")))?;

        Ok(())
    }

    async fn get_tool_reliability(
        &self,
    ) -> Result<Vec<temm1e_core::ToolReliabilityRecord>, Temm1eError> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - 30 * 86400;

        let rows: Vec<ToolReliabilityRow> = sqlx::query_as(
            "SELECT tool_name, task_type, successes, failures, last_updated \
             FROM tool_reliability WHERE last_updated > ?1 \
             ORDER BY (successes + failures) DESC LIMIT 50",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("get_tool_reliability: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| temm1e_core::ToolReliabilityRecord {
                tool_name: r.tool_name,
                task_type: r.task_type,
                successes: r.successes as u32,
                failures: r.failures as u32,
                last_updated: r.last_updated as u64,
            })
            .collect())
    }

    // ── Classification outcomes (v4.6.0) ──────────────────────────

    async fn record_classification_outcome(
        &self,
        category: &str,
        difficulty: &str,
        rounds: u32,
        tools_used: u32,
        cost_usd: f64,
        success: bool,
        prompt_tier: &str,
        had_whisper: bool,
    ) -> Result<(), Temm1eError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO classification_outcomes \
             (category, difficulty, rounds, tools_used, cost_usd, success, \
              prompt_tier, had_whisper, timestamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(category)
        .bind(difficulty)
        .bind(rounds as i32)
        .bind(tools_used as i32)
        .bind(cost_usd)
        .bind(success as i32)
        .bind(prompt_tier)
        .bind(had_whisper as i32)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("record_classification_outcome: {e}")))?;

        // Retention: keep last 500 records
        let _ = sqlx::query(
            "DELETE FROM classification_outcomes WHERE rowid NOT IN \
             (SELECT rowid FROM classification_outcomes ORDER BY timestamp DESC LIMIT 500)",
        )
        .execute(&self.pool)
        .await;

        Ok(())
    }

    async fn get_classification_priors(
        &self,
    ) -> Result<Vec<temm1e_core::ClassificationPrior>, Temm1eError> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - 30 * 86400;

        let rows: Vec<ClassificationPriorRow> = sqlx::query_as(
            "SELECT category, difficulty, \
             AVG(rounds) as avg_rounds, AVG(tools_used) as avg_tools, \
             AVG(cost_usd) as avg_cost, COUNT(*) as cnt \
             FROM classification_outcomes WHERE timestamp > ?1 \
             GROUP BY category, difficulty \
             ORDER BY cnt DESC",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("get_classification_priors: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| temm1e_core::ClassificationPrior {
                category: r.category,
                difficulty: r.difficulty,
                avg_rounds: r.avg_rounds as f32,
                avg_tools: r.avg_tools as f32,
                avg_cost: r.avg_cost,
                count: r.cnt as u32,
            })
            .collect())
    }

    // ── Skill usage (v4.6.0) ──────────────────────────────────────

    async fn record_skill_usage(&self, skill_name: &str) -> Result<(), Temm1eError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO skill_usage (skill_name, invocations, last_invoked_at) \
             VALUES (?1, 1, ?2) \
             ON CONFLICT(skill_name) DO UPDATE SET \
             invocations = invocations + 1, last_invoked_at = ?2",
        )
        .bind(skill_name)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("record_skill_usage: {e}")))?;

        Ok(())
    }

    async fn get_skill_usage(&self) -> Result<Vec<temm1e_core::SkillUsageRecord>, Temm1eError> {
        let rows: Vec<SkillUsageRow> = sqlx::query_as(
            "SELECT skill_name, invocations, last_invoked_at \
             FROM skill_usage ORDER BY invocations DESC LIMIT 30",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("get_skill_usage: {e}")))?;

        Ok(rows
            .into_iter()
            .map(|r| temm1e_core::SkillUsageRecord {
                skill_name: r.skill_name,
                invocations: r.invocations as u32,
                last_invoked_at: r.last_invoked_at as u64,
            })
            .collect())
    }

    async fn record_audit_outcome(
        &self,
        provider: &str,
        model: &str,
        outcome: temm1e_core::AuditOutcomeKind,
        was_text_only: bool,
    ) -> Result<(), Temm1eError> {
        use temm1e_core::AuditOutcomeKind;
        let now = chrono::Utc::now().timestamp();
        // Increment one of: text_only_exits (always when was_text_only),
        // and exactly one of the per-outcome columns.
        let (done_inc, tool_inc, fail_inc, skip_inc) = match outcome {
            AuditOutcomeKind::Done => (1, 0, 0, 0),
            AuditOutcomeKind::ToolCallTriggered => (0, 1, 0, 0),
            AuditOutcomeKind::FailedOpen => (0, 0, 1, 0),
            AuditOutcomeKind::Skipped => (0, 0, 0, 1),
        };
        let text_only_inc: i64 = if was_text_only { 1 } else { 0 };

        sqlx::query(
            "INSERT INTO model_discipline \
                (provider, model, text_only_exits, audit_done_responses, \
                 audit_tool_call_responses, audit_failed_responses, audit_skipped, last_updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(provider, model) DO UPDATE SET \
                text_only_exits = text_only_exits + ?3, \
                audit_done_responses = audit_done_responses + ?4, \
                audit_tool_call_responses = audit_tool_call_responses + ?5, \
                audit_failed_responses = audit_failed_responses + ?6, \
                audit_skipped = audit_skipped + ?7, \
                last_updated = ?8",
        )
        .bind(provider)
        .bind(model)
        .bind(text_only_inc)
        .bind(done_inc as i64)
        .bind(tool_inc as i64)
        .bind(fail_inc as i64)
        .bind(skip_inc as i64)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("record_audit_outcome: {e}")))?;

        Ok(())
    }

    async fn get_model_discipline(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Option<temm1e_core::ModelDiscipline>, Temm1eError> {
        let row: Option<ModelDisciplineRow> = sqlx::query_as(
            "SELECT provider, model, text_only_exits, audit_done_responses, \
                    audit_tool_call_responses, audit_failed_responses, audit_skipped, last_updated \
             FROM model_discipline WHERE provider = ?1 AND model = ?2",
        )
        .bind(provider)
        .bind(model)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("get_model_discipline: {e}")))?;

        Ok(row.map(|r| temm1e_core::ModelDiscipline {
            provider: r.provider,
            model: r.model,
            text_only_exits: r.text_only_exits as u64,
            audit_done_responses: r.audit_done_responses as u64,
            audit_tool_call_responses: r.audit_tool_call_responses as u64,
            audit_failed_responses: r.audit_failed_responses as u64,
            audit_skipped: r.audit_skipped as u64,
            last_updated: r.last_updated as u64,
        }))
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
    recall_boost: f32,
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
        recall_boost: row.recall_boost,
    }
}

fn lambda_type_to_str(lt: &LambdaMemoryType) -> &'static str {
    match lt {
        LambdaMemoryType::Conversation => "conversation",
        LambdaMemoryType::Knowledge => "knowledge",
        LambdaMemoryType::Learning => "learning",
    }
}

#[derive(sqlx::FromRow)]
struct EngramRow {
    id: String,
    content: String,
    summary: String,
    essence: String,
    fact_type: String,
    scope: String,
    pinned_by: String,
    subject_key: Option<String>,
    importance: f32,
    created_at: i64,
    last_accessed: i64,
    tags: String,
    links: String,
}

fn engram_row_to_fact(row: EngramRow) -> EngramFact {
    EngramFact {
        id: row.id,
        content: row.content,
        summary: row.summary,
        essence: row.essence,
        fact_type: str_to_fact_type(&row.fact_type),
        scope: MemoryScope::from_key(&row.scope),
        pinned_by: str_to_pinned_by(&row.pinned_by),
        subject_key: row.subject_key,
        importance: row.importance,
        created_at: row.created_at as u64,
        last_accessed: row.last_accessed as u64,
        tags: serde_json::from_str(&row.tags).unwrap_or_default(),
        links: serde_json::from_str(&row.links).unwrap_or_default(),
    }
}

fn fact_type_to_str(t: &FactType) -> &'static str {
    match t {
        FactType::Identity => "identity",
        FactType::Preference => "preference",
        FactType::Project => "project",
        FactType::Constraint => "constraint",
        FactType::Reference => "reference",
    }
}

fn str_to_fact_type(s: &str) -> FactType {
    match s {
        "identity" => FactType::Identity,
        "preference" => FactType::Preference,
        "project" => FactType::Project,
        "constraint" => FactType::Constraint,
        _ => FactType::Reference,
    }
}

fn pinned_by_to_str(p: &PinnedBy) -> &'static str {
    match p {
        PinnedBy::None => "none",
        PinnedBy::Agent => "agent",
        PinnedBy::User => "user",
    }
}

fn str_to_pinned_by(s: &str) -> PinnedBy {
    match s {
        "agent" => PinnedBy::Agent,
        "user" => PinnedBy::User,
        _ => PinnedBy::None,
    }
}

#[derive(sqlx::FromRow)]
struct ToolReliabilityRow {
    tool_name: String,
    task_type: String,
    successes: i32,
    failures: i32,
    last_updated: i64,
}

#[derive(sqlx::FromRow)]
struct ClassificationPriorRow {
    category: String,
    difficulty: String,
    avg_rounds: f64,
    avg_tools: f64,
    avg_cost: f64,
    cnt: i64,
}

#[derive(sqlx::FromRow)]
struct SkillUsageRow {
    skill_name: String,
    invocations: i32,
    last_invoked_at: i64,
}

#[derive(sqlx::FromRow)]
struct ModelDisciplineRow {
    provider: String,
    model: String,
    text_only_exits: i64,
    audit_done_responses: i64,
    audit_tool_call_responses: i64,
    audit_failed_responses: i64,
    audit_skipped: i64,
    last_updated: i64,
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

    fn mk_fact(
        id: &str,
        scope: MemoryScope,
        subject: Option<&str>,
        imp: f32,
        pin: PinnedBy,
    ) -> EngramFact {
        EngramFact {
            id: id.to_string(),
            content: format!("content for {id} about gpu"),
            summary: format!("summary {id}"),
            essence: format!("ess {id}"),
            fact_type: FactType::Preference,
            scope,
            pinned_by: pin,
            subject_key: subject.map(String::from),
            importance: imp,
            created_at: 1000,
            last_accessed: 1000,
            tags: vec!["gpu".to_string()],
            links: vec![],
        }
    }

    #[tokio::test]
    async fn engram_roundtrip_and_scope_isolation() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.engram_store(mk_fact(
            "g1",
            MemoryScope::Global,
            Some("pref:gpu"),
            4.0,
            PinnedBy::Agent,
        ))
        .await
        .unwrap();
        mem.engram_store(mk_fact(
            "u1",
            MemoryScope::User("alice".into()),
            None,
            5.0,
            PinnedBy::User,
        ))
        .await
        .unwrap();
        mem.engram_store(mk_fact(
            "c1",
            MemoryScope::Chat("room1".into()),
            None,
            3.0,
            PinnedBy::None,
        ))
        .await
        .unwrap();
        mem.engram_store(mk_fact(
            "u2",
            MemoryScope::User("bob".into()),
            None,
            5.0,
            PinnedBy::User,
        ))
        .await
        .unwrap();

        // alice@room1 sees global + her own + this chat — NOT bob's user fact.
        let listed = mem.engram_list("alice", "room1", 50).await.unwrap();
        let ids: Vec<&str> = listed.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"g1") && ids.contains(&"u1") && ids.contains(&"c1"));
        assert!(!ids.contains(&"u2"), "scope leak: bob's fact reached alice");
        assert_eq!(listed.first().unwrap().importance, 5.0, "importance DESC");

        // by_subject within scope
        let s = mem
            .engram_by_subject("pref:gpu", "alice", "room1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.id, "g1");

        // fidelity roundtrip
        let u1 = mem.engram_get("u1").await.unwrap().unwrap();
        assert_eq!(u1.pinned_by, PinnedBy::User);
        assert_eq!(u1.scope, MemoryScope::User("alice".into()));
        assert_eq!(u1.fact_type, FactType::Preference);
        assert_eq!(u1.tags, vec!["gpu".to_string()]);

        // recall is scope-filtered: bob sees the global fact + his OWN (u2),
        // but never alice's (u1) or another chat's (c1).
        let r_bob = mem.engram_recall("gpu", "bob", "other", 50).await.unwrap();
        let bids: Vec<&str> = r_bob.iter().map(|f| f.id.as_str()).collect();
        assert!(bids.contains(&"g1"), "bob should see the global fact");
        assert!(
            !bids.contains(&"u1") && !bids.contains(&"c1"),
            "scope leak in recall: bob saw alice's or room1's fact"
        );

        // forget
        mem.engram_forget("g1").await.unwrap();
        assert!(mem.engram_get("g1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn engram_store_is_upsert() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        mem.engram_store(mk_fact(
            "x",
            MemoryScope::Global,
            None,
            2.0,
            PinnedBy::Agent,
        ))
        .await
        .unwrap();
        mem.engram_store(mk_fact(
            "x",
            MemoryScope::Global,
            None,
            4.5,
            PinnedBy::Agent,
        ))
        .await
        .unwrap();
        let got = mem.engram_get("x").await.unwrap().unwrap();
        assert_eq!(got.importance, 4.5, "second store replaces the first");
        let all = mem.engram_list("anyone", "anywhere", 50).await.unwrap();
        assert_eq!(all.iter().filter(|f| f.id == "x").count(), 1, "no dup");
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

    // ── Model discipline (GH-62) ──────────────────────────────────

    #[tokio::test]
    async fn record_audit_outcome_increments_correct_counter() {
        use temm1e_core::AuditOutcomeKind;
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let p = "openai-compat";
        let m = "qwen3-27b";

        mem.record_audit_outcome(p, m, AuditOutcomeKind::Done, true)
            .await
            .unwrap();
        mem.record_audit_outcome(p, m, AuditOutcomeKind::Done, true)
            .await
            .unwrap();
        mem.record_audit_outcome(p, m, AuditOutcomeKind::ToolCallTriggered, true)
            .await
            .unwrap();
        mem.record_audit_outcome(p, m, AuditOutcomeKind::FailedOpen, true)
            .await
            .unwrap();
        mem.record_audit_outcome(p, m, AuditOutcomeKind::Skipped, false)
            .await
            .unwrap();

        let d = mem
            .get_model_discipline(p, m)
            .await
            .unwrap()
            .expect("row should exist");
        assert_eq!(d.text_only_exits, 4);
        assert_eq!(d.audit_done_responses, 2);
        assert_eq!(d.audit_tool_call_responses, 1);
        assert_eq!(d.audit_failed_responses, 1);
        assert_eq!(d.audit_skipped, 1);
        assert!(d.last_updated > 0);
    }

    #[tokio::test]
    async fn get_model_discipline_returns_none_for_unseen() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let d = mem.get_model_discipline("never", "seen").await.unwrap();
        assert!(d.is_none());
    }

    #[tokio::test]
    async fn concurrent_audit_record_safe() {
        use temm1e_core::AuditOutcomeKind;
        let mem = std::sync::Arc::new(SqliteMemory::new("sqlite::memory:").await.unwrap());
        let mut handles = Vec::new();
        for _ in 0..20 {
            let mem = mem.clone();
            handles.push(tokio::spawn(async move {
                mem.record_audit_outcome("p", "m", AuditOutcomeKind::Done, true)
                    .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        let d = mem.get_model_discipline("p", "m").await.unwrap().unwrap();
        assert_eq!(d.text_only_exits, 20);
        assert_eq!(d.audit_done_responses, 20);
    }
}
