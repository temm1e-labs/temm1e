//! The Witness Ledger — append-only, hash-chained, SQLite-backed audit trail.
//!
//! Every `OathSealed`, `ClaimSubmitted`, `EvidenceProduced`, `VerdictRendered`,
//! `Skip*`, `Task*`, and `TamperAlarm` event is appended as a row. Each row
//! carries `payload_hash` (SHA256 of payload JSON) and `entry_hash` (SHA256 of
//! `prev_entry_hash || payload_hash || created_at_ms`), chaining the history.
//!
//! The table has `BEFORE UPDATE/DELETE` triggers that raise ABORT — append-only
//! is enforced at the database level. Tampering is additionally detectable by
//! recomputing the chain from the first entry and comparing to the stored
//! `entry_hash` of the latest row.
//!
//! The root hash is also mirrored to a plain file that the `temm1e-watchdog`
//! binary reads and seals (chmod 0400) — see `anchor.rs` and
//! `crates/temm1e-watchdog/src/main.rs`.

use crate::error::WitnessError;
use crate::types::{LedgerEntry, LedgerEntryType, LedgerPayload, SessionId, SubtaskId};
use chrono::{TimeZone, Utc};
use serde_json;
use sha2::{Digest, Sha256};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

pub const LEDGER_SCHEMA_VERSION: u32 = 1;

/// Append-only hash-chained Ledger.
///
/// Not Clone — hold via Arc. All methods take `&self`.
pub struct Ledger {
    pool: SqlitePool,
    /// Serializes appends so the chain is consistent under concurrency.
    append_lock: Mutex<()>,
    /// Optional path to mirror the live root hash to (for watchdog anchor).
    live_root_path: Option<std::path::PathBuf>,
}

impl Ledger {
    /// Open or create a ledger at the given SQLite URL (e.g. `sqlite::memory:`
    /// or `sqlite:///path/to/witness.db`).
    pub async fn open(url: &str) -> Result<Arc<Self>, WitnessError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await
            .map_err(|e| WitnessError::Ledger(format!("open pool: {e}")))?;

        Self::init_schema(&pool).await?;
        Ok(Arc::new(Self {
            pool,
            append_lock: Mutex::new(()),
            live_root_path: None,
        }))
    }

    /// Configure a live root file that is rewritten after every append.
    /// `temm1e-watchdog` reads this file and seals a read-only copy.
    pub fn with_live_root_path(
        mut self: Arc<Self>,
        path: impl Into<std::path::PathBuf>,
    ) -> Arc<Self> {
        // Take unique ownership temporarily via Arc::get_mut. If there are
        // other Arcs, this fails silently — the caller should set this
        // before sharing.
        if let Some(inner) = Arc::get_mut(&mut self) {
            inner.live_root_path = Some(path.into());
        }
        self
    }

    async fn init_schema(pool: &SqlitePool) -> Result<(), WitnessError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS witness_ledger (
                entry_id         INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id       TEXT NOT NULL,
                subtask_id       TEXT,
                root_goal_id     TEXT,
                entry_type       TEXT NOT NULL,
                payload_json     TEXT NOT NULL,
                payload_hash     BLOB NOT NULL,
                prev_entry_hash  BLOB,
                entry_hash       BLOB NOT NULL UNIQUE,
                schema_version   INTEGER NOT NULL DEFAULT 1,
                witness_cost_usd REAL NOT NULL DEFAULT 0.0,
                witness_latency_ms INTEGER NOT NULL DEFAULT 0,
                created_at_ms    INTEGER NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("create table: {e}")))?;

        for stmt in [
            "CREATE INDEX IF NOT EXISTS idx_witness_ledger_session ON witness_ledger(session_id)",
            "CREATE INDEX IF NOT EXISTS idx_witness_ledger_subtask ON witness_ledger(subtask_id)",
            "CREATE INDEX IF NOT EXISTS idx_witness_ledger_entry_type ON witness_ledger(entry_type)",
            "CREATE INDEX IF NOT EXISTS idx_witness_ledger_created_at ON witness_ledger(created_at_ms)",
        ] {
            sqlx::query(stmt)
                .execute(pool)
                .await
                .map_err(|e| WitnessError::Ledger(format!("create index: {e}")))?;
        }

        // Append-only triggers. These raise SQL errors on UPDATE/DELETE.
        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS witness_ledger_no_update
            BEFORE UPDATE ON witness_ledger
            BEGIN
                SELECT RAISE(ABORT, 'witness_ledger is append-only: UPDATE is forbidden');
            END
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("create update trigger: {e}")))?;

        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS witness_ledger_no_delete
            BEFORE DELETE ON witness_ledger
            BEGIN
                SELECT RAISE(ABORT, 'witness_ledger is append-only: DELETE is forbidden');
            END
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("create delete trigger: {e}")))?;

        Ok(())
    }

    /// Append an entry to the ledger. Computes payload hash and chains it to
    /// the previous entry. Returns the stored `LedgerEntry` with its final
    /// `entry_id`, `entry_hash`, and `created_at`.
    pub async fn append(
        &self,
        session_id: SessionId,
        subtask_id: Option<SubtaskId>,
        root_goal_id: Option<String>,
        payload: LedgerPayload,
        cost_usd: f64,
        latency_ms: u64,
    ) -> Result<LedgerEntry, WitnessError> {
        let _guard = self.append_lock.lock().await;

        let entry_type = payload.entry_type();
        let payload_json = serde_json::to_string(&payload)?;
        let payload_hash = Self::hash_bytes(payload_json.as_bytes());

        let prev_hash = self.latest_entry_hash_inner().await?;

        let created_at_ms = Utc::now().timestamp_millis();
        let entry_hash = Self::hash_entry(&payload_hash, prev_hash.as_deref(), created_at_ms);

        let row = sqlx::query(
            r#"
            INSERT INTO witness_ledger (
                session_id, subtask_id, root_goal_id, entry_type,
                payload_json, payload_hash, prev_entry_hash, entry_hash,
                schema_version, witness_cost_usd, witness_latency_ms, created_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            RETURNING entry_id
            "#,
        )
        .bind(&session_id)
        .bind(&subtask_id)
        .bind(&root_goal_id)
        .bind(entry_type_str(entry_type))
        .bind(&payload_json)
        .bind(&payload_hash)
        .bind(prev_hash.as_deref())
        .bind(&entry_hash)
        .bind(LEDGER_SCHEMA_VERSION as i64)
        .bind(cost_usd)
        .bind(latency_ms as i64)
        .bind(created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("insert: {e}")))?;

        let entry_id: i64 = row
            .try_get(0)
            .map_err(|e| WitnessError::Ledger(format!("get id: {e}")))?;

        let entry = LedgerEntry {
            entry_id,
            session_id: session_id.clone(),
            subtask_id,
            root_goal_id,
            entry_type,
            payload,
            payload_hash: hex::encode(&payload_hash),
            prev_entry_hash: prev_hash.map(hex::encode),
            entry_hash: hex::encode(&entry_hash),
            schema_version: LEDGER_SCHEMA_VERSION,
            cost_usd,
            latency_ms,
            created_at: Utc
                .timestamp_millis_opt(created_at_ms)
                .single()
                .unwrap_or_else(Utc::now),
        };

        // Mirror the new latest root to the live file for the watchdog.
        if let Some(ref path) = self.live_root_path {
            let hex_root = entry.entry_hash.clone();
            if let Err(e) = tokio::fs::write(path, format!("{}\n", hex_root)).await {
                tracing::warn!(error = %e, path = %path.display(), "failed to mirror live root hash");
            }
        }

        Ok(entry)
    }

    /// Return the `entry_hash` bytes of the most recent entry, or None if empty.
    pub async fn latest_entry_hash(&self) -> Result<Option<Vec<u8>>, WitnessError> {
        self.latest_entry_hash_inner().await
    }

    async fn latest_entry_hash_inner(&self) -> Result<Option<Vec<u8>>, WitnessError> {
        let row =
            sqlx::query("SELECT entry_hash FROM witness_ledger ORDER BY entry_id DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| WitnessError::Ledger(format!("select latest: {e}")))?;
        Ok(row.and_then(|r| r.try_get::<Vec<u8>, _>(0).ok()))
    }

    /// Count entries for a session.
    pub async fn count_for_session(&self, session_id: &str) -> Result<i64, WitnessError> {
        let row = sqlx::query("SELECT COUNT(*) FROM witness_ledger WHERE session_id = ?1")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| WitnessError::Ledger(format!("count: {e}")))?;
        row.try_get::<i64, _>(0)
            .map_err(|e| WitnessError::Ledger(format!("count get: {e}")))
    }

    /// Count total entries.
    pub async fn count_total(&self) -> Result<i64, WitnessError> {
        let row = sqlx::query("SELECT COUNT(*) FROM witness_ledger")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| WitnessError::Ledger(format!("count total: {e}")))?;
        row.try_get::<i64, _>(0)
            .map_err(|e| WitnessError::Ledger(format!("count get: {e}")))
    }

    /// Read all entries for a session, ordered by entry_id.
    pub async fn read_session(&self, session_id: &str) -> Result<Vec<LedgerEntry>, WitnessError> {
        let rows = sqlx::query(
            r#"
            SELECT entry_id, session_id, subtask_id, root_goal_id, entry_type,
                   payload_json, payload_hash, prev_entry_hash, entry_hash,
                   schema_version, witness_cost_usd, witness_latency_ms, created_at_ms
            FROM witness_ledger WHERE session_id = ?1 ORDER BY entry_id ASC
            "#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("read session: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(Self::row_to_entry(row)?);
        }
        Ok(out)
    }

    /// Read all entries in the entire ledger. Used for integrity verification
    /// and tests. Do not call in hot paths on large ledgers.
    pub async fn read_all(&self) -> Result<Vec<LedgerEntry>, WitnessError> {
        let rows = sqlx::query(
            r#"
            SELECT entry_id, session_id, subtask_id, root_goal_id, entry_type,
                   payload_json, payload_hash, prev_entry_hash, entry_hash,
                   schema_version, witness_cost_usd, witness_latency_ms, created_at_ms
            FROM witness_ledger ORDER BY entry_id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| WitnessError::Ledger(format!("read all: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(Self::row_to_entry(row)?);
        }
        Ok(out)
    }

    /// Verify the hash chain from scratch. Returns Ok(()) if integral,
    /// Err(WitnessError::TamperDetected) if any row's recomputed hash
    /// disagrees with the stored value.
    pub async fn verify_integrity(&self) -> Result<(), WitnessError> {
        let entries = self.read_all().await?;
        let mut prev: Option<Vec<u8>> = None;
        for e in &entries {
            let payload_hash = Self::hash_bytes(
                serde_json::to_string(&e.payload)
                    .map_err(WitnessError::Json)?
                    .as_bytes(),
            );
            let expected = Self::hash_entry(
                &payload_hash,
                prev.as_deref(),
                e.created_at.timestamp_millis(),
            );
            let stored = hex::decode(&e.entry_hash)
                .map_err(|_| WitnessError::Ledger("bad stored hex".into()))?;
            if expected != stored {
                return Err(WitnessError::TamperDetected {
                    expected: hex::encode(&expected),
                    actual: e.entry_hash.clone(),
                });
            }
            prev = Some(stored);
        }
        Ok(())
    }

    fn row_to_entry(row: sqlx::sqlite::SqliteRow) -> Result<LedgerEntry, WitnessError> {
        let entry_id: i64 = row
            .try_get(0)
            .map_err(|e| WitnessError::Ledger(format!("col 0: {e}")))?;
        let session_id: String = row
            .try_get(1)
            .map_err(|e| WitnessError::Ledger(format!("col 1: {e}")))?;
        let subtask_id: Option<String> = row
            .try_get(2)
            .map_err(|e| WitnessError::Ledger(format!("col 2: {e}")))?;
        let root_goal_id: Option<String> = row
            .try_get(3)
            .map_err(|e| WitnessError::Ledger(format!("col 3: {e}")))?;
        let entry_type_s: String = row
            .try_get(4)
            .map_err(|e| WitnessError::Ledger(format!("col 4: {e}")))?;
        let payload_json: String = row
            .try_get(5)
            .map_err(|e| WitnessError::Ledger(format!("col 5: {e}")))?;
        let payload_hash: Vec<u8> = row
            .try_get(6)
            .map_err(|e| WitnessError::Ledger(format!("col 6: {e}")))?;
        let prev_entry_hash: Option<Vec<u8>> = row
            .try_get(7)
            .map_err(|e| WitnessError::Ledger(format!("col 7: {e}")))?;
        let entry_hash: Vec<u8> = row
            .try_get(8)
            .map_err(|e| WitnessError::Ledger(format!("col 8: {e}")))?;
        let schema_version: i64 = row
            .try_get(9)
            .map_err(|e| WitnessError::Ledger(format!("col 9: {e}")))?;
        let cost_usd: f64 = row
            .try_get(10)
            .map_err(|e| WitnessError::Ledger(format!("col 10: {e}")))?;
        let latency_ms: i64 = row
            .try_get(11)
            .map_err(|e| WitnessError::Ledger(format!("col 11: {e}")))?;
        let created_at_ms: i64 = row
            .try_get(12)
            .map_err(|e| WitnessError::Ledger(format!("col 12: {e}")))?;

        let payload: LedgerPayload = serde_json::from_str(&payload_json)?;
        let entry_type = entry_type_from_str(&entry_type_s)
            .ok_or_else(|| WitnessError::Ledger(format!("unknown entry type {}", entry_type_s)))?;

        Ok(LedgerEntry {
            entry_id,
            session_id,
            subtask_id,
            root_goal_id,
            entry_type,
            payload,
            payload_hash: hex::encode(&payload_hash),
            prev_entry_hash: prev_entry_hash.map(hex::encode),
            entry_hash: hex::encode(&entry_hash),
            schema_version: schema_version as u32,
            cost_usd,
            latency_ms: latency_ms as u64,
            created_at: Utc
                .timestamp_millis_opt(created_at_ms)
                .single()
                .unwrap_or_else(Utc::now),
        })
    }

    fn hash_bytes(b: &[u8]) -> Vec<u8> {
        let mut h = Sha256::new();
        h.update(b);
        h.finalize().to_vec()
    }

    fn hash_entry(
        payload_hash: &[u8],
        prev_entry_hash: Option<&[u8]>,
        created_at_ms: i64,
    ) -> Vec<u8> {
        let mut h = Sha256::new();
        if let Some(p) = prev_entry_hash {
            h.update(p);
        }
        h.update(payload_hash);
        h.update(created_at_ms.to_be_bytes());
        h.finalize().to_vec()
    }

    /// Borrow the underlying pool (for tests that need to execute raw SQL,
    /// such as disabling triggers to simulate tamper).
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

fn entry_type_str(t: LedgerEntryType) -> &'static str {
    match t {
        LedgerEntryType::OathSealed => "oath_sealed",
        LedgerEntryType::ClaimSubmitted => "claim_submitted",
        LedgerEntryType::EvidenceProduced => "evidence_produced",
        LedgerEntryType::VerdictRendered => "verdict_rendered",
        LedgerEntryType::SkipRequested => "skip_requested",
        LedgerEntryType::SkipApproved => "skip_approved",
        LedgerEntryType::SkipDenied => "skip_denied",
        LedgerEntryType::TaskCompleted => "task_completed",
        LedgerEntryType::TaskFailed => "task_failed",
        LedgerEntryType::TamperAlarm => "tamper_alarm",
        LedgerEntryType::CostSkipped => "cost_skipped",
    }
}

fn entry_type_from_str(s: &str) -> Option<LedgerEntryType> {
    Some(match s {
        "oath_sealed" => LedgerEntryType::OathSealed,
        "claim_submitted" => LedgerEntryType::ClaimSubmitted,
        "evidence_produced" => LedgerEntryType::EvidenceProduced,
        "verdict_rendered" => LedgerEntryType::VerdictRendered,
        "skip_requested" => LedgerEntryType::SkipRequested,
        "skip_approved" => LedgerEntryType::SkipApproved,
        "skip_denied" => LedgerEntryType::SkipDenied,
        "task_completed" => LedgerEntryType::TaskCompleted,
        "task_failed" => LedgerEntryType::TaskFailed,
        "tamper_alarm" => LedgerEntryType::TamperAlarm,
        "cost_skipped" => LedgerEntryType::CostSkipped,
        _ => return None,
    })
}

/// Read the latest root hash from a live-root file. Used by `temm1e-watchdog`
/// and by the main process to cross-check against a sealed copy.
pub fn read_root_from_file(path: &Path) -> Result<Option<String>, WitnessError> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(WitnessError::Io(e)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Claim, Oath, Predicate, Verdict, VerdictOutcome};
    use chrono::Utc;
    use std::path::PathBuf;

    async fn mem_ledger() -> Arc<Ledger> {
        Ledger::open("sqlite::memory:").await.unwrap()
    }

    fn sample_oath(subtask: &str, session: &str) -> Oath {
        let mut o = Oath::draft(subtask, "root-1", session, "do the thing").with_postcondition(
            Predicate::FileExists {
                path: PathBuf::from("/tmp/x"),
            },
        );
        o.sealed_hash = "a".repeat(64);
        o.sealed_at = Utc::now();
        o
    }

    #[tokio::test]
    async fn open_and_empty() {
        let ledger = mem_ledger().await;
        assert_eq!(ledger.count_total().await.unwrap(), 0);
        assert!(ledger.latest_entry_hash().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn append_single_entry() {
        let ledger = mem_ledger().await;
        let oath = sample_oath("st-1", "sess-1");
        let entry = ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(oath),
                0.0,
                0,
            )
            .await
            .unwrap();
        assert_eq!(entry.entry_id, 1);
        assert!(!entry.entry_hash.is_empty());
        assert!(entry.prev_entry_hash.is_none());
        assert_eq!(ledger.count_total().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn append_chains_hashes() {
        let ledger = mem_ledger().await;
        let e1 = ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-1", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();
        // Small sleep to ensure distinct created_at_ms.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let e2 = ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::ClaimSubmitted(Claim {
                    subtask_id: "st-1".into(),
                    claimed_at: Utc::now(),
                    claim_text: "done".into(),
                    evidence_refs: vec![],
                    agent_step_id: 1,
                }),
                0.0,
                0,
            )
            .await
            .unwrap();

        assert_eq!(e2.prev_entry_hash.as_deref(), Some(e1.entry_hash.as_str()));
        assert_ne!(e1.entry_hash, e2.entry_hash);
    }

    #[tokio::test]
    async fn verify_integrity_passes() {
        let ledger = mem_ledger().await;
        for i in 0..5 {
            ledger
                .append(
                    "sess-1".into(),
                    Some(format!("st-{}", i)),
                    Some("root-1".into()),
                    LedgerPayload::OathSealed(sample_oath(&format!("st-{}", i), "sess-1")),
                    0.0,
                    0,
                )
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        ledger.verify_integrity().await.unwrap();
    }

    #[tokio::test]
    async fn append_only_trigger_blocks_update() {
        let ledger = mem_ledger().await;
        ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-1", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();

        let err = sqlx::query("UPDATE witness_ledger SET payload_json = 'x' WHERE entry_id = 1")
            .execute(ledger.pool())
            .await;
        assert!(err.is_err(), "UPDATE should be blocked by trigger");
    }

    #[tokio::test]
    async fn append_only_trigger_blocks_delete() {
        let ledger = mem_ledger().await;
        ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-1", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();

        let err = sqlx::query("DELETE FROM witness_ledger WHERE entry_id = 1")
            .execute(ledger.pool())
            .await;
        assert!(err.is_err(), "DELETE should be blocked by trigger");
    }

    #[tokio::test]
    async fn tamper_detected_when_triggers_dropped() {
        let ledger = mem_ledger().await;
        ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-1", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        ledger
            .append(
                "sess-1".into(),
                Some("st-2".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-2", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();

        // Drop the update trigger temporarily for this test to simulate a
        // direct SQL tamper bypass.
        sqlx::query("DROP TRIGGER IF EXISTS witness_ledger_no_update")
            .execute(ledger.pool())
            .await
            .unwrap();
        // Tamper with created_at_ms — this invalidates the hash chain
        // without breaking JSON parsing of the payload.
        sqlx::query("UPDATE witness_ledger SET created_at_ms = 99999999999 WHERE entry_id = 1")
            .execute(ledger.pool())
            .await
            .unwrap();

        let err = ledger.verify_integrity().await;
        assert!(
            matches!(err, Err(WitnessError::TamperDetected { .. })),
            "tamper should be detected, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn live_root_mirror_writes_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let live_path = dir.path().join("latest_root.hex");

        let ledger = mem_ledger().await.with_live_root_path(live_path.clone());
        ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::OathSealed(sample_oath("st-1", "sess-1")),
                0.0,
                0,
            )
            .await
            .unwrap();

        let written = std::fs::read_to_string(&live_path).unwrap();
        let trimmed = written.trim();
        assert_eq!(trimmed.len(), 64, "expected 64-char hex root hash");
    }

    #[tokio::test]
    async fn read_session_returns_inserted_entries() {
        let ledger = mem_ledger().await;
        for i in 0..3 {
            ledger
                .append(
                    "sess-1".into(),
                    Some(format!("st-{}", i)),
                    Some("root-1".into()),
                    LedgerPayload::OathSealed(sample_oath(&format!("st-{}", i), "sess-1")),
                    0.0,
                    0,
                )
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let entries = ledger.read_session("sess-1").await.unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].entry_id, 1);
        assert_eq!(entries[2].entry_id, 3);
    }

    #[tokio::test]
    async fn verdict_roundtrip_through_ledger() {
        let ledger = mem_ledger().await;
        let verdict = Verdict {
            subtask_id: "st-1".into(),
            rendered_at: Utc::now(),
            outcome: VerdictOutcome::Pass,
            per_predicate: vec![],
            tier_usage: Default::default(),
            reason: "ok".into(),
            cost_usd: 0.0,
            latency_ms: 0,
        };
        ledger
            .append(
                "sess-1".into(),
                Some("st-1".into()),
                Some("root-1".into()),
                LedgerPayload::VerdictRendered(verdict),
                0.0,
                0,
            )
            .await
            .unwrap();
        let entries = ledger.read_session("sess-1").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].payload,
            LedgerPayload::VerdictRendered(_)
        ));
    }
}
