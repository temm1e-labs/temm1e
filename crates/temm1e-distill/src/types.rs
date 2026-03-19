//! Eigen-Tune shared types.
//!
//! All types used across the distillation pipeline: training pairs,
//! runs, tier state machines, observations, quality signals, and
//! status reports.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── EigenTier ───────────────────────────────────────────────────────

/// Maps to TaskDifficulty in temm1e-agent. Determines which local
/// model tier handles a given request after graduation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EigenTier {
    Simple,
    Standard,
    Complex,
}

impl EigenTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            EigenTier::Simple => "simple",
            EigenTier::Standard => "standard",
            EigenTier::Complex => "complex",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "simple" => EigenTier::Simple,
            "standard" => EigenTier::Standard,
            "complex" => EigenTier::Complex,
            _ => EigenTier::Standard,
        }
    }
}

impl std::fmt::Display for EigenTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── TierState ───────────────────────────────────────────────────────

/// State machine states for each tier's distillation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TierState {
    /// Accumulating training pairs from cloud provider calls.
    Collecting,
    /// Fine-tuning in progress.
    Training,
    /// Running eval suite against holdout set.
    Evaluating,
    /// Local model runs in shadow mode (cloud still serves).
    Shadowing,
    /// Local model serves traffic for this tier.
    Graduated,
}

impl TierState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TierState::Collecting => "collecting",
            TierState::Training => "training",
            TierState::Evaluating => "evaluating",
            TierState::Shadowing => "shadowing",
            TierState::Graduated => "graduated",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "collecting" => TierState::Collecting,
            "training" => TierState::Training,
            "evaluating" => TierState::Evaluating,
            "shadowing" => TierState::Shadowing,
            "graduated" => TierState::Graduated,
            _ => TierState::Collecting,
        }
    }
}

impl std::fmt::Display for TierState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── TrainingPair ────────────────────────────────────────────────────

/// One (request, response) pair captured from a provider call.
/// Quality is tracked via a Beta distribution (alpha, beta) updated
/// by implicit user signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingPair {
    pub id: String,
    pub conversation_id: String,
    pub turn: i32,
    pub created_at: DateTime<Utc>,
    /// ChatML format `[{role, content}]`.
    pub messages_json: String,
    pub system_prompt: Option<String>,
    pub tools_json: Option<String>,
    pub response_json: String,
    pub source_model: String,
    pub source_provider: String,
    pub complexity: EigenTier,
    pub domain_category: Option<String>,
    /// Beta distribution alpha (default 2.0).
    pub quality_alpha: f64,
    /// Beta distribution beta (default 2.0).
    pub quality_beta: f64,
    /// Computed quality score: alpha / (alpha + beta).
    pub quality_score: Option<f64>,
    pub user_continued: Option<bool>,
    pub user_retried: Option<bool>,
    pub tool_success: Option<bool>,
    pub response_error: Option<bool>,
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub cost_usd: Option<f64>,
    pub dataset_version: Option<i32>,
    pub is_eval_holdout: bool,
}

// ── TrainingRun ─────────────────────────────────────────────────────

/// Status of a fine-tuning run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrainingRunStatus {
    Running,
    Completed,
    Failed,
}

impl TrainingRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrainingRunStatus::Running => "running",
            TrainingRunStatus::Completed => "completed",
            TrainingRunStatus::Failed => "failed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "running" => TrainingRunStatus::Running,
            "completed" => TrainingRunStatus::Completed,
            "failed" => TrainingRunStatus::Failed,
            _ => TrainingRunStatus::Running,
        }
    }
}

/// One fine-tuning run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingRun {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: TrainingRunStatus,
    pub base_model: String,
    pub backend: String,
    pub method: String,
    pub dataset_version: i32,
    pub pair_count: i32,
    pub general_mix_pct: f64,
    pub output_model_path: Option<String>,
    pub gguf_path: Option<String>,
    pub ollama_model_name: Option<String>,
    pub train_loss: Option<f64>,
    pub eval_loss: Option<f64>,
    pub epochs: Option<i32>,
    pub learning_rate: Option<f64>,
    pub error_message: Option<String>,
}

// ── TierRecord ──────────────────────────────────────────────────────

/// Per-tier state machine state, persisted in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRecord {
    pub tier: EigenTier,
    pub state: TierState,
    pub current_run_id: Option<String>,
    pub sprt_lambda: f64,
    pub sprt_n: i32,
    pub cusum_s: f64,
    pub cusum_n: i32,
    pub pair_count: i32,
    pub eval_accuracy: Option<f64>,
    pub eval_n: Option<i32>,
    pub last_trained_at: Option<DateTime<Utc>>,
    pub last_graduated_at: Option<DateTime<Utc>>,
    pub last_demoted_at: Option<DateTime<Utc>>,
    pub serving_run_id: Option<String>,
    pub serving_since: Option<DateTime<Utc>>,
}

// ── Observation ─────────────────────────────────────────────────────

/// Phase during which an observation was collected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObservationPhase {
    /// Local model runs alongside cloud; result discarded.
    Shadow,
    /// Local model serves, cloud sampled for monitoring.
    Monitor,
}

impl ObservationPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            ObservationPhase::Shadow => "shadow",
            ObservationPhase::Monitor => "monitor",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "shadow" => ObservationPhase::Shadow,
            "monitor" => ObservationPhase::Monitor,
            _ => ObservationPhase::Shadow,
        }
    }
}

/// Shadow/monitor test result comparing local vs cloud responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: String,
    pub tier: EigenTier,
    pub observed_at: DateTime<Utc>,
    pub phase: ObservationPhase,
    pub query_hash: String,
    pub local_response: String,
    pub cloud_response: String,
    pub judge_verdict: bool,
    pub judge_model: String,
    pub judge_reasoning: Option<String>,
    pub forward_verdict: Option<bool>,
    pub reverse_verdict: Option<bool>,
}

// ── QualitySignal ───────────────────────────────────────────────────

/// User behavior signals used to update Beta distribution quality
/// scores on training pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualitySignal {
    /// User sent another message (positive).
    UserContinued,
    /// Tool call completed successfully (positive).
    ToolCallSucceeded,
    /// Conversation extended beyond 3 turns (positive).
    ConversationExtended,
    /// User re-sent the same question (negative).
    UserRetried,
    /// User explicitly rejected the response (negative).
    UserRejected,
    /// Provider returned an error (negative).
    ResponseError,
    /// User left without responding (negative).
    ConversationAbandoned,
}

impl QualitySignal {
    /// Weight magnitude for Beta distribution update.
    /// Stronger signals get higher weight.
    pub fn weight(&self) -> f64 {
        match self {
            QualitySignal::UserContinued => 1.0,
            QualitySignal::ToolCallSucceeded => 2.0,
            QualitySignal::ConversationExtended => 1.5,
            QualitySignal::UserRetried => 2.0,
            QualitySignal::UserRejected => 3.0,
            QualitySignal::ResponseError => 2.5,
            QualitySignal::ConversationAbandoned => 1.0,
        }
    }

    /// Whether this signal is positive (true) or negative (false).
    /// Positive signals increase alpha; negative increase beta.
    pub fn is_positive(&self) -> bool {
        matches!(
            self,
            QualitySignal::UserContinued
                | QualitySignal::ToolCallSucceeded
                | QualitySignal::ConversationExtended
        )
    }
}

// ── ModelEndpoint ───────────────────────────────────────────────────

/// Where a local model is serving (Ollama, vLLM, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEndpoint {
    pub base_url: String,
    pub model_name: String,
}

// ── RouteDecision ───────────────────────────────────────────────────

/// Cloud vs local routing decision made by the Eigen-Tune router.
#[derive(Debug, Clone)]
pub enum RouteDecision {
    /// Route to cloud provider (default).
    Cloud,
    /// Route to graduated local model.
    Local(ModelEndpoint),
    /// Shadow mode: cloud serves, local runs in parallel for eval.
    Shadow(ModelEndpoint),
    /// Monitor mode: local serves, cloud sampled for drift detection.
    Monitor(ModelEndpoint),
}

// ── Status Reports ──────────────────────────────────────────────────

/// Full Eigen-Tune system status report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EigenTuneStatus {
    pub enabled: bool,
    pub total_pairs: i64,
    pub high_quality_pairs: i64,
    pub diversity_j: f64,
    pub category_distribution: Vec<(String, f64)>,
    pub tiers: Vec<TierStatusReport>,
    pub total_savings_usd: f64,
}

/// Per-tier status within the Eigen-Tune report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierStatusReport {
    pub tier: EigenTier,
    pub state: TierState,
    pub pair_count: i32,
    pub accuracy: Option<f64>,
    pub accuracy_ci: Option<(f64, f64)>,
    pub sprt_lambda: Option<f64>,
    pub sprt_progress: Option<String>,
    pub serving_model: Option<String>,
    pub savings_usd: f64,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eigen_tier_serde_roundtrip() {
        let tiers = [EigenTier::Simple, EigenTier::Standard, EigenTier::Complex];
        for tier in &tiers {
            let json = serde_json::to_string(tier).unwrap();
            let back: EigenTier = serde_json::from_str(&json).unwrap();
            assert_eq!(*tier, back);
        }
        // Verify lowercase serialization
        assert_eq!(
            serde_json::to_string(&EigenTier::Simple).unwrap(),
            "\"simple\""
        );
        assert_eq!(
            serde_json::to_string(&EigenTier::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(
            serde_json::to_string(&EigenTier::Complex).unwrap(),
            "\"complex\""
        );
    }

    #[test]
    fn tier_state_serde_roundtrip() {
        let states = [
            TierState::Collecting,
            TierState::Training,
            TierState::Evaluating,
            TierState::Shadowing,
            TierState::Graduated,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let back: TierState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, back);
        }
    }

    #[test]
    fn quality_signal_weights_and_directions() {
        // Positive signals
        assert!(QualitySignal::UserContinued.is_positive());
        assert!(QualitySignal::ToolCallSucceeded.is_positive());
        assert!(QualitySignal::ConversationExtended.is_positive());

        // Negative signals
        assert!(!QualitySignal::UserRetried.is_positive());
        assert!(!QualitySignal::UserRejected.is_positive());
        assert!(!QualitySignal::ResponseError.is_positive());
        assert!(!QualitySignal::ConversationAbandoned.is_positive());

        // All weights are positive
        let signals = [
            QualitySignal::UserContinued,
            QualitySignal::ToolCallSucceeded,
            QualitySignal::ConversationExtended,
            QualitySignal::UserRetried,
            QualitySignal::UserRejected,
            QualitySignal::ResponseError,
            QualitySignal::ConversationAbandoned,
        ];
        for signal in &signals {
            assert!(
                signal.weight() > 0.0,
                "{:?} weight must be positive",
                signal
            );
        }

        // Explicit rejection is the strongest negative signal
        assert!(QualitySignal::UserRejected.weight() >= QualitySignal::UserRetried.weight());
        assert!(QualitySignal::UserRejected.weight() >= QualitySignal::ResponseError.weight());
    }

    #[test]
    fn eigen_tier_from_str() {
        assert_eq!(EigenTier::from_str("simple"), EigenTier::Simple);
        assert_eq!(EigenTier::from_str("SIMPLE"), EigenTier::Simple);
        assert_eq!(EigenTier::from_str("Standard"), EigenTier::Standard);
        assert_eq!(EigenTier::from_str("complex"), EigenTier::Complex);
        assert_eq!(EigenTier::from_str("COMPLEX"), EigenTier::Complex);
        // Unknown falls back to Standard
        assert_eq!(EigenTier::from_str("unknown"), EigenTier::Standard);
        assert_eq!(EigenTier::from_str(""), EigenTier::Standard);
    }

    #[test]
    fn tier_state_transitions() {
        // Verify the full lifecycle ordering
        let lifecycle = [
            TierState::Collecting,
            TierState::Training,
            TierState::Evaluating,
            TierState::Shadowing,
            TierState::Graduated,
        ];

        // Each state has a distinct string representation
        let mut seen = std::collections::HashSet::new();
        for state in &lifecycle {
            let s = state.as_str();
            assert!(!s.is_empty());
            assert!(seen.insert(s), "Duplicate state string: {}", s);
            // Roundtrip via from_str
            assert_eq!(TierState::from_str(s), *state);
        }

        // Unknown falls back to Collecting
        assert_eq!(TierState::from_str("bogus"), TierState::Collecting);
    }
}
