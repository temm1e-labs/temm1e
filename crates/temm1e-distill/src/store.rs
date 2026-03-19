//! SQLite storage for Eigen-Tune state.
//!
//! Manages four tables: `eigentune_pairs`, `eigentune_runs`,
//! `eigentune_tiers`, and `eigentune_observations`.

use chrono::{DateTime, Utc};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use temm1e_core::types::error::Temm1eError;
use tracing::info;

use crate::types::{
    EigenTier, Observation, TierRecord, TierState, TrainingPair, TrainingRun, TrainingRunStatus,
};

/// SQLite-backed storage for all Eigen-Tune persistent state.
pub struct EigenTuneStore {
    pool: SqlitePool,
}

impl EigenTuneStore {
    /// Create a new store, connect to the database, and initialise all tables.
    ///
    /// `database_url` is a SQLite connection string, e.g. `"sqlite:eigentune.db"`
    /// or `"sqlite::memory:"` for tests.
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: failed to connect: {e}")))?;

        let store = Self { pool };
        store.init_tables().await?;
        info!("EigenTune store initialised");
        Ok(store)
    }

    /// Create all tables and seed default tier records.
    async fn init_tables(&self) -> Result<(), Temm1eError> {
        // ── Pairs table ─────────────────────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS eigentune_pairs (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                turn INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                messages_json TEXT NOT NULL,
                system_prompt TEXT,
                tools_json TEXT,
                response_json TEXT NOT NULL,
                source_model TEXT NOT NULL,
                source_provider TEXT NOT NULL,
                complexity TEXT NOT NULL,
                domain_category TEXT,
                quality_alpha REAL NOT NULL DEFAULT 2.0,
                quality_beta REAL NOT NULL DEFAULT 2.0,
                quality_score REAL,
                user_continued INTEGER,
                user_retried INTEGER,
                tool_success INTEGER,
                response_error INTEGER,
                tokens_in INTEGER,
                tokens_out INTEGER,
                cost_usd REAL,
                dataset_version INTEGER,
                is_eval_holdout INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create pairs table: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_et_pairs_complexity ON eigentune_pairs(complexity)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create pairs index: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_et_pairs_conv ON eigentune_pairs(conversation_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create conv index: {e}")))?;

        // ── Runs table ──────────────────────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS eigentune_runs (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                status TEXT NOT NULL,
                base_model TEXT NOT NULL,
                backend TEXT NOT NULL,
                method TEXT NOT NULL,
                dataset_version INTEGER NOT NULL,
                pair_count INTEGER NOT NULL,
                general_mix_pct REAL NOT NULL,
                output_model_path TEXT,
                gguf_path TEXT,
                ollama_model_name TEXT,
                train_loss REAL,
                eval_loss REAL,
                epochs INTEGER,
                learning_rate REAL,
                error_message TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create runs table: {e}")))?;

        // ── Tiers table ─────────────────────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS eigentune_tiers (
                tier TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                current_run_id TEXT,
                sprt_lambda REAL NOT NULL DEFAULT 0.0,
                sprt_n INTEGER NOT NULL DEFAULT 0,
                cusum_s REAL NOT NULL DEFAULT 0.0,
                cusum_n INTEGER NOT NULL DEFAULT 0,
                pair_count INTEGER NOT NULL DEFAULT 0,
                eval_accuracy REAL,
                eval_n INTEGER,
                last_trained_at TEXT,
                last_graduated_at TEXT,
                last_demoted_at TEXT,
                serving_run_id TEXT,
                serving_since TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create tiers table: {e}")))?;

        // Seed default rows for all three tiers.
        for tier in &["simple", "standard", "complex"] {
            sqlx::query(
                "INSERT OR IGNORE INTO eigentune_tiers (tier, state, sprt_lambda, sprt_n, cusum_s, cusum_n, pair_count) VALUES (?1, 'collecting', 0.0, 0, 0.0, 0, 0)",
            )
            .bind(tier)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: seed tier {tier}: {e}")))?;
        }

        // ── Observations table ──────────────────────────────────────
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS eigentune_observations (
                id TEXT PRIMARY KEY,
                tier TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                phase TEXT NOT NULL,
                query_hash TEXT NOT NULL,
                local_response TEXT NOT NULL,
                cloud_response TEXT NOT NULL,
                judge_verdict INTEGER NOT NULL,
                judge_model TEXT NOT NULL,
                judge_reasoning TEXT,
                forward_verdict INTEGER,
                reverse_verdict INTEGER
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: create observations table: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_et_obs_tier ON eigentune_observations(tier)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: create obs index: {e}")))?;

        Ok(())
    }

    // ── Pair operations ─────────────────────────────────────────────

    /// Insert a new training pair.
    pub async fn save_pair(&self, pair: &TrainingPair) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            INSERT INTO eigentune_pairs (
                id, conversation_id, turn, created_at, messages_json,
                system_prompt, tools_json, response_json, source_model,
                source_provider, complexity, domain_category, quality_alpha,
                quality_beta, quality_score, user_continued, user_retried,
                tool_success, response_error, tokens_in, tokens_out,
                cost_usd, dataset_version, is_eval_holdout
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
            )
            "#,
        )
        .bind(&pair.id)
        .bind(&pair.conversation_id)
        .bind(pair.turn)
        .bind(pair.created_at.to_rfc3339())
        .bind(&pair.messages_json)
        .bind(&pair.system_prompt)
        .bind(&pair.tools_json)
        .bind(&pair.response_json)
        .bind(&pair.source_model)
        .bind(&pair.source_provider)
        .bind(pair.complexity.as_str())
        .bind(&pair.domain_category)
        .bind(pair.quality_alpha)
        .bind(pair.quality_beta)
        .bind(pair.quality_score)
        .bind(pair.user_continued.map(|b| b as i32))
        .bind(pair.user_retried.map(|b| b as i32))
        .bind(pair.tool_success.map(|b| b as i32))
        .bind(pair.response_error.map(|b| b as i32))
        .bind(pair.tokens_in.map(|v| v as i32))
        .bind(pair.tokens_out.map(|v| v as i32))
        .bind(pair.cost_usd)
        .bind(pair.dataset_version)
        .bind(pair.is_eval_holdout as i32)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: save pair: {e}")))?;

        Ok(())
    }

    /// Fetch a single training pair by id.
    pub async fn get_pair(&self, id: &str) -> Result<Option<TrainingPair>, Temm1eError> {
        let row = sqlx::query("SELECT * FROM eigentune_pairs WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: get pair: {e}")))?;

        Ok(row.as_ref().map(row_to_pair))
    }

    /// Update the quality Beta distribution parameters for a pair.
    pub async fn update_quality(
        &self,
        id: &str,
        alpha: f64,
        beta: f64,
        score: f64,
    ) -> Result<(), Temm1eError> {
        sqlx::query(
            "UPDATE eigentune_pairs SET quality_alpha = ?1, quality_beta = ?2, quality_score = ?3 WHERE id = ?4",
        )
        .bind(alpha)
        .bind(beta)
        .bind(score)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: update quality: {e}")))?;

        Ok(())
    }

    /// Update a specific boolean signal column on a pair.
    ///
    /// `signal_field` must be one of: `user_continued`, `user_retried`,
    /// `tool_success`, `response_error`.
    pub async fn update_signal(
        &self,
        id: &str,
        signal_field: &str,
        value: bool,
    ) -> Result<(), Temm1eError> {
        // Allowlist to prevent SQL injection
        let column = match signal_field {
            "user_continued" => "user_continued",
            "user_retried" => "user_retried",
            "tool_success" => "tool_success",
            "response_error" => "response_error",
            other => {
                return Err(Temm1eError::Memory(format!(
                    "EigenTune: invalid signal field: {other}"
                )))
            }
        };

        let sql = format!("UPDATE eigentune_pairs SET {column} = ?1 WHERE id = ?2");
        sqlx::query(&sql)
            .bind(value as i32)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: update signal: {e}")))?;

        Ok(())
    }

    /// Get all training pairs for a tier with quality score above the threshold.
    pub async fn get_pairs_for_tier(
        &self,
        tier: &str,
        min_quality: f64,
    ) -> Result<Vec<TrainingPair>, Temm1eError> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM eigentune_pairs
            WHERE complexity = ?1
              AND (quality_score IS NULL OR quality_score >= ?2)
            ORDER BY created_at DESC
            "#,
        )
        .bind(tier)
        .bind(min_quality)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: get pairs for tier: {e}")))?;

        let pairs = rows.iter().map(row_to_pair).collect();
        Ok(pairs)
    }

    /// Get the most recent training pair for a conversation.
    pub async fn get_recent_pair(
        &self,
        conversation_id: &str,
    ) -> Result<Option<TrainingPair>, Temm1eError> {
        let row = sqlx::query(
            "SELECT * FROM eigentune_pairs WHERE conversation_id = ?1 ORDER BY turn DESC LIMIT 1",
        )
        .bind(conversation_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: get recent pair: {e}")))?;

        Ok(row.as_ref().map(row_to_pair))
    }

    /// Count total pairs for a tier.
    pub async fn count_pairs(&self, tier: &str) -> Result<i64, Temm1eError> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM eigentune_pairs WHERE complexity = ?1")
            .bind(tier)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: count pairs: {e}")))?;

        Ok(row.get::<i64, _>("cnt"))
    }

    /// Count pairs for a tier with quality score above the threshold.
    pub async fn count_high_quality_pairs(
        &self,
        tier: &str,
        threshold: f64,
    ) -> Result<i64, Temm1eError> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM eigentune_pairs WHERE complexity = ?1 AND quality_score IS NOT NULL AND quality_score >= ?2",
        )
        .bind(tier)
        .bind(threshold)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: count high quality: {e}")))?;

        Ok(row.get::<i64, _>("cnt"))
    }

    /// Get domain category distribution for a tier.
    pub async fn get_category_counts(&self, tier: &str) -> Result<Vec<(String, i64)>, Temm1eError> {
        let rows = sqlx::query(
            r#"
            SELECT COALESCE(domain_category, 'uncategorized') as cat, COUNT(*) as cnt
            FROM eigentune_pairs
            WHERE complexity = ?1
            GROUP BY cat
            ORDER BY cnt DESC
            "#,
        )
        .bind(tier)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: category counts: {e}")))?;

        let counts = rows
            .iter()
            .map(|row| {
                let cat: String = row.get("cat");
                let cnt: i64 = row.get("cnt");
                (cat, cnt)
            })
            .collect();
        Ok(counts)
    }

    /// Get domain category distribution across all tiers.
    pub async fn get_all_category_counts(&self) -> Result<Vec<(String, i64)>, Temm1eError> {
        let rows = sqlx::query(
            r#"
            SELECT COALESCE(domain_category, 'uncategorized') as cat, COUNT(*) as cnt
            FROM eigentune_pairs
            GROUP BY cat
            ORDER BY cnt DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: all category counts: {e}")))?;

        let counts = rows
            .iter()
            .map(|row| {
                let cat: String = row.get("cat");
                let cnt: i64 = row.get("cnt");
                (cat, cnt)
            })
            .collect();
        Ok(counts)
    }

    /// Total number of training pairs across all tiers.
    pub async fn total_pairs(&self) -> Result<i64, Temm1eError> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM eigentune_pairs")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: total pairs: {e}")))?;

        Ok(row.get::<i64, _>("cnt"))
    }

    /// Total number of high-quality pairs across all tiers.
    pub async fn total_high_quality(&self, threshold: f64) -> Result<i64, Temm1eError> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM eigentune_pairs WHERE quality_score IS NOT NULL AND quality_score >= ?1",
        )
        .bind(threshold)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            Temm1eError::Memory(format!("EigenTune: total high quality: {e}"))
        })?;

        Ok(row.get::<i64, _>("cnt"))
    }

    // ── Run operations ──────────────────────────────────────────────

    /// Insert a new training run.
    pub async fn save_run(&self, run: &TrainingRun) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            INSERT INTO eigentune_runs (
                id, started_at, completed_at, status, base_model, backend,
                method, dataset_version, pair_count, general_mix_pct,
                output_model_path, gguf_path, ollama_model_name,
                train_loss, eval_loss, epochs, learning_rate, error_message
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
            )
            "#,
        )
        .bind(&run.id)
        .bind(run.started_at.to_rfc3339())
        .bind(run.completed_at.map(|dt| dt.to_rfc3339()))
        .bind(run.status.as_str())
        .bind(&run.base_model)
        .bind(&run.backend)
        .bind(&run.method)
        .bind(run.dataset_version)
        .bind(run.pair_count)
        .bind(run.general_mix_pct)
        .bind(&run.output_model_path)
        .bind(&run.gguf_path)
        .bind(&run.ollama_model_name)
        .bind(run.train_loss)
        .bind(run.eval_loss)
        .bind(run.epochs)
        .bind(run.learning_rate)
        .bind(&run.error_message)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: save run: {e}")))?;

        Ok(())
    }

    /// Update an existing training run (by id).
    pub async fn update_run(&self, run: &TrainingRun) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            UPDATE eigentune_runs SET
                completed_at = ?1, status = ?2, output_model_path = ?3,
                gguf_path = ?4, ollama_model_name = ?5, train_loss = ?6,
                eval_loss = ?7, epochs = ?8, learning_rate = ?9,
                error_message = ?10
            WHERE id = ?11
            "#,
        )
        .bind(run.completed_at.map(|dt| dt.to_rfc3339()))
        .bind(run.status.as_str())
        .bind(&run.output_model_path)
        .bind(&run.gguf_path)
        .bind(&run.ollama_model_name)
        .bind(run.train_loss)
        .bind(run.eval_loss)
        .bind(run.epochs)
        .bind(run.learning_rate)
        .bind(&run.error_message)
        .bind(&run.id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: update run: {e}")))?;

        Ok(())
    }

    /// Fetch a training run by id.
    pub async fn get_run(&self, id: &str) -> Result<Option<TrainingRun>, Temm1eError> {
        let row = sqlx::query("SELECT * FROM eigentune_runs WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: get run: {e}")))?;

        Ok(row.as_ref().map(row_to_run))
    }

    // ── Tier operations ─────────────────────────────────────────────

    /// Get a tier record, returning seeded defaults if the row exists.
    pub async fn get_tier(&self, tier: &str) -> Result<TierRecord, Temm1eError> {
        let row = sqlx::query("SELECT * FROM eigentune_tiers WHERE tier = ?1")
            .bind(tier)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: get tier: {e}")))?;

        match row {
            Some(ref r) => Ok(row_to_tier(r)),
            None => {
                // Insert default and return it
                sqlx::query(
                    "INSERT OR IGNORE INTO eigentune_tiers (tier, state, sprt_lambda, sprt_n, cusum_s, cusum_n, pair_count) VALUES (?1, 'collecting', 0.0, 0, 0.0, 0, 0)",
                )
                .bind(tier)
                .execute(&self.pool)
                .await
                .map_err(|e| Temm1eError::Memory(format!("EigenTune: init tier: {e}")))?;

                Ok(TierRecord {
                    tier: EigenTier::from_str(tier),
                    state: TierState::Collecting,
                    current_run_id: None,
                    sprt_lambda: 0.0,
                    sprt_n: 0,
                    cusum_s: 0.0,
                    cusum_n: 0,
                    pair_count: 0,
                    eval_accuracy: None,
                    eval_n: None,
                    last_trained_at: None,
                    last_graduated_at: None,
                    last_demoted_at: None,
                    serving_run_id: None,
                    serving_since: None,
                })
            }
        }
    }

    /// Update a tier record.
    pub async fn update_tier(&self, record: &TierRecord) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            UPDATE eigentune_tiers SET
                state = ?1, current_run_id = ?2, sprt_lambda = ?3,
                sprt_n = ?4, cusum_s = ?5, cusum_n = ?6, pair_count = ?7,
                eval_accuracy = ?8, eval_n = ?9, last_trained_at = ?10,
                last_graduated_at = ?11, last_demoted_at = ?12,
                serving_run_id = ?13, serving_since = ?14
            WHERE tier = ?15
            "#,
        )
        .bind(record.state.as_str())
        .bind(&record.current_run_id)
        .bind(record.sprt_lambda)
        .bind(record.sprt_n)
        .bind(record.cusum_s)
        .bind(record.cusum_n)
        .bind(record.pair_count)
        .bind(record.eval_accuracy)
        .bind(record.eval_n)
        .bind(record.last_trained_at.map(|dt| dt.to_rfc3339()))
        .bind(record.last_graduated_at.map(|dt| dt.to_rfc3339()))
        .bind(record.last_demoted_at.map(|dt| dt.to_rfc3339()))
        .bind(&record.serving_run_id)
        .bind(record.serving_since.map(|dt| dt.to_rfc3339()))
        .bind(record.tier.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: update tier: {e}")))?;

        Ok(())
    }

    /// Get all three tier records.
    pub async fn get_all_tiers(&self) -> Result<Vec<TierRecord>, Temm1eError> {
        let rows = sqlx::query("SELECT * FROM eigentune_tiers ORDER BY tier")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Temm1eError::Memory(format!("EigenTune: get all tiers: {e}")))?;

        Ok(rows.iter().map(row_to_tier).collect())
    }

    // ── Observation operations ──────────────────────────────────────

    /// Insert a new observation.
    pub async fn save_observation(&self, obs: &Observation) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            INSERT INTO eigentune_observations (
                id, tier, observed_at, phase, query_hash, local_response,
                cloud_response, judge_verdict, judge_model, judge_reasoning,
                forward_verdict, reverse_verdict
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
            )
            "#,
        )
        .bind(&obs.id)
        .bind(obs.tier.as_str())
        .bind(obs.observed_at.to_rfc3339())
        .bind(obs.phase.as_str())
        .bind(&obs.query_hash)
        .bind(&obs.local_response)
        .bind(&obs.cloud_response)
        .bind(obs.judge_verdict as i32)
        .bind(&obs.judge_model)
        .bind(&obs.judge_reasoning)
        .bind(obs.forward_verdict.map(|b| b as i32))
        .bind(obs.reverse_verdict.map(|b| b as i32))
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Memory(format!("EigenTune: save observation: {e}")))?;

        Ok(())
    }
}

// ── Row mapping helpers ─────────────────────────────────────────────

fn row_to_pair(row: &sqlx::sqlite::SqliteRow) -> TrainingPair {
    TrainingPair {
        id: row.get("id"),
        conversation_id: row.get("conversation_id"),
        turn: row.get("turn"),
        created_at: parse_dt(row.get::<String, _>("created_at")),
        messages_json: row.get("messages_json"),
        system_prompt: row.get("system_prompt"),
        tools_json: row.get("tools_json"),
        response_json: row.get("response_json"),
        source_model: row.get("source_model"),
        source_provider: row.get("source_provider"),
        complexity: EigenTier::from_str(row.get::<String, _>("complexity").as_str()),
        domain_category: row.get("domain_category"),
        quality_alpha: row.get("quality_alpha"),
        quality_beta: row.get("quality_beta"),
        quality_score: row.get("quality_score"),
        user_continued: row.get::<Option<i32>, _>("user_continued").map(|v| v != 0),
        user_retried: row.get::<Option<i32>, _>("user_retried").map(|v| v != 0),
        tool_success: row.get::<Option<i32>, _>("tool_success").map(|v| v != 0),
        response_error: row.get::<Option<i32>, _>("response_error").map(|v| v != 0),
        tokens_in: row.get::<Option<i32>, _>("tokens_in").map(|v| v as u32),
        tokens_out: row.get::<Option<i32>, _>("tokens_out").map(|v| v as u32),
        cost_usd: row.get("cost_usd"),
        dataset_version: row.get("dataset_version"),
        is_eval_holdout: row.get::<i32, _>("is_eval_holdout") != 0,
    }
}

fn row_to_run(row: &sqlx::sqlite::SqliteRow) -> TrainingRun {
    TrainingRun {
        id: row.get("id"),
        started_at: parse_dt(row.get::<String, _>("started_at")),
        completed_at: row.get::<Option<String>, _>("completed_at").map(parse_dt),
        status: TrainingRunStatus::from_str(row.get::<String, _>("status").as_str()),
        base_model: row.get("base_model"),
        backend: row.get("backend"),
        method: row.get("method"),
        dataset_version: row.get("dataset_version"),
        pair_count: row.get("pair_count"),
        general_mix_pct: row.get("general_mix_pct"),
        output_model_path: row.get("output_model_path"),
        gguf_path: row.get("gguf_path"),
        ollama_model_name: row.get("ollama_model_name"),
        train_loss: row.get("train_loss"),
        eval_loss: row.get("eval_loss"),
        epochs: row.get("epochs"),
        learning_rate: row.get("learning_rate"),
        error_message: row.get("error_message"),
    }
}

fn row_to_tier(row: &sqlx::sqlite::SqliteRow) -> TierRecord {
    TierRecord {
        tier: EigenTier::from_str(row.get::<String, _>("tier").as_str()),
        state: TierState::from_str(row.get::<String, _>("state").as_str()),
        current_run_id: row.get("current_run_id"),
        sprt_lambda: row.get("sprt_lambda"),
        sprt_n: row.get("sprt_n"),
        cusum_s: row.get("cusum_s"),
        cusum_n: row.get("cusum_n"),
        pair_count: row.get("pair_count"),
        eval_accuracy: row.get("eval_accuracy"),
        eval_n: row.get("eval_n"),
        last_trained_at: row
            .get::<Option<String>, _>("last_trained_at")
            .map(parse_dt),
        last_graduated_at: row
            .get::<Option<String>, _>("last_graduated_at")
            .map(parse_dt),
        last_demoted_at: row
            .get::<Option<String>, _>("last_demoted_at")
            .map(parse_dt),
        serving_run_id: row.get("serving_run_id"),
        serving_since: row.get::<Option<String>, _>("serving_since").map(parse_dt),
    }
}

/// Parse an ISO 8601 / RFC 3339 datetime string into DateTime<Utc>.
fn parse_dt(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ObservationPhase;
    use chrono::Utc;

    async fn test_store() -> EigenTuneStore {
        EigenTuneStore::new("sqlite::memory:").await.unwrap()
    }

    fn make_pair(id: &str, tier: EigenTier) -> TrainingPair {
        TrainingPair {
            id: id.to_string(),
            conversation_id: "conv-1".to_string(),
            turn: 1,
            created_at: Utc::now(),
            messages_json: r#"[{"role":"user","content":"hello"}]"#.to_string(),
            system_prompt: Some("You are helpful.".to_string()),
            tools_json: None,
            response_json: r#"{"role":"assistant","content":"Hi!"}"#.to_string(),
            source_model: "claude-sonnet-4-20250514".to_string(),
            source_provider: "anthropic".to_string(),
            complexity: tier,
            domain_category: Some("general".to_string()),
            quality_alpha: 2.0,
            quality_beta: 2.0,
            quality_score: Some(0.5),
            user_continued: None,
            user_retried: None,
            tool_success: None,
            response_error: None,
            tokens_in: Some(10),
            tokens_out: Some(20),
            cost_usd: Some(0.001),
            dataset_version: Some(1),
            is_eval_holdout: false,
        }
    }

    fn make_run(id: &str) -> TrainingRun {
        TrainingRun {
            id: id.to_string(),
            started_at: Utc::now(),
            completed_at: None,
            status: TrainingRunStatus::Running,
            base_model: "qwen2.5-7b".to_string(),
            backend: "unsloth".to_string(),
            method: "qlora".to_string(),
            dataset_version: 1,
            pair_count: 200,
            general_mix_pct: 0.1,
            output_model_path: None,
            gguf_path: None,
            ollama_model_name: None,
            train_loss: None,
            eval_loss: None,
            epochs: Some(3),
            learning_rate: Some(2e-4),
            error_message: None,
        }
    }

    #[tokio::test]
    async fn create_store() {
        let store = test_store().await;
        // All three default tiers should be seeded
        let tiers = store.get_all_tiers().await.unwrap();
        assert_eq!(tiers.len(), 3);
        for t in &tiers {
            assert_eq!(t.state, TierState::Collecting);
            assert_eq!(t.pair_count, 0);
        }
    }

    #[tokio::test]
    async fn save_and_retrieve_pair() {
        let store = test_store().await;
        let pair = make_pair("p-1", EigenTier::Simple);
        store.save_pair(&pair).await.unwrap();

        let retrieved = store.get_recent_pair("conv-1").await.unwrap();
        assert!(retrieved.is_some());
        let r = retrieved.unwrap();
        assert_eq!(r.id, "p-1");
        assert_eq!(r.complexity, EigenTier::Simple);
        assert_eq!(r.source_model, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn update_quality_score() {
        let store = test_store().await;
        let pair = make_pair("p-q", EigenTier::Standard);
        store.save_pair(&pair).await.unwrap();

        store
            .update_quality("p-q", 5.0, 1.0, 5.0 / 6.0)
            .await
            .unwrap();

        let pairs = store.get_pairs_for_tier("standard", 0.0).await.unwrap();
        assert_eq!(pairs.len(), 1);
        assert!((pairs[0].quality_alpha - 5.0).abs() < f64::EPSILON);
        assert!((pairs[0].quality_beta - 1.0).abs() < f64::EPSILON);
        let expected_score = 5.0 / 6.0;
        assert!((pairs[0].quality_score.unwrap() - expected_score).abs() < 1e-10);
    }

    #[tokio::test]
    async fn count_pairs_by_tier() {
        let store = test_store().await;
        store
            .save_pair(&make_pair("p-a", EigenTier::Simple))
            .await
            .unwrap();
        store
            .save_pair(&make_pair("p-b", EigenTier::Simple))
            .await
            .unwrap();
        store
            .save_pair(&make_pair("p-c", EigenTier::Complex))
            .await
            .unwrap();

        assert_eq!(store.count_pairs("simple").await.unwrap(), 2);
        assert_eq!(store.count_pairs("complex").await.unwrap(), 1);
        assert_eq!(store.count_pairs("standard").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn get_pairs_for_tier_quality_filter() {
        let store = test_store().await;

        let mut high = make_pair("p-high", EigenTier::Simple);
        high.quality_score = Some(0.9);
        store.save_pair(&high).await.unwrap();

        let mut low = make_pair("p-low", EigenTier::Simple);
        low.id = "p-low".to_string();
        low.quality_score = Some(0.3);
        store.save_pair(&low).await.unwrap();

        let pairs = store.get_pairs_for_tier("simple", 0.5).await.unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].id, "p-high");
    }

    #[tokio::test]
    async fn save_and_retrieve_run() {
        let store = test_store().await;
        let run = make_run("run-1");
        store.save_run(&run).await.unwrap();

        let retrieved = store.get_run("run-1").await.unwrap();
        assert!(retrieved.is_some());
        let r = retrieved.unwrap();
        assert_eq!(r.id, "run-1");
        assert_eq!(r.status, TrainingRunStatus::Running);
        assert_eq!(r.base_model, "qwen2.5-7b");

        // Update run
        let mut updated = r;
        updated.status = TrainingRunStatus::Completed;
        updated.completed_at = Some(Utc::now());
        updated.train_loss = Some(0.42);
        updated.eval_loss = Some(0.45);
        updated.output_model_path = Some("/models/run-1".to_string());
        store.update_run(&updated).await.unwrap();

        let final_run = store.get_run("run-1").await.unwrap().unwrap();
        assert_eq!(final_run.status, TrainingRunStatus::Completed);
        assert!(final_run.completed_at.is_some());
        assert!((final_run.train_loss.unwrap() - 0.42).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn save_and_retrieve_tier() {
        let store = test_store().await;

        let mut tier = store.get_tier("simple").await.unwrap();
        assert_eq!(tier.state, TierState::Collecting);

        tier.state = TierState::Training;
        tier.pair_count = 250;
        tier.current_run_id = Some("run-1".to_string());
        store.update_tier(&tier).await.unwrap();

        let updated = store.get_tier("simple").await.unwrap();
        assert_eq!(updated.state, TierState::Training);
        assert_eq!(updated.pair_count, 250);
        assert_eq!(updated.current_run_id, Some("run-1".to_string()));
    }

    #[tokio::test]
    async fn save_observation() {
        let store = test_store().await;
        let obs = Observation {
            id: "obs-1".to_string(),
            tier: EigenTier::Simple,
            observed_at: Utc::now(),
            phase: ObservationPhase::Shadow,
            query_hash: "abc123".to_string(),
            local_response: "Local says hi".to_string(),
            cloud_response: "Cloud says hello".to_string(),
            judge_verdict: true,
            judge_model: "gpt-4o".to_string(),
            judge_reasoning: Some("Equivalent responses".to_string()),
            forward_verdict: Some(true),
            reverse_verdict: Some(true),
        };
        store.save_observation(&obs).await.unwrap();

        // Verify via raw query
        let row = sqlx::query("SELECT * FROM eigentune_observations WHERE id = ?1")
            .bind("obs-1")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let tier_str: String = row.get("tier");
        assert_eq!(tier_str, "simple");
        let verdict: i32 = row.get("judge_verdict");
        assert_eq!(verdict, 1);
    }

    #[tokio::test]
    async fn total_pairs_across_tiers() {
        let store = test_store().await;
        store
            .save_pair(&make_pair("p-1", EigenTier::Simple))
            .await
            .unwrap();
        store
            .save_pair(&make_pair("p-2", EigenTier::Standard))
            .await
            .unwrap();
        store
            .save_pair(&make_pair("p-3", EigenTier::Complex))
            .await
            .unwrap();

        assert_eq!(store.total_pairs().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn category_counts() {
        let store = test_store().await;

        let mut p1 = make_pair("p-1", EigenTier::Simple);
        p1.domain_category = Some("coding".to_string());
        store.save_pair(&p1).await.unwrap();

        let mut p2 = make_pair("p-2", EigenTier::Simple);
        p2.domain_category = Some("coding".to_string());
        store.save_pair(&p2).await.unwrap();

        let mut p3 = make_pair("p-3", EigenTier::Simple);
        p3.domain_category = Some("math".to_string());
        store.save_pair(&p3).await.unwrap();

        let counts = store.get_category_counts("simple").await.unwrap();
        assert_eq!(counts.len(), 2);
        // Sorted by count DESC
        assert_eq!(counts[0].0, "coding");
        assert_eq!(counts[0].1, 2);
        assert_eq!(counts[1].0, "math");
        assert_eq!(counts[1].1, 1);
    }

    #[tokio::test]
    async fn update_signal_field() {
        let store = test_store().await;
        let pair = make_pair("p-sig", EigenTier::Simple);
        store.save_pair(&pair).await.unwrap();

        store
            .update_signal("p-sig", "user_continued", true)
            .await
            .unwrap();
        store
            .update_signal("p-sig", "tool_success", true)
            .await
            .unwrap();

        let retrieved = store.get_recent_pair("conv-1").await.unwrap().unwrap();
        assert_eq!(retrieved.user_continued, Some(true));
        assert_eq!(retrieved.tool_success, Some(true));
        assert_eq!(retrieved.user_retried, None);

        // Invalid field should error
        let result = store.update_signal("p-sig", "bad_field", true).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn high_quality_count() {
        let store = test_store().await;

        let mut p1 = make_pair("p-hq1", EigenTier::Simple);
        p1.quality_score = Some(0.8);
        store.save_pair(&p1).await.unwrap();

        let mut p2 = make_pair("p-hq2", EigenTier::Simple);
        p2.quality_score = Some(0.4);
        store.save_pair(&p2).await.unwrap();

        let mut p3 = make_pair("p-hq3", EigenTier::Simple);
        p3.quality_score = None;
        store.save_pair(&p3).await.unwrap();

        assert_eq!(
            store.count_high_quality_pairs("simple", 0.6).await.unwrap(),
            1
        );
        assert_eq!(store.total_high_quality(0.6).await.unwrap(), 1);
        assert_eq!(store.total_high_quality(0.3).await.unwrap(), 2);
    }
}
