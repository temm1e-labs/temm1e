//! Eigen-Tune configuration.
//!
//! All fields have serde defaults so that an empty `[eigentune]` section
//! in TOML is valid and produces a sensible default configuration.

use serde::{Deserialize, Serialize};

/// Complete Eigen-Tune configuration.
///
/// Loaded from the `[eigentune]` section in `temm1e.toml`.
/// Every field has a default so an empty section is valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EigenTuneConfig {
    /// Master enable switch. When false, no capture or training happens.
    #[serde(default = "default_false")]
    pub enabled: bool,

    // ── Capture thresholds ──────────────────────────────────────────
    /// Minimum training pairs before first training run (per tier).
    #[serde(default = "default_min_pairs")]
    pub min_pairs: i32,

    /// Fraction of pairs reserved for evaluation holdout.
    #[serde(default = "default_eval_holdout_pct")]
    pub eval_holdout_pct: f64,

    /// Minimum quality score (alpha / (alpha + beta)) for a pair to be
    /// included in training datasets.
    #[serde(default = "default_quality_threshold")]
    pub quality_threshold: f64,

    /// Target normalized entropy for category diversity (0.0 = monoculture,
    /// 1.0 = perfectly uniform).
    #[serde(default = "default_diversity_target")]
    pub diversity_target: f64,

    // ── Training backend ────────────────────────────────────────────
    /// Training backend: "unsloth", "axolotl", or "llamacpp".
    #[serde(default = "default_backend")]
    pub backend: String,

    /// Fine-tuning method: "qlora", "lora", or "full".
    #[serde(default = "default_method")]
    pub method: String,

    /// Base model for fine-tuning (e.g. "unsloth/Qwen2.5-7B-Instruct-bnb-4bit").
    #[serde(default = "default_base_model")]
    pub base_model: String,

    /// Number of training epochs.
    #[serde(default = "default_epochs")]
    pub epochs: i32,

    /// Learning rate.
    #[serde(default = "default_learning_rate")]
    pub learning_rate: f64,

    /// LoRA rank (r parameter).
    #[serde(default = "default_lora_r")]
    pub lora_r: i32,

    /// LoRA alpha scaling factor.
    #[serde(default = "default_lora_alpha")]
    pub lora_alpha: i32,

    /// Percentage of general-purpose data mixed into training.
    /// Prevents catastrophic forgetting.
    #[serde(default = "default_general_mix_pct")]
    pub general_mix_pct: f64,

    /// Maximum sequence length for training.
    #[serde(default = "default_max_seq_length")]
    pub max_seq_length: i32,

    /// Batch size per device for training.
    #[serde(default = "default_batch_size")]
    pub batch_size: i32,

    /// Gradient accumulation steps.
    #[serde(default = "default_gradient_accumulation")]
    pub gradient_accumulation_steps: i32,

    // ── Evaluation / graduation ─────────────────────────────────────
    /// Accuracy threshold for SPRT graduation decision.
    #[serde(default = "default_graduation_accuracy")]
    pub graduation_accuracy: f64,

    /// SPRT alpha (Type I error rate — false graduation).
    #[serde(default = "default_sprt_alpha")]
    pub sprt_alpha: f64,

    /// SPRT beta (Type II error rate — missed graduation).
    #[serde(default = "default_sprt_beta")]
    pub sprt_beta: f64,

    /// Null hypothesis accuracy for SPRT (below this = reject).
    #[serde(default = "default_sprt_p0")]
    pub sprt_p0: f64,

    /// Alternative hypothesis accuracy for SPRT (above this = accept).
    #[serde(default = "default_sprt_p1")]
    pub sprt_p1: f64,

    /// Maximum SPRT sample count before forced decision (truncation).
    #[serde(default = "default_sprt_max_samples")]
    pub sprt_max_samples: i32,

    /// Number of shadow observations before SPRT decision.
    #[serde(default = "default_shadow_min_n")]
    pub shadow_min_n: i32,

    /// Confidence level for Wilson score intervals (e.g. 0.99 for 99%).
    #[serde(default = "default_graduation_confidence")]
    pub graduation_confidence: f64,

    /// Minimum number of eval samples before graduation decision.
    #[serde(default = "default_min_eval_samples")]
    pub min_eval_samples: i32,

    /// Alias for `min_pairs` used by the state machine (same semantics).
    #[serde(default = "default_min_pairs")]
    pub min_training_pairs: i32,

    /// Alias for `diversity_target` used by the state machine (same semantics).
    #[serde(default = "default_diversity_target")]
    pub diversity_threshold: f64,

    // ── CUSUM drift detection ───────────────────────────────────────
    /// CUSUM threshold for drift detection during monitoring.
    #[serde(default = "default_cusum_threshold")]
    pub cusum_threshold: f64,

    /// CUSUM target (the expected mean under normal operation).
    #[serde(default = "default_cusum_target")]
    pub cusum_target: f64,

    /// CUSUM slack parameter (k). Determines sensitivity to small shifts.
    #[serde(default = "default_cusum_k")]
    pub cusum_slack: f64,

    /// CUSUM allowance (same as slack, kept for backwards compat).
    #[serde(default = "default_cusum_k")]
    pub cusum_k: f64,

    /// Enable CUSUM Fast Initial Response (start at threshold/2).
    #[serde(default = "default_false")]
    pub cusum_fir: bool,

    /// Sampling rate for monitor-phase cloud comparisons (0.0 to 1.0).
    #[serde(default = "default_monitor_sample_rate")]
    pub monitor_sample_rate: f64,

    // ── Serving ─────────────────────────────────────────────────────
    /// Ollama base URL for local model serving.
    #[serde(default = "default_ollama_url")]
    pub ollama_url: String,

    /// Whether to auto-convert fine-tuned models to GGUF.
    #[serde(default = "default_true")]
    pub auto_gguf: bool,

    /// GGUF quantization level (e.g. "Q4_K_M", "Q5_K_S").
    #[serde(default = "default_gguf_quant")]
    pub gguf_quant: String,

    // ── Teacher model (distillation source) ─────────────────────────
    /// Enable teacher-mode capture: use a stronger model for training
    /// data instead of the model that normally handles the request.
    #[serde(default = "default_false")]
    pub teacher_enabled: bool,

    /// Teacher model name (e.g. "claude-sonnet-4-20250514").
    /// Only used when `teacher_enabled = true`.
    #[serde(default = "default_teacher_model")]
    pub teacher_model: String,

    // ── Scheduling ──────────────────────────────────────────────────
    /// Cron expression for periodic training check.
    #[serde(default = "default_train_schedule")]
    pub train_schedule: String,

    /// Directory for storing training artifacts (datasets, checkpoints).
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: String,
}

// ── Default value functions ─────────────────────────────────────────

fn default_false() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_min_pairs() -> i32 {
    200
}
fn default_eval_holdout_pct() -> f64 {
    0.15
}
fn default_quality_threshold() -> f64 {
    0.6
}
fn default_diversity_target() -> f64 {
    0.7
}
fn default_backend() -> String {
    "unsloth".to_string()
}
fn default_method() -> String {
    "qlora".to_string()
}
fn default_base_model() -> String {
    "auto".to_string()
}
fn default_epochs() -> i32 {
    3
}
fn default_learning_rate() -> f64 {
    2e-4
}
fn default_lora_r() -> i32 {
    32
}
fn default_lora_alpha() -> i32 {
    64
}
fn default_general_mix_pct() -> f64 {
    0.1
}
fn default_max_seq_length() -> i32 {
    4096
}
fn default_batch_size() -> i32 {
    4
}
fn default_gradient_accumulation() -> i32 {
    4
}
fn default_graduation_accuracy() -> f64 {
    0.95
}
fn default_sprt_alpha() -> f64 {
    0.05
}
fn default_sprt_beta() -> f64 {
    0.10
}
fn default_sprt_p0() -> f64 {
    0.85
}
fn default_sprt_p1() -> f64 {
    0.95
}
fn default_sprt_max_samples() -> i32 {
    200
}
fn default_shadow_min_n() -> i32 {
    50
}
fn default_graduation_confidence() -> f64 {
    0.99
}
fn default_min_eval_samples() -> i32 {
    30
}
fn default_cusum_threshold() -> f64 {
    5.0
}
fn default_cusum_target() -> f64 {
    0.0
}
fn default_cusum_k() -> f64 {
    0.5
}
fn default_monitor_sample_rate() -> f64 {
    0.05
}
fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_gguf_quant() -> String {
    "Q4_K_M".to_string()
}
fn default_teacher_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}
fn default_train_schedule() -> String {
    "0 3 * * *".to_string()
}
fn default_artifacts_dir() -> String {
    "~/.temm1e/eigentune".to_string()
}

impl Default for EigenTuneConfig {
    fn default() -> Self {
        Self {
            enabled: default_false(),
            min_pairs: default_min_pairs(),
            eval_holdout_pct: default_eval_holdout_pct(),
            quality_threshold: default_quality_threshold(),
            diversity_target: default_diversity_target(),
            backend: default_backend(),
            method: default_method(),
            base_model: default_base_model(),
            epochs: default_epochs(),
            learning_rate: default_learning_rate(),
            lora_r: default_lora_r(),
            lora_alpha: default_lora_alpha(),
            general_mix_pct: default_general_mix_pct(),
            max_seq_length: default_max_seq_length(),
            batch_size: default_batch_size(),
            gradient_accumulation_steps: default_gradient_accumulation(),
            graduation_accuracy: default_graduation_accuracy(),
            sprt_alpha: default_sprt_alpha(),
            sprt_beta: default_sprt_beta(),
            sprt_p0: default_sprt_p0(),
            sprt_p1: default_sprt_p1(),
            sprt_max_samples: default_sprt_max_samples(),
            shadow_min_n: default_shadow_min_n(),
            graduation_confidence: default_graduation_confidence(),
            min_eval_samples: default_min_eval_samples(),
            min_training_pairs: default_min_pairs(),
            diversity_threshold: default_diversity_target(),
            cusum_threshold: default_cusum_threshold(),
            cusum_target: default_cusum_target(),
            cusum_slack: default_cusum_k(),
            cusum_k: default_cusum_k(),
            cusum_fir: default_false(),
            monitor_sample_rate: default_monitor_sample_rate(),
            ollama_url: default_ollama_url(),
            auto_gguf: default_true(),
            gguf_quant: default_gguf_quant(),
            teacher_enabled: default_false(),
            teacher_model: default_teacher_model(),
            train_schedule: default_train_schedule(),
            artifacts_dir: default_artifacts_dir(),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = EigenTuneConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_pairs, 200);
        assert_eq!(cfg.backend, "unsloth");
        assert_eq!(cfg.method, "qlora");
        assert!(cfg.quality_threshold > 0.0 && cfg.quality_threshold < 1.0);
        assert!(cfg.eval_holdout_pct > 0.0 && cfg.eval_holdout_pct < 1.0);
        assert!(cfg.learning_rate > 0.0);
        assert!(cfg.graduation_accuracy > 0.0 && cfg.graduation_accuracy <= 1.0);
    }

    #[test]
    fn serde_from_empty_toml() {
        let toml_str = "";
        let cfg: EigenTuneConfig = toml::from_str(toml_str).unwrap();
        // All defaults should apply
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_pairs, 200);
        assert_eq!(cfg.backend, "unsloth");
        assert_eq!(cfg.graduation_accuracy, 0.95);
        assert!(!cfg.teacher_enabled);
    }

    #[test]
    fn serde_specific_field_values() {
        let toml_str = r#"
            enabled = true
            min_pairs = 500
            backend = "axolotl"
            method = "lora"
            epochs = 5
            learning_rate = 1e-4
            graduation_accuracy = 0.90
            ollama_url = "http://gpu-box:11434"
            teacher_enabled = true
            teacher_model = "gpt-4o"
        "#;
        let cfg: EigenTuneConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.min_pairs, 500);
        assert_eq!(cfg.backend, "axolotl");
        assert_eq!(cfg.method, "lora");
        assert_eq!(cfg.epochs, 5);
        assert!((cfg.learning_rate - 1e-4).abs() < f64::EPSILON);
        assert!((cfg.graduation_accuracy - 0.90).abs() < f64::EPSILON);
        assert_eq!(cfg.ollama_url, "http://gpu-box:11434");
        assert!(cfg.teacher_enabled);
        assert_eq!(cfg.teacher_model, "gpt-4o");

        // Unspecified fields get defaults
        assert_eq!(cfg.lora_r, 32);
        assert_eq!(cfg.lora_alpha, 64);
        assert_eq!(cfg.gguf_quant, "Q4_K_M");
    }

    #[test]
    fn teacher_enabled_default_false() {
        let cfg = EigenTuneConfig::default();
        assert!(!cfg.teacher_enabled);
        assert_eq!(cfg.teacher_model, "claude-sonnet-4-20250514");

        // Also verify via TOML deserialization
        let cfg2: EigenTuneConfig = toml::from_str("").unwrap();
        assert!(!cfg2.teacher_enabled);
    }

    #[test]
    fn graduation_accuracy_default_095() {
        let cfg = EigenTuneConfig::default();
        assert!((cfg.graduation_accuracy - 0.95).abs() < f64::EPSILON);

        let cfg2: EigenTuneConfig = toml::from_str("").unwrap();
        assert!((cfg2.graduation_accuracy - 0.95).abs() < f64::EPSILON);

        // Can be overridden
        let cfg3: EigenTuneConfig = toml::from_str("graduation_accuracy = 0.90").unwrap();
        assert!((cfg3.graduation_accuracy - 0.90).abs() < f64::EPSILON);
    }
}
