# Eigen-Tune — Implementation Plan

## Reference Document for Implementation

**Purpose:** This is the build guide. Every struct, function, file, and test is specified here. Follow this document top-to-bottom during implementation.

---

## Phase 1: Crate Scaffold + Core Types

### 1.1 Create `crates/temm1e-distill/`

```
crates/temm1e-distill/
├── Cargo.toml
├── src/
│   ├── lib.rs              # EigenTuneEngine struct, public API
│   ├── collector.rs        # Captures (request, response) pairs
│   ├── scorer.rs           # Beta-Binomial quality scoring
│   ├── curator.rs          # Dataset building, dedup, balancing, export
│   ├── store.rs            # SQLite storage (eigentune_* tables)
│   ├── config.rs           # EigenTuneConfig (re-exported)
│   ├── types.rs            # Shared types (TrainingPair, TierState, etc.)
│   ├── status.rs           # Observability, status reporting
│   │
│   ├── stats/              # Mathematical engines (pure, no deps)
│   │   ├── mod.rs
│   │   ├── sprt.rs         # Sequential Probability Ratio Test
│   │   ├── cusum.rs        # Cumulative Sum drift detection
│   │   ├── wilson.rs       # Wilson score confidence intervals
│   │   ├── entropy.rs      # Shannon entropy for diversity
│   │   ├── thompson.rs     # Thompson sampling for curation
│   │   ├── beta.rs         # Beta distribution utilities
│   │   └── power.rs        # Sample size / power analysis
│   │
│   ├── engine/             # Pipeline orchestration
│   │   ├── mod.rs
│   │   ├── state_machine.rs # Per-tier state management
│   │   ├── trainer.rs      # Training orchestration
│   │   ├── evaluator.rs    # Benchmark runner
│   │   ├── shadow.rs       # Shadow test coordinator (SPRT)
│   │   ├── monitor.rs      # Production CUSUM monitor
│   │   ├── router.rs       # Cloud vs local routing decision
│   │   └── graduation.rs   # Graduation/demotion logic
│   │
│   ├── backends/           # Pluggable training backends
│   │   ├── mod.rs
│   │   ├── unsloth.rs      # Unsloth/Axolotl
│   │   ├── mlx.rs          # MLX (Apple Silicon)
│   │   ├── hf_autotrain.rs # HuggingFace AutoTrain API
│   │   └── ollama.rs       # Ollama model deploy/serve
│   │
│   └── judge/              # Response comparison
│       ├── mod.rs
│       ├── embedding.rs    # Cosine similarity via local embeddings (default)
│       ├── behavior.rs     # User behavior signals for SPRT/CUSUM (default)
│       └── teacher.rs      # LLM-as-judge, optional, costs money (opt-in)
│
└── tests/
    └── (inline #[cfg(test)] modules)
```

### 1.2 Cargo.toml

```toml
[package]
name = "temm1e-distill"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[dependencies]
temm1e-core = { path = "../temm1e-core" }
tokio = { workspace = true }
sqlx = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
rand = { workspace = true }
sha2 = "0.10"                   # for query hashing

[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
temm1e-test-utils = { path = "../temm1e-test-utils" }
```

### 1.3 Root Cargo.toml Changes

Add to workspace members:
```toml
members = [
    # ... existing ...
    "crates/temm1e-distill",
]
```

Add workspace dependency:
```toml
temm1e-distill = { path = "crates/temm1e-distill" }
```

Add feature flag:
```toml
[features]
default = ["telegram", "browser", "mcp", "codex-oauth", "eigentune"]
eigentune = ["dep:temm1e-distill"]
```

### 1.4 Types (`src/types.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Tier & State ───

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EigenTier {
    Simple,
    Standard,
    Complex,
}

impl EigenTier {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Simple => "simple",
            Self::Standard => "standard",
            Self::Complex => "complex",
        }
    }

    pub fn from_difficulty(difficulty: &str) -> Self {
        match difficulty.to_lowercase().as_str() {
            "simple" => Self::Simple,
            "standard" => Self::Standard,
            "complex" => Self::Complex,
            _ => Self::Standard, // default
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TierState {
    Collecting,
    Training,
    Evaluating,
    Shadowing,
    Graduated,
}

impl TierState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Collecting => "collecting",
            Self::Training => "training",
            Self::Evaluating => "evaluating",
            Self::Shadowing => "shadowing",
            Self::Graduated => "graduated",
        }
    }
}

// ─── Training Pair ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingPair {
    pub id: String,
    pub conversation_id: String,
    pub turn: i32,
    pub created_at: DateTime<Utc>,

    // The data
    pub messages_json: String,       // ChatML format
    pub system_prompt: Option<String>,
    pub tools_json: Option<String>,
    pub response_json: String,

    // Source
    pub source_model: String,
    pub source_provider: String,
    pub complexity: EigenTier,
    pub domain_category: Option<String>,

    // Quality (Beta-Binomial)
    pub quality_alpha: f64,
    pub quality_beta: f64,
    pub quality_score: Option<f64>,

    // Signals
    pub user_continued: Option<bool>,
    pub user_retried: Option<bool>,
    pub tool_success: Option<bool>,
    pub response_error: Option<bool>,

    // Cost
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub cost_usd: Option<f64>,

    // Curation
    pub dataset_version: Option<i32>,
    pub is_eval_holdout: bool,
}

// ─── Training Run ───

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrainingRunStatus {
    Running,
    Completed,
    Failed,
}

// ─── Tier State Record ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierRecord {
    pub tier: EigenTier,
    pub state: TierState,
    pub current_run_id: Option<String>,

    // SPRT
    pub sprt_lambda: f64,
    pub sprt_n: i32,

    // CUSUM
    pub cusum_s: f64,
    pub cusum_n: i32,

    // Stats
    pub pair_count: i32,
    pub eval_accuracy: Option<f64>,
    pub eval_n: Option<i32>,

    // Timestamps
    pub last_trained_at: Option<DateTime<Utc>>,
    pub last_graduated_at: Option<DateTime<Utc>>,
    pub last_demoted_at: Option<DateTime<Utc>>,

    // Serving
    pub serving_run_id: Option<String>,
    pub serving_since: Option<DateTime<Utc>>,
}

// ─── Observation ───

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationPhase {
    Shadow,
    Monitor,
}

// ─── Training Backend Types ───

#[derive(Debug, Clone)]
pub struct BaseModelInfo {
    pub name: String,           // e.g., "meta-llama/Llama-3.1-8B"
    pub parameter_count: u64,   // e.g., 8_000_000_000
    pub quantization: Option<String>, // e.g., "Q4_K_M"
}

#[derive(Debug, Clone)]
pub struct TrainJobConfig {
    pub base_model: String,
    pub dataset_path: String,
    pub output_path: String,
    pub method: String,          // "qlora" | "lora" | "sft"
    pub epochs: u32,
    pub learning_rate: f64,
    pub lora_rank: u32,
    pub lora_alpha: u32,
    pub general_data_path: Option<String>,
    pub general_mix_pct: f64,
}

#[derive(Debug, Clone)]
pub struct TrainResult {
    pub model_path: String,
    pub train_loss: f64,
    pub eval_loss: f64,
    pub epochs_completed: u32,
    pub duration_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ModelEndpoint {
    pub base_url: String,        // e.g., "http://localhost:11434"
    pub model_name: String,      // e.g., "eigentune-v3"
}

#[derive(Debug, Clone)]
pub struct JudgeVerdict {
    pub agree: bool,
    pub reasoning: String,
    pub confidence: f64,         // 0.0-1.0
}

// ─── Quality Signal ───

#[derive(Debug, Clone, Copy)]
pub enum QualitySignal {
    UserContinued,          // weight: +1.0
    ToolCallSucceeded,      // weight: +1.5
    ConversationExtended,   // weight: +0.5
    UserRetried,            // weight: -2.0
    UserRejected,           // weight: -2.5
    ResponseError,          // weight: -2.0
    ConversationAbandoned,  // weight: -0.5
}

impl QualitySignal {
    pub fn weight(&self) -> f64 {
        match self {
            Self::UserContinued => 1.0,
            Self::ToolCallSucceeded => 1.5,
            Self::ConversationExtended => 0.5,
            Self::UserRetried => 2.0,
            Self::UserRejected => 2.5,
            Self::ResponseError => 2.0,
            Self::ConversationAbandoned => 0.5,
        }
    }

    pub fn is_positive(&self) -> bool {
        matches!(
            self,
            Self::UserContinued | Self::ToolCallSucceeded | Self::ConversationExtended
        )
    }
}

// ─── Status Report ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EigenTuneStatus {
    pub enabled: bool,
    pub total_pairs: i64,
    pub high_quality_pairs: i64,
    pub diversity_j: f64,
    pub category_distribution: Vec<(String, f64)>,
    pub tiers: Vec<TierStatusReport>,
    pub total_savings_usd: f64,
    pub last_curation: Option<DateTime<Utc>>,
    pub next_curation: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierStatusReport {
    pub tier: EigenTier,
    pub state: TierState,
    pub pair_count: i32,
    pub accuracy: Option<f64>,
    pub accuracy_ci: Option<(f64, f64)>,     // Wilson CI
    pub sprt_lambda: Option<f64>,
    pub sprt_progress: Option<String>,        // "187/500"
    pub serving_model: Option<String>,
    pub savings_usd: f64,
}
```

### 1.5 Config (`src/config.rs`)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EigenTuneConfig {
    #[serde(default)]
    pub enabled: bool,

    // Graduation thresholds
    #[serde(default = "default_graduation_accuracy")]
    pub graduation_accuracy: f64,           // 0.95
    #[serde(default = "default_graduation_confidence")]
    pub graduation_confidence: f64,         // 0.99
    #[serde(default = "default_min_training_pairs")]
    pub min_training_pairs: u32,            // 500
    #[serde(default = "default_min_eval_samples")]
    pub min_eval_samples: u32,              // 568

    // SPRT
    #[serde(default = "default_sprt_p0")]
    pub sprt_p0: f64,                       // 0.92
    #[serde(default = "default_sprt_p1")]
    pub sprt_p1: f64,                       // 0.97
    #[serde(default = "default_sprt_alpha")]
    pub sprt_alpha: f64,                    // 0.01
    #[serde(default = "default_sprt_beta")]
    pub sprt_beta: f64,                     // 0.01
    #[serde(default = "default_sprt_max_samples")]
    pub sprt_max_samples: u32,              // 500

    // CUSUM
    #[serde(default = "default_monitor_sample_rate")]
    pub monitor_sample_rate: f64,           // 0.05
    #[serde(default = "default_cusum_target")]
    pub cusum_target: f64,                  // 0.95
    #[serde(default = "default_cusum_slack")]
    pub cusum_slack: f64,                   // 0.109
    #[serde(default = "default_cusum_threshold")]
    pub cusum_threshold: f64,               // 1.090
    #[serde(default = "default_cusum_fir")]
    pub cusum_fir: bool,                    // true

    // Dataset
    #[serde(default = "default_diversity_threshold")]
    pub diversity_threshold: f64,           // 0.75
    #[serde(default = "default_general_data_mix")]
    pub general_data_mix: f64,              // 0.05
    #[serde(default = "default_quality_threshold")]
    pub quality_threshold: f64,             // 0.7
    #[serde(default = "default_curation_interval")]
    pub curation_interval: String,          // "6h"

    // Training
    #[serde(default = "default_training_backend")]
    pub training_backend: String,           // "auto"
    #[serde(default = "default_base_model")]
    pub base_model: String,                 // "auto"
    #[serde(default = "default_training_method")]
    pub training_method: String,            // "qlora"
    #[serde(default = "default_training_epochs")]
    pub training_epochs: u32,               // 3
    #[serde(default = "default_training_lr")]
    pub training_learning_rate: f64,        // 0.0002
    #[serde(default = "default_lora_rank")]
    pub lora_rank: u32,                     // 16
    #[serde(default = "default_lora_alpha")]
    pub lora_alpha: u32,                    // 32

    // Teacher (opt-in LLM judge)
    #[serde(default)]
    pub teacher_enabled: bool,              // false
    #[serde(default = "default_teacher_model")]
    pub teacher_model: String,              // "auto"

    // Observability
    #[serde(default = "default_show_routing")]
    pub show_routing_indicator: bool,       // true
    #[serde(default = "default_notify_train")]
    pub notify_on_train: bool,              // true
    #[serde(default = "default_notify_graduate")]
    pub notify_on_graduate: bool,           // true
    #[serde(default = "default_notify_demote")]
    pub notify_on_demote: bool,             // true
    #[serde(default = "default_status_interval")]
    pub status_interval: String,            // "weekly"
}

impl Default for EigenTuneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            graduation_accuracy: 0.95,
            graduation_confidence: 0.99,
            min_training_pairs: 500,
            min_eval_samples: 568,
            sprt_p0: 0.92,
            sprt_p1: 0.97,
            sprt_alpha: 0.01,
            sprt_beta: 0.01,
            sprt_max_samples: 500,
            monitor_sample_rate: 0.05,
            cusum_target: 0.95,
            cusum_slack: 0.109,
            cusum_threshold: 1.090,
            cusum_fir: true,
            diversity_threshold: 0.75,
            general_data_mix: 0.05,
            quality_threshold: 0.7,
            curation_interval: "6h".to_string(),
            training_backend: "auto".to_string(),
            base_model: "auto".to_string(),
            training_method: "qlora".to_string(),
            training_epochs: 3,
            training_learning_rate: 0.0002,
            lora_rank: 16,
            lora_alpha: 32,
            teacher_enabled: false,
            teacher_model: "auto".to_string(),
            show_routing_indicator: true,
            notify_on_train: true,
            notify_on_graduate: true,
            notify_on_demote: true,
            status_interval: "weekly".to_string(),
        }
    }
}

// Default functions for serde
fn default_graduation_accuracy() -> f64 { 0.95 }
fn default_graduation_confidence() -> f64 { 0.99 }
fn default_min_training_pairs() -> u32 { 500 }
fn default_min_eval_samples() -> u32 { 568 }
fn default_sprt_p0() -> f64 { 0.92 }
fn default_sprt_p1() -> f64 { 0.97 }
fn default_sprt_alpha() -> f64 { 0.01 }
fn default_sprt_beta() -> f64 { 0.01 }
fn default_sprt_max_samples() -> u32 { 500 }
fn default_monitor_sample_rate() -> f64 { 0.05 }
fn default_cusum_target() -> f64 { 0.95 }
fn default_cusum_slack() -> f64 { 0.109 }
fn default_cusum_threshold() -> f64 { 1.090 }
fn default_cusum_fir() -> bool { true }
fn default_diversity_threshold() -> f64 { 0.75 }
fn default_general_data_mix() -> f64 { 0.05 }
fn default_quality_threshold() -> f64 { 0.7 }
fn default_curation_interval() -> String { "6h".to_string() }
fn default_training_backend() -> String { "auto".to_string() }
fn default_base_model() -> String { "auto".to_string() }
fn default_training_method() -> String { "qlora".to_string() }
fn default_training_epochs() -> u32 { 3 }
fn default_training_lr() -> f64 { 0.0002 }
fn default_lora_rank() -> u32 { 16 }
fn default_lora_alpha() -> u32 { 32 }
fn default_teacher_model() -> String { "auto".to_string() }
fn default_show_routing() -> bool { true }
fn default_notify_train() -> bool { true }
fn default_notify_graduate() -> bool { true }
fn default_notify_demote() -> bool { true }
fn default_status_interval() -> String { "weekly".to_string() }
```

---

## Phase 2: Statistical Engines (`src/stats/`)

These are pure mathematical functions. No async, no I/O, no dependencies beyond `f64`. They are the mathematical foundation of Eigen-Tune and will never need to change.

### 2.1 SPRT (`src/stats/sprt.rs`)

```rust
/// Sequential Probability Ratio Test (Wald, 1945)
pub struct Sprt {
    p0: f64,           // H0 accuracy (model is bad)
    p1: f64,           // H1 accuracy (model is good)
    log_a: f64,        // upper boundary (accept H1)
    log_b: f64,        // lower boundary (accept H0)
    max_n: u32,        // truncation safety
    lambda: f64,       // current log-likelihood ratio
    n: u32,            // observations so far
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SprtDecision {
    AcceptH1,  // Graduate
    AcceptH0,  // Demote
    Continue,  // Need more data
}

impl Sprt {
    pub fn new(p0: f64, p1: f64, alpha: f64, beta: f64, max_n: u32) -> Self;
    pub fn observe(&mut self, agree: bool) -> SprtDecision;
    pub fn decision(&self) -> SprtDecision;
    pub fn lambda(&self) -> f64;
    pub fn n(&self) -> u32;
    pub fn reset(&mut self);

    // Restore from persisted state
    pub fn from_state(p0: f64, p1: f64, alpha: f64, beta: f64, max_n: u32,
                      lambda: f64, n: u32) -> Self;
}
```

**Tests (8):**
1. New SPRT starts at lambda=0, n=0
2. All agreements → eventually AcceptH1
3. All disagreements → eventually AcceptH0
4. Mixed observations → Continue
5. Truncation at max_n → AcceptH0
6. Single disagreement offsets ~19 agreements
7. State restore produces same decisions as fresh
8. Boundary values: exact boundary → correct decision

### 2.2 CUSUM (`src/stats/cusum.rs`)

```rust
/// Cumulative Sum control chart (Page, 1954)
pub struct Cusum {
    target: f64,       // μ₀ (expected accuracy)
    slack: f64,        // k (allowance)
    threshold: f64,    // h (decision interval)
    s: f64,            // current CUSUM statistic
    n: u32,            // observations
    fir: bool,         // Fast Initial Response
}

impl Cusum {
    pub fn new(target: f64, slack: f64, threshold: f64, fir: bool) -> Self;
    pub fn observe(&mut self, value: f64) -> bool;  // returns true on alarm
    pub fn statistic(&self) -> f64;
    pub fn n(&self) -> u32;
    pub fn reset(&mut self);
    pub fn from_state(target: f64, slack: f64, threshold: f64,
                      fir: bool, s: f64, n: u32) -> Self;
}
```

**Tests (7):**
1. In-control observations → no alarm for many samples
2. Sustained drift → alarm within expected ARL
3. Single outlier → no alarm (slack absorbs it)
4. FIR: starts at h/2 instead of 0
5. Reset clears state
6. State restore produces same behavior
7. Exact threshold boundary → alarm

### 2.3 Wilson Score (`src/stats/wilson.rs`)

```rust
/// Wilson score confidence interval (Wilson, 1927)
pub fn wilson_interval(successes: u64, total: u64, confidence: f64) -> (f64, f64);
// Returns (lower_bound, upper_bound)

pub fn wilson_lower(successes: u64, total: u64, confidence: f64) -> f64;
// Convenience: just the lower bound

/// Z-value for confidence level
pub fn z_value(confidence: f64) -> f64;
// 0.95 → 1.960, 0.99 → 2.576
```

**Tests (6):**
1. 100/100 → lower bound near 1.0
2. 0/100 → lower bound near 0.0
3. 95/100 at 99% CI → lower bound < 0.95
4. 950/1000 at 99% CI → lower bound ≈ 0.936
5. Small sample (5/5) → wide interval (Wilson handles this)
6. z_value(0.99) ≈ 2.576

### 2.4 Shannon Entropy (`src/stats/entropy.rs`)

```rust
/// Shannon entropy of a distribution
pub fn shannon_entropy(counts: &[u64]) -> f64;
// H = -Σ pᵢ ln(pᵢ)

/// Normalized entropy (evenness index)
pub fn normalized_entropy(counts: &[u64]) -> f64;
// J = H / H_max = H / ln(K), range [0, 1]
```

**Tests (5):**
1. Uniform distribution → J = 1.0
2. All in one category → J = 0.0
3. Two equal categories → J = 1.0
4. Skewed distribution → 0 < J < 1
5. Empty/zero counts → 0.0

### 2.5 Thompson Sampling (`src/stats/thompson.rs`)

```rust
/// Thompson sampling for category balancing
pub struct ThompsonSampler {
    arms: Vec<(f64, f64)>,  // (alpha, beta) per category
}

impl ThompsonSampler {
    pub fn new(k: usize) -> Self;  // k categories, uniform prior
    pub fn sample(&self) -> usize; // returns index of selected arm
    pub fn update(&mut self, arm: usize, reward: bool);
    pub fn expected_values(&self) -> Vec<f64>;  // mean of each arm
}
```

**Tests (4):**
1. New sampler with uniform priors → roughly uniform selection
2. Strongly rewarded arm → selected more often
3. Strongly penalized arm → selected less often
4. Update changes expected values correctly

### 2.6 Beta Distribution (`src/stats/beta.rs`)

```rust
/// Beta distribution utilities for quality scoring
pub fn beta_mean(alpha: f64, beta: f64) -> f64;
// α / (α + β)

pub fn beta_variance(alpha: f64, beta: f64) -> f64;
// αβ / ((α+β)²(α+β+1))

/// Update Beta parameters with a quality signal
pub fn beta_update(alpha: f64, beta: f64, signal_weight: f64, positive: bool) -> (f64, f64);
// positive: (α + w, β)
// negative: (α, β + w)
```

**Tests (4):**
1. Mean of Beta(2,2) = 0.5
2. Mean of Beta(10,2) ≈ 0.833
3. Positive signal increases mean
4. Negative signal decreases mean

### 2.7 Power Analysis (`src/stats/power.rs`)

```rust
/// Minimum sample size for binomial test
pub fn min_sample_size(p0: f64, p1: f64, alpha: f64, power: f64) -> u64;
// n = p(1-p) × ((z_α + z_β) / δ)²
```

**Tests (3):**
1. p0=0.90, p1=0.95, alpha=0.01, power=0.95 → ~568
2. Smaller effect size → larger n
3. Lower confidence → smaller n

---

## Phase 3: Storage Layer (`src/store.rs`)

### 3.1 Public API

```rust
pub struct EigenTuneStore {
    pool: SqlitePool,
}

impl EigenTuneStore {
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError>;
    // Creates tables if not exist

    // ─── Pairs ───
    pub async fn save_pair(&self, pair: &TrainingPair) -> Result<(), Temm1eError>;
    pub async fn update_quality(&self, id: &str, alpha: f64, beta: f64,
                                 score: f64) -> Result<(), Temm1eError>;
    pub async fn update_signal(&self, id: &str, signal: &str,
                                value: bool) -> Result<(), Temm1eError>;
    pub async fn get_pairs_for_tier(&self, tier: &str, min_quality: f64)
        -> Result<Vec<TrainingPair>, Temm1eError>;
    pub async fn get_recent_pair(&self, conversation_id: &str)
        -> Result<Option<TrainingPair>, Temm1eError>;
    pub async fn count_pairs(&self, tier: &str) -> Result<i64, Temm1eError>;
    pub async fn count_high_quality_pairs(&self, tier: &str, threshold: f64)
        -> Result<i64, Temm1eError>;
    pub async fn get_category_counts(&self, tier: &str) -> Result<Vec<(String, u64)>, Temm1eError>;

    // ─── Runs ───
    pub async fn save_run(&self, run: &TrainingRun) -> Result<(), Temm1eError>;
    pub async fn update_run(&self, run: &TrainingRun) -> Result<(), Temm1eError>;
    pub async fn get_run(&self, id: &str) -> Result<Option<TrainingRun>, Temm1eError>;
    pub async fn get_latest_run(&self, tier: &str) -> Result<Option<TrainingRun>, Temm1eError>;

    // ─── Tiers ───
    pub async fn get_tier(&self, tier: &str) -> Result<TierRecord, Temm1eError>;
    pub async fn update_tier(&self, record: &TierRecord) -> Result<(), Temm1eError>;
    pub async fn get_all_tiers(&self) -> Result<Vec<TierRecord>, Temm1eError>;

    // ─── Observations ───
    pub async fn save_observation(&self, obs: &Observation) -> Result<(), Temm1eError>;
    pub async fn count_observations(&self, tier: &str, phase: &str)
        -> Result<i64, Temm1eError>;

    // ─── Status ───
    pub async fn total_pairs(&self) -> Result<i64, Temm1eError>;
    pub async fn total_high_quality(&self, threshold: f64) -> Result<i64, Temm1eError>;
    pub async fn total_savings_usd(&self) -> Result<f64, Temm1eError>;
}
```

**Tests (12):**
1. Create store with in-memory SQLite
2. Save and retrieve pair
3. Update quality scores
4. Update signals (user_continued, etc.)
5. Get pairs for tier with quality filter
6. Count pairs by tier
7. Category counts for entropy calculation
8. Save and retrieve training run
9. Save and retrieve tier state
10. Save and retrieve observation
11. Get recent pair by conversation_id
12. Total savings calculation

---

## Phase 4: Collector (`src/collector.rs`)

### 4.1 Public API

```rust
use temm1e_core::types::message::{CompletionRequest, CompletionResponse};

pub struct EigenTuneCollector {
    store: Arc<EigenTuneStore>,
    enabled: bool,
}

impl EigenTuneCollector {
    pub fn new(store: Arc<EigenTuneStore>, enabled: bool) -> Self;

    /// Called after every Provider.complete() — fire-and-forget
    pub async fn collect(
        &self,
        request: &CompletionRequest,
        response: &CompletionResponse,
        complexity: &str,           // from classifier: "simple" | "standard" | "complex"
        conversation_id: &str,
        turn: i32,
        source_model: &str,
        source_provider: &str,
    ) -> Result<String, Temm1eError>;  // returns pair ID

    /// Called when a quality signal is observed
    pub async fn observe_signal(
        &self,
        conversation_id: &str,
        signal: QualitySignal,
    ) -> Result<(), Temm1eError>;

    /// Classify domain category from message content
    pub fn classify_domain(request: &CompletionRequest) -> String;
}
```

**Domain classifier (heuristic, no LLM):**
```rust
fn classify_domain(request: &CompletionRequest) -> String {
    let text = extract_user_text(request).to_lowercase();

    if contains_code_markers(&text) { return "coding".into(); }
    if has_tool_calls(request)       { return "tool-use".into(); }
    if is_reasoning_query(&text)     { return "reasoning".into(); }
    if is_creative_prompt(&text)     { return "creative".into(); }
    if is_factual_query(&text)       { return "factual".into(); }
    if is_analysis_query(&text)      { return "analysis".into(); }
    if is_meta_query(&text)          { return "meta".into(); }

    "conversation".into()  // default
}
```

**Tests (8):**
1. Collect saves pair to store
2. Pair has correct complexity tier
3. Domain classification: code block → "coding"
4. Domain classification: "explain why" → "reasoning"
5. Domain classification: "write a poem" → "creative"
6. Domain classification: greeting → "conversation"
7. Observe signal updates quality score
8. Negative signal decreases quality score

---

## Phase 5: Scorer (`src/scorer.rs`)

### 5.1 Public API

```rust
pub struct EigenTuneScorer {
    store: Arc<EigenTuneStore>,
}

impl EigenTuneScorer {
    pub fn new(store: Arc<EigenTuneStore>) -> Self;

    /// Score a pair given observed signals
    pub fn compute_score(alpha: f64, beta: f64) -> f64;
    // α / (α + β)

    /// Apply a signal to a pair's quality
    pub async fn apply_signal(
        &self,
        pair_id: &str,
        signal: QualitySignal,
    ) -> Result<f64, Temm1eError>;  // returns new score

    /// Batch score all unscored pairs
    pub async fn score_pending(&self) -> Result<u32, Temm1eError>;
    // returns number scored
}
```

**Tests (5):**
1. Initial score (Beta(2,2)) = 0.5
2. Positive signals increase score
3. Negative signals decrease score
4. Multiple signals compound correctly
5. Score never exceeds 1.0 or drops below 0.0

---

## Phase 6: Engine Layer (`src/engine/`)

### 6.1 State Machine (`src/engine/state_machine.rs`)

```rust
pub struct EigenTuneStateMachine {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl EigenTuneStateMachine {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self;

    /// Check if a tier should transition
    pub async fn check_transitions(&self, tier: EigenTier) -> Result<Option<TierState>, Temm1eError>;

    /// Execute a state transition
    pub async fn transition(
        &self,
        tier: EigenTier,
        from: TierState,
        to: TierState,
    ) -> Result<(), Temm1eError>;

    /// Get current state for a tier
    pub async fn state(&self, tier: EigenTier) -> Result<TierState, Temm1eError>;
}
```

**Tests (8):**
1. Initial state is Collecting for all tiers
2. Collecting → Training when pair_count >= 500 AND entropy >= 0.75
3. Training → Evaluating on success
4. Training → Collecting on failure
5. Evaluating → Shadowing when Wilson lower bound >= τ
6. Evaluating → Collecting when Wilson lower bound < τ
7. Shadowing → Graduated when SPRT accepts H1
8. Graduated → Collecting when CUSUM alarms

### 6.2 Router (`src/engine/router.rs`)

```rust
pub struct EigenTuneRouter {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl EigenTuneRouter {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self;

    /// Decide whether to route to local or cloud
    pub async fn route(
        &self,
        complexity: &str,
    ) -> Result<RouteDecision, Temm1eError>;

    /// Get the local model endpoint for a graduated tier
    pub async fn local_endpoint(
        &self,
        tier: EigenTier,
    ) -> Result<Option<ModelEndpoint>, Temm1eError>;
}

#[derive(Debug, Clone)]
pub enum RouteDecision {
    Cloud,                        // use cloud provider (default)
    Local(ModelEndpoint),         // use local model
    Shadow(ModelEndpoint),        // send to both, show cloud, compare
    Monitor(ModelEndpoint),       // send to both (5% sample), show local
}
```

**Tests (6):**
1. Non-graduated tier → Cloud
2. Graduated tier → Local with endpoint
3. Shadowing tier → Shadow
4. Graduated tier with monitor sample → Monitor (5% rate)
5. Graduated tier normal query → Local (95% rate)
6. Missing local endpoint → fallback to Cloud

### 6.3 Shadow (`src/engine/shadow.rs`)

```rust
pub struct ShadowCoordinator {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
    judge: Arc<dyn ResponseJudge>,
}

impl ShadowCoordinator {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig,
               judge: Arc<dyn ResponseJudge>) -> Self;

    /// Process a shadow comparison
    pub async fn compare(
        &self,
        tier: EigenTier,
        request: &CompletionRequest,
        local_response: &CompletionResponse,
        cloud_response: &CompletionResponse,
    ) -> Result<SprtDecision, Temm1eError>;
}
```

### 6.4 Monitor (`src/engine/monitor.rs`)

```rust
pub struct ProductionMonitor {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
    judge: Arc<dyn ResponseJudge>,
}

impl ProductionMonitor {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig,
               judge: Arc<dyn ResponseJudge>) -> Self;

    /// Should we monitor this query? (5% sample)
    pub fn should_monitor(&self) -> bool;

    /// Process a monitor comparison
    pub async fn compare(
        &self,
        tier: EigenTier,
        request: &CompletionRequest,
        local_response: &CompletionResponse,
        cloud_response: &CompletionResponse,
    ) -> Result<bool, Temm1eError>;  // returns true on CUSUM alarm
}
```

---

## Phase 7: Backends (`src/backends/`)

### 7.1 Backend Detection (`src/backends/mod.rs`)

```rust
pub async fn detect_backend(config: &EigenTuneConfig) -> Option<Box<dyn TrainingBackend>>;
// Tries each backend in order, returns first available

pub async fn detect_server() -> Option<Box<dyn ModelServer>>;
// Checks for Ollama, returns if available
```

### 7.2 Ollama Backend (`src/backends/ollama.rs`)

```rust
pub struct OllamaBackend;

#[async_trait]
impl TrainingBackend for OllamaBackend {
    fn name(&self) -> &str { "ollama" }
    async fn is_available(&self) -> Result<bool, Temm1eError>;
    // Check: curl http://localhost:11434/api/tags
    async fn detect_base_models(&self) -> Result<Vec<BaseModelInfo>, Temm1eError>;
    // List available models via Ollama API
    async fn train(&self, config: TrainJobConfig) -> Result<TrainResult, Temm1eError>;
    // Create Modelfile → ollama create
}

pub struct OllamaServer;

#[async_trait]
impl ModelServer for OllamaServer {
    fn name(&self) -> &str { "ollama" }
    async fn is_available(&self) -> Result<bool, Temm1eError>;
    async fn deploy(&self, model_path: &str, name: &str) -> Result<ModelEndpoint, Temm1eError>;
    async fn undeploy(&self, name: &str) -> Result<(), Temm1eError>;
    async fn health_check(&self, name: &str) -> Result<bool, Temm1eError>;
}
```

**Tests (4):**
1. Ollama not running → is_available returns false
2. ModelEndpoint has correct base_url format
3. Deploy creates model with correct name
4. Health check returns false for non-existent model

---

## Phase 8: Judge (`src/judge/`)

### 8.0 Architecture

Three-judge system with zero-cost defaults:

- **Default (eval phase):** `EmbeddingJudge` — cosine similarity via local Ollama embeddings. $0 cost.
- **Default (shadow/monitor phase):** `BehaviorJudge` — user behavior signals (continued, retried, abandoned) mapped to agree/disagree. $0 cost.
- **Optional (all phases):** `TeacherJudge` — LLM-as-judge with position debiasing. Requires `teacher_enabled = true` in `[eigentune]` config. Costs LLM API money but provides higher confidence.

Default pipeline: EmbeddingJudge (eval) + BehaviorJudge (shadow/monitor) = $0 cost.
Optional pipeline: TeacherJudge (all phases) = LLM API cost, higher confidence.

### 8.1 Embedding Judge (`src/judge/embedding.rs`)

```rust
pub struct EmbeddingJudge {
    ollama_url: String,        // default: "http://localhost:11434"
    model: String,             // default: "nomic-embed-text"
    threshold: f64,            // cosine similarity threshold, default: 0.92
}

impl EmbeddingJudge {
    pub fn new(ollama_url: &str, model: &str, threshold: f64) -> Self;

    /// Get embedding vector from Ollama
    async fn embed(&self, text: &str) -> Result<Vec<f64>, Temm1eError>;

    /// Cosine similarity between two vectors
    fn cosine_similarity(a: &[f64], b: &[f64]) -> f64;
}

#[async_trait]
impl ResponseJudge for EmbeddingJudge {
    fn name(&self) -> &str { "embedding-judge" }

    async fn judge(
        &self,
        input: &CompletionRequest,
        response_a: &CompletionResponse,
        response_b: &CompletionResponse,
    ) -> Result<JudgeVerdict, Temm1eError>;
    // Embeds both responses, computes cosine similarity
    // agree = similarity >= threshold
}
```

**Tests (4):**
1. Identical text → similarity = 1.0 → agree
2. Completely unrelated text → similarity < threshold → disagree
3. Threshold boundary: similarity exactly at threshold → agree
4. Empty response handling → error (not false positive)

### 8.2 Behavior Judge (`src/judge/behavior.rs`)

```rust
pub struct BehaviorJudge;

impl BehaviorJudge {
    pub fn new() -> Self;

    /// Map user behavior signals to agree/disagree
    fn signal_to_verdict(signals: &[QualitySignal]) -> JudgeVerdict;
}

#[async_trait]
impl ResponseJudge for BehaviorJudge {
    fn name(&self) -> &str { "behavior-judge" }

    async fn judge(
        &self,
        input: &CompletionRequest,
        response_a: &CompletionResponse,
        response_b: &CompletionResponse,
    ) -> Result<JudgeVerdict, Temm1eError>;
    // Used in shadow/monitor phase: checks if user continued conversation
    // (local response shown), retried (disagreement), or abandoned (weak signal)
}
```

**Tests (4):**
1. UserContinued signal → agree
2. UserRetried signal → disagree
3. Mixed signals → weighted verdict based on signal weights
4. No signals (abandoned) → disagree with low confidence

### 8.3 Teacher Judge (`src/judge/teacher.rs`) — optional, `teacher_enabled = true`

```rust
pub struct TeacherJudge {
    provider: Arc<dyn Provider>,
    model: String,
}

impl TeacherJudge {
    pub fn new(provider: Arc<dyn Provider>, model: String) -> Self;
}

#[async_trait]
impl ResponseJudge for TeacherJudge {
    fn name(&self) -> &str { "teacher-judge" }

    async fn judge(
        &self,
        input: &CompletionRequest,
        response_a: &CompletionResponse,
        response_b: &CompletionResponse,
    ) -> Result<JudgeVerdict, Temm1eError>;
    // Sends two comparisons (A,B) and (B,A) for position debiasing
    // Only returns agree=true if BOTH orderings agree
}
```

**Teacher judge prompt:**
```
You are evaluating whether two AI responses are functionally equivalent.

USER QUERY:
{input}

RESPONSE A:
{response_a}

RESPONSE B:
{response_b}

Are these responses functionally equivalent? They don't need to be word-for-word identical,
but should convey the same information, have the same quality, and be equally helpful.

Respond with JSON:
{"equivalent": true/false, "reasoning": "brief explanation", "confidence": 0.0-1.0}
```

**Tests (4) — only compiled when `teacher_enabled` feature is active:**
1. Position debiasing: both orderings agree → agree
2. Position debiasing: inconsistent orderings → disagree (conservative)
3. Structured output parsing: valid JSON → correct verdict
4. Malformed judge response → error (not false positive)

---

## Phase 9: Public API (`src/lib.rs`)

### 9.1 EigenTuneEngine

```rust
pub struct EigenTuneEngine {
    store: Arc<EigenTuneStore>,
    collector: EigenTuneCollector,
    scorer: EigenTuneScorer,
    state_machine: EigenTuneStateMachine,
    router: EigenTuneRouter,
    config: EigenTuneConfig,
}

impl EigenTuneEngine {
    pub async fn new(config: &EigenTuneConfig, database_url: &str) -> Result<Self, Temm1eError>;

    /// The collection hook — called after every Provider.complete()
    pub async fn on_completion(
        &self,
        request: &CompletionRequest,
        response: &CompletionResponse,
        complexity: &str,
        conversation_id: &str,
        turn: i32,
        source_model: &str,
        source_provider: &str,
    );

    /// The signal hook — called when user behavior is observed
    pub async fn on_signal(
        &self,
        conversation_id: &str,
        signal: QualitySignal,
    );

    /// The routing hook — called before Provider.complete()
    pub async fn route(&self, complexity: &str) -> RouteDecision;

    /// Process a shadow/monitor comparison
    pub async fn on_shadow_result(
        &self,
        tier: EigenTier,
        request: &CompletionRequest,
        local_response: &CompletionResponse,
        cloud_response: &CompletionResponse,
    ) -> Result<(), Temm1eError>;

    /// Get full status report
    pub async fn status(&self) -> Result<EigenTuneStatus, Temm1eError>;

    /// Run curation cycle (called by cron)
    pub async fn curate(&self) -> Result<(), Temm1eError>;

    /// Run training for a tier (called when conditions met)
    pub async fn train(&self, tier: EigenTier) -> Result<(), Temm1eError>;

    /// Check and execute state transitions for all tiers
    pub async fn tick(&self) -> Result<(), Temm1eError>;
}
```

---

## Phase 10: Integration

### 10.1 Config Addition (`temm1e-core/types/config.rs`)

```rust
#[serde(default)]
pub eigentune: EigenTuneConfig,
```

### 10.2 Agent Runtime Hook (`crates/temm1e-agent/src/runtime.rs`)

At line ~885, after `Provider.complete()` returns:

```rust
// POST-RESPONSE HOOK: Eigen-Tune collection
#[cfg(feature = "eigentune")]
if let Some(eigentune_engine) = &self.eigentune_engine {
    let req = request.clone();
    let resp = response.clone();
    let complexity = classification.difficulty.as_str().to_string();
    let conv_id = conversation_id.clone();
    let turn = turn_count;
    let model = self.model.clone();
    let provider_name = self.provider.name().to_string();
    let engine = eigentune_engine.clone();
    tokio::spawn(async move {
        engine.on_completion(&req, &resp, &complexity, &conv_id,
                            turn, &model, &provider_name).await;
    });
}
```

Before the `Provider.complete()` call:

```rust
// PRE-REQUEST HOOK: Eigen-Tune routing
#[cfg(feature = "eigentune")]
let (provider, model) = if let Some(eigentune_engine) = &self.eigentune_engine {
    match eigentune_engine.route(&classification.difficulty.as_str()).await {
        RouteDecision::Local(endpoint) => {
            // Create OpenAI-compat provider pointing to local endpoint
            (create_local_provider(&endpoint)?, endpoint.model_name.clone())
        }
        RouteDecision::Shadow(endpoint) => {
            // Will send to both after cloud response
            self.eigentune_shadow_endpoint = Some(endpoint);
            (self.provider.clone(), self.model.clone())
        }
        _ => (self.provider.clone(), self.model.clone()),
    }
} else {
    (self.provider.clone(), self.model.clone())
};
```

### 10.3 Slash Command (`/eigentune`)

Register in the command handler:

```rust
"/eigentune" | "/eigentune status" => {
    let status = eigentune_engine.status().await?;
    format_eigentune_status(&status)
}
"/eigentune on" => {
    // Update config, enable eigentune
    "Eigen-Tune enabled. Collecting training data from your conversations."
}
"/eigentune off" => {
    // Update config, disable eigentune (keeps data)
    "Eigen-Tune paused. Data preserved. Cloud-only mode."
}
```

---

## Phase 11: Tests

### 11.1 Unit Test Count Target

| Module | Tests |
|--------|-------|
| types.rs | 5 (serde roundtrip, tier/state conversions) |
| stats/sprt.rs | 8 (convergence, boundaries, truncation, state restore) |
| stats/cusum.rs | 7 (alarm, no-alarm, drift detection, FIR, reset) |
| stats/wilson.rs | 6 (intervals, edge cases, z-values) |
| stats/entropy.rs | 5 (uniform, skewed, empty, normalized) |
| stats/thompson.rs | 4 (sampling, updating, convergence) |
| stats/beta.rs | 4 (mean, variance, updates) |
| stats/power.rs | 3 (sample sizes, parameter sensitivity) |
| store.rs | 12 (CRUD for all tables) |
| collector.rs | 8 (collection, signals, domain classification) |
| scorer.rs | 5 (scoring, signal application) |
| engine/state_machine.rs | 8 (all state transitions) |
| engine/router.rs | 6 (routing decisions) |
| judge/embedding.rs | 4 (similarity computation, threshold, edge cases) |
| judge/behavior.rs | 4 (signal detection, agree/disagree mapping) |
| judge/teacher.rs | 4 (position debiasing, structured output) — only compiled when teacher feature enabled |
| backends/ollama.rs | 4 (availability, deploy, health) |
| lib.rs | 4 (initialization, public API integration) |
| **Total** | **101** |

### 11.2 Compilation Gate

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

ALL must pass. No exceptions.

---

## Phase 12: Benchmark Plan

### 12.1 Benchmark Design

The benchmark validates the full pipeline from collection through graduation.

**File:** `tems_lab/eigen/bench_eigentune.py`

**Stages:**
1. **Collection benchmark:** Measure overhead of collector hook (should be < 1ms)
2. **Scoring benchmark:** Score 1000 pairs with various signal patterns
3. **Curation benchmark:** Curate dataset from 5000 pairs, verify entropy gate
4. **Statistical tests benchmark:** Run SPRT/CUSUM/Wilson with synthetic data, verify decisions match expected outcomes
5. **End-to-end simulation:** Simulate full lifecycle with synthetic conversations

### 12.2 Metrics

```rust
struct EigenTuneBenchResult {
    collection_overhead_us: u64,  // microseconds per pair
    scoring_throughput: f64,      // pairs per second
    curation_time_ms: u64,       // for 5000 pairs
    sprt_decisions_correct: u32,  // out of 100 synthetic scenarios
    cusum_detections_correct: u32,
    wilson_intervals_cover: u32,  // coverage test
}
```

---

## Implementation Order

1. ✅ Design doc (DESIGN.md)
2. ✅ Implementation plan (this document)
3. 🔲 Create crate scaffold (Cargo.toml, lib.rs, mod declarations)
4. 🔲 Implement types.rs
5. 🔲 Implement config.rs
6. 🔲 Implement stats/ (all 7 modules — pure math, no deps)
7. 🔲 Implement store.rs + tests
8. 🔲 Implement collector.rs + tests
9. 🔲 Implement scorer.rs + tests
10. 🔲 Implement engine/state_machine.rs + tests
11. 🔲 Implement engine/router.rs + tests
12. 🔲 Implement judge/embedding.rs + judge/behavior.rs + judge/teacher.rs + tests
13. 🔲 Implement backends/ollama.rs + tests
14. 🔲 Implement engine/shadow.rs + engine/monitor.rs
15. 🔲 Implement engine/trainer.rs + engine/evaluator.rs + engine/graduation.rs
16. 🔲 Implement lib.rs (EigenTuneEngine) + tests
17. 🔲 Compilation gate (check + clippy + fmt + test)
18. 🔲 Integration: config.rs addition
19. 🔲 Integration: runtime.rs hooks
20. 🔲 Integration: /eigentune slash command
21. 🔲 Full compilation gate
22. 🔲 Status display formatting
23. 🔲 Benchmark scripts
24. 🔲 Run benchmarks
25. 🔲 Final report
