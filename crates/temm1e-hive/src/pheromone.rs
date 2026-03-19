//! Pheromone Field — the stigmergic coordination layer.
//!
//! Workers communicate indirectly through pheromone signals stored in SQLite.
//! Each signal has a type, target, intensity, and exponential decay rate.
//! The field is read via arithmetic (no LLM calls) and garbage-collected
//! periodically to bound memory usage.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tracing::{debug, info, warn};

use temm1e_core::types::error::Temm1eError;

use crate::config::PheromoneConfig;
use crate::types::{PheromoneSignal, SignalType};

// ---------------------------------------------------------------------------
// PheromoneField
// ---------------------------------------------------------------------------

/// The pheromone field: a SQLite-backed store of time-decaying signals.
pub struct PheromoneField {
    pool: SqlitePool,
    config: PheromoneConfig,
}

impl PheromoneField {
    /// Create a new pheromone field, initializing the SQLite schema.
    pub async fn new(database_url: &str, config: PheromoneConfig) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Internal(format!("PheromoneField connect: {e}")))?;

        let field = Self { pool, config };
        field.init_tables().await?;
        info!("Pheromone field initialized");
        Ok(field)
    }

    async fn init_tables(&self) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS hive_pheromones (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                signal_type TEXT NOT NULL,
                target      TEXT NOT NULL,
                intensity   REAL NOT NULL,
                decay_rate  REAL NOT NULL,
                emitter     TEXT,
                metadata    TEXT,
                created_at  INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField init: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_pheromones_lookup \
             ON hive_pheromones(signal_type, target)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField index: {e}")))?;

        Ok(())
    }

    /// Emit a new pheromone signal.
    pub async fn emit(
        &self,
        signal_type: SignalType,
        target: &str,
        intensity: f64,
        decay_rate: f64,
        emitter: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<(), Temm1eError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let meta_str = metadata.map(|m| m.to_string());

        sqlx::query(
            "INSERT INTO hive_pheromones (signal_type, target, intensity, decay_rate, emitter, metadata, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(signal_type.as_str())
        .bind(target)
        .bind(intensity)
        .bind(decay_rate)
        .bind(emitter)
        .bind(meta_str)
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField emit: {e}")))?;

        debug!(
            signal = signal_type.as_str(),
            target = target,
            intensity = intensity,
            "Emitted pheromone"
        );
        Ok(())
    }

    /// Emit a pheromone with default intensity and decay rate for its type.
    pub async fn emit_default(
        &self,
        signal_type: SignalType,
        target: &str,
        emitter: Option<&str>,
    ) -> Result<(), Temm1eError> {
        self.emit(
            signal_type,
            target,
            signal_type.default_intensity(),
            signal_type.default_decay_rate(),
            emitter,
            None,
        )
        .await
    }

    /// Read the total intensity for a (signal_type, target) pair at current time.
    ///
    /// This is the sum of all matching signals' decayed intensities (linear superposition).
    pub async fn read_total(
        &self,
        signal_type: SignalType,
        target: &str,
    ) -> Result<f64, Temm1eError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.read_total_at(signal_type, target, now_ms).await
    }

    /// Read total intensity at a specific timestamp (for testing).
    pub async fn read_total_at(
        &self,
        signal_type: SignalType,
        target: &str,
        now_ms: i64,
    ) -> Result<f64, Temm1eError> {
        let rows: Vec<(f64, f64, i64)> = sqlx::query_as(
            "SELECT intensity, decay_rate, created_at FROM hive_pheromones \
             WHERE signal_type = ?1 AND target = ?2",
        )
        .bind(signal_type.as_str())
        .bind(target)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField read: {e}")))?;

        let total: f64 = rows
            .iter()
            .map(|(intensity, decay_rate, created_at)| {
                let sig = PheromoneSignal {
                    id: 0,
                    signal_type,
                    target: String::new(),
                    intensity: *intensity,
                    decay_rate: *decay_rate,
                    emitter: None,
                    metadata: None,
                    created_at: *created_at,
                };
                sig.intensity_at(now_ms)
            })
            .sum();

        Ok(total)
    }

    /// Read all signal type totals for a target.
    pub async fn read_all_for_target(
        &self,
        target: &str,
    ) -> Result<HashMap<SignalType, f64>, Temm1eError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut result = HashMap::new();

        for st in [
            SignalType::Completion,
            SignalType::Failure,
            SignalType::Difficulty,
            SignalType::Urgency,
            SignalType::Progress,
            SignalType::HelpWanted,
        ] {
            let total = self.read_total_at(st, target, now_ms).await?;
            if total > 0.0 {
                result.insert(st, total);
            }
        }

        Ok(result)
    }

    /// Garbage-collect expired signals. Returns the number of signals removed.
    pub async fn gc(&self) -> Result<usize, Temm1eError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.gc_at(now_ms).await
    }

    /// GC at a specific timestamp (for testing).
    pub async fn gc_at(&self, now_ms: i64) -> Result<usize, Temm1eError> {
        let threshold = self.config.evaporation_threshold;

        // Fetch all signals and check which are expired
        let rows: Vec<(i64, f64, f64, i64)> = sqlx::query_as(
            "SELECT id, intensity, decay_rate, created_at FROM hive_pheromones \
             WHERE decay_rate > 0", // only decay positive signals (not urgency)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField gc fetch: {e}")))?;

        let mut expired_ids = Vec::new();
        for (id, intensity, decay_rate, created_at) in &rows {
            let sig = PheromoneSignal {
                id: *id,
                signal_type: SignalType::Completion, // type doesn't matter for decay calc
                target: String::new(),
                intensity: *intensity,
                decay_rate: *decay_rate,
                emitter: None,
                metadata: None,
                created_at: *created_at,
            };
            if sig.intensity_at(now_ms) < threshold {
                expired_ids.push(*id);
            }
        }

        // Also GC urgency signals that have been capped for too long
        let urgency_rows: Vec<(i64, f64, f64, i64)> = sqlx::query_as(
            "SELECT id, intensity, decay_rate, created_at FROM hive_pheromones \
             WHERE decay_rate <= 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("PheromoneField gc urgency: {e}")))?;

        for (id, _intensity, _decay_rate, created_at) in &urgency_rows {
            // Urgency signals older than 30 minutes are stale
            let age_secs = (now_ms - created_at) as f64 / 1000.0;
            if age_secs > 1800.0 {
                expired_ids.push(*id);
            }
        }

        let count = expired_ids.len();
        if !expired_ids.is_empty() {
            // Delete in batches to avoid huge SQL statements
            for chunk in expired_ids.chunks(100) {
                let placeholders: Vec<String> = chunk.iter().map(|id| id.to_string()).collect();
                let sql = format!(
                    "DELETE FROM hive_pheromones WHERE id IN ({})",
                    placeholders.join(",")
                );
                sqlx::query(&sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| Temm1eError::Internal(format!("PheromoneField gc delete: {e}")))?;
            }
            debug!(removed = count, "Pheromone GC sweep");
        }

        Ok(count)
    }

    /// Start a background GC loop. Call this once after creating the field.
    pub fn start_gc_loop(self: &Arc<Self>, interval_secs: u64) {
        let field = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                match field.gc().await {
                    Ok(n) if n > 0 => {
                        debug!(removed = n, "Pheromone GC tick");
                    }
                    Err(e) => {
                        warn!(error = %e, "Pheromone GC error");
                    }
                    _ => {}
                }
            }
        });
    }

    /// Get the total number of active signals (for monitoring).
    pub async fn signal_count(&self) -> Result<usize, Temm1eError> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hive_pheromones")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| Temm1eError::Internal(format!("PheromoneField count: {e}")))?;

        Ok(count as usize)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_field() -> PheromoneField {
        PheromoneField::new("sqlite::memory:", PheromoneConfig::default())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn emit_and_read() {
        let field = make_field().await;
        let now = chrono::Utc::now().timestamp_millis();

        field
            .emit(SignalType::Completion, "t1", 1.0, 0.003, Some("w1"), None)
            .await
            .unwrap();

        let total = field
            .read_total_at(SignalType::Completion, "t1", now)
            .await
            .unwrap();
        assert!((total - 1.0).abs() < 0.1, "got {total}");
    }

    #[tokio::test]
    async fn exponential_decay() {
        let field = make_field().await;
        let base_time = 1_000_000_000_i64; // arbitrary epoch ms

        // Manually insert with known created_at
        sqlx::query(
            "INSERT INTO hive_pheromones (signal_type, target, intensity, decay_rate, created_at) \
             VALUES ('completion', 't1', 1.0, 0.003, ?1)",
        )
        .bind(base_time)
        .execute(&field.pool)
        .await
        .unwrap();

        // At creation time: intensity ≈ 1.0
        let at_0 = field
            .read_total_at(SignalType::Completion, "t1", base_time)
            .await
            .unwrap();
        assert!((at_0 - 1.0).abs() < 0.01, "at t=0: {at_0}");

        // After ~231 seconds (half-life for ρ=0.003): intensity ≈ 0.5
        let half_life_ms = (0.693 / 0.003 * 1000.0) as i64;
        let at_half = field
            .read_total_at(SignalType::Completion, "t1", base_time + half_life_ms)
            .await
            .unwrap();
        assert!((at_half - 0.5).abs() < 0.05, "at half-life: {at_half}");

        // After 10 minutes: should be very low
        let at_10min = field
            .read_total_at(SignalType::Completion, "t1", base_time + 600_000)
            .await
            .unwrap();
        assert!(at_10min < 0.2, "at 10 min: {at_10min}");
    }

    #[tokio::test]
    async fn superposition() {
        let field = make_field().await;
        let now = chrono::Utc::now().timestamp_millis();

        // Emit 3 signals of the same type on the same target
        for _ in 0..3 {
            field
                .emit(SignalType::Difficulty, "t1", 0.7, 0.006, None, None)
                .await
                .unwrap();
        }

        let total = field
            .read_total_at(SignalType::Difficulty, "t1", now)
            .await
            .unwrap();
        // Should be approximately 3 × 0.7 = 2.1
        assert!((total - 2.1).abs() < 0.1, "superposition: {total}");
    }

    #[tokio::test]
    async fn urgency_grows() {
        let field = make_field().await;
        let base_time = 1_000_000_000_i64;

        sqlx::query(
            "INSERT INTO hive_pheromones (signal_type, target, intensity, decay_rate, created_at) \
             VALUES ('urgency', 't1', 0.1, -0.001, ?1)",
        )
        .bind(base_time)
        .execute(&field.pool)
        .await
        .unwrap();

        let at_0 = field
            .read_total_at(SignalType::Urgency, "t1", base_time)
            .await
            .unwrap();
        assert!((at_0 - 0.1).abs() < 0.01);

        // After 60 seconds: should have grown
        let at_60s = field
            .read_total_at(SignalType::Urgency, "t1", base_time + 60_000)
            .await
            .unwrap();
        assert!(at_60s > 0.1, "urgency should grow: {at_60s}");

        // After very long time: capped at 5.0
        let at_far = field
            .read_total_at(SignalType::Urgency, "t1", base_time + 100_000_000)
            .await
            .unwrap();
        assert!((at_far - 5.0).abs() < 0.01, "should cap at 5.0: {at_far}");
    }

    #[tokio::test]
    async fn gc_removes_expired() {
        let field = make_field().await;
        let old_time = 0_i64; // very old

        // Insert an old completion signal that should be expired
        sqlx::query(
            "INSERT INTO hive_pheromones (signal_type, target, intensity, decay_rate, created_at) \
             VALUES ('completion', 't1', 1.0, 0.003, ?1)",
        )
        .bind(old_time)
        .execute(&field.pool)
        .await
        .unwrap();

        // Insert a fresh signal
        field
            .emit(SignalType::Completion, "t2", 1.0, 0.003, None, None)
            .await
            .unwrap();

        let before = field.signal_count().await.unwrap();
        assert_eq!(before, 2);

        let removed = field.gc().await.unwrap();
        assert_eq!(removed, 1, "should remove old signal");

        let after = field.signal_count().await.unwrap();
        assert_eq!(after, 1, "fresh signal should remain");
    }

    #[tokio::test]
    async fn gc_preserves_active() {
        let field = make_field().await;

        // Emit a fresh signal
        field
            .emit(SignalType::Failure, "t1", 1.0, 0.002, None, None)
            .await
            .unwrap();

        let removed = field.gc().await.unwrap();
        assert_eq!(removed, 0, "fresh signal should survive GC");

        let count = field.signal_count().await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn different_types_independent() {
        let field = make_field().await;
        let now = chrono::Utc::now().timestamp_millis();

        field
            .emit(SignalType::Completion, "t1", 1.0, 0.003, None, None)
            .await
            .unwrap();
        field
            .emit(SignalType::Failure, "t1", 0.5, 0.002, None, None)
            .await
            .unwrap();

        let completion = field
            .read_total_at(SignalType::Completion, "t1", now)
            .await
            .unwrap();
        let failure = field
            .read_total_at(SignalType::Failure, "t1", now)
            .await
            .unwrap();

        assert!((completion - 1.0).abs() < 0.1);
        assert!((failure - 0.5).abs() < 0.1);
    }
}
