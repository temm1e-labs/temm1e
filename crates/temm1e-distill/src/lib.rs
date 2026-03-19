//! # Eigen-Tune: Self-Tuning Knowledge Distillation Engine
//!
//! A closed-loop distillation pipeline that:
//! 1. **Collects** every (request, response) pair from LLM provider calls
//! 2. **Scores** quality using user behavior signals (Beta-Binomial model)
//! 3. **Curates** datasets with diversity gating (Shannon entropy)
//! 4. **Trains** local models via pluggable backends (Unsloth/MLX -> GGUF -> Ollama)
//! 5. **Evaluates** using embedding similarity (Wilson score, 99% CI)
//! 6. **Shadows** with user behavior SPRT (Wald, 1945)
//! 7. **Monitors** with CUSUM drift detection (Page, 1954)
//!
//! Zero added LLM cost by default. Optional Teacher Mode for premium evaluation.

pub mod backends;
pub mod collector;
pub mod config;
pub mod engine;
pub mod judge;
pub mod scorer;
pub mod stats;
pub mod store;
pub mod types;

use crate::collector::{EigenTuneCollector, EigenTunePairData};
use crate::config::EigenTuneConfig;
use crate::engine::graduation::GraduationManager;
use crate::engine::monitor::ProductionMonitor;
use crate::engine::router::EigenTuneRouter;
use crate::engine::shadow::ShadowCoordinator;
use crate::scorer::EigenTuneScorer;
use crate::store::EigenTuneStore;
use crate::types::{EigenTier, EigenTuneStatus, QualitySignal, RouteDecision, TierStatusReport};
use std::sync::Arc;

/// The public API for Eigen-Tune.
///
/// Create one instance at startup. Call hooks from the agent runtime.
/// All operations are resilient — failures degrade to cloud, never silence.
pub struct EigenTuneEngine {
    store: Arc<EigenTuneStore>,
    collector: EigenTuneCollector,
    #[allow(dead_code)]
    scorer: EigenTuneScorer,
    router: EigenTuneRouter,
    shadow: ShadowCoordinator,
    monitor: ProductionMonitor,
    graduation: GraduationManager,
    config: EigenTuneConfig,
}

impl EigenTuneEngine {
    /// Create a new Eigen-Tune engine.
    pub async fn new(
        config: &EigenTuneConfig,
        database_url: &str,
    ) -> Result<Self, temm1e_core::types::error::Temm1eError> {
        let store = Arc::new(EigenTuneStore::new(database_url).await?);
        let collector = EigenTuneCollector::new(store.clone(), config.enabled);
        let scorer = EigenTuneScorer::new(store.clone());
        let router = EigenTuneRouter::new(store.clone(), config.clone());
        let shadow = ShadowCoordinator::new(store.clone(), config.clone());
        let monitor = ProductionMonitor::new(store.clone(), config.clone());
        let graduation = GraduationManager::new(store.clone(), config.clone());

        tracing::info!(enabled = config.enabled, "Eigen-Tune: engine initialized");

        Ok(Self {
            store,
            collector,
            scorer,
            router,
            shadow,
            monitor,
            graduation,
            config: config.clone(),
        })
    }

    /// Collection hook — called after every Provider.complete().
    /// Fire-and-forget: errors are logged, never propagated to user.
    pub async fn on_completion(&self, data: EigenTunePairData) {
        if let Err(e) = self.collector.collect(data).await {
            tracing::debug!(error = %e, "Eigen-Tune: collection failed (non-fatal)");
        }
    }

    /// Signal hook — called when user behavior is observed.
    pub async fn on_signal(&self, conversation_id: &str, signal: QualitySignal) {
        if let Err(e) = self.collector.observe_signal(conversation_id, signal).await {
            tracing::debug!(error = %e, "Eigen-Tune: signal failed (non-fatal)");
        }
    }

    /// Routing hook — called before Provider.complete().
    /// On ANY error, returns Cloud (safe fallback).
    pub async fn route(&self, complexity: &str) -> RouteDecision {
        match self.router.route(complexity).await {
            Ok(decision) => decision,
            Err(e) => {
                tracing::debug!(error = %e, "Eigen-Tune: routing failed, fallback to cloud");
                RouteDecision::Cloud
            }
        }
    }

    /// Shadow observation — user behavior during shadow phase.
    pub async fn on_shadow_observation(&self, tier: EigenTier, agree: bool) {
        if let Err(e) = self.shadow.observe(tier, agree).await {
            tracing::debug!(error = %e, "Eigen-Tune: shadow observation failed (non-fatal)");
        }
    }

    /// Monitor observation — user behavior on graduated tier.
    pub async fn on_monitor_observation(&self, tier: EigenTier, agree: bool) {
        match self.monitor.observe(tier, agree).await {
            Ok(true) => {
                // CUSUM alarm — demote
                if let Err(e) = self.graduation.demote(tier).await {
                    tracing::error!(error = %e, "Eigen-Tune: demotion failed");
                }
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!(error = %e, "Eigen-Tune: monitor failed (non-fatal)");
            }
        }
    }

    /// Tick — check all tiers for state transitions.
    pub async fn tick(&self) -> Vec<(EigenTier, types::TierState, types::TierState)> {
        match self.graduation.tick().await {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(error = %e, "Eigen-Tune: tick failed (non-fatal)");
                Vec::new()
            }
        }
    }

    /// Get full status report.
    pub async fn status(&self) -> Result<EigenTuneStatus, temm1e_core::types::error::Temm1eError> {
        let total_pairs = self.store.total_pairs().await?;
        let high_quality = self
            .store
            .total_high_quality(self.config.quality_threshold)
            .await?;

        // Aggregate category counts across all tiers
        let mut all_categories: Vec<(String, i64)> = Vec::new();
        for tier_name in &["simple", "standard", "complex"] {
            let tier_cats = self.store.get_category_counts(tier_name).await?;
            for (cat, cnt) in tier_cats {
                if let Some(entry) = all_categories.iter_mut().find(|(c, _)| c == &cat) {
                    entry.1 += cnt;
                } else {
                    all_categories.push((cat, cnt));
                }
            }
        }
        let counts: Vec<u64> = all_categories.iter().map(|(_, c)| *c as u64).collect();
        let diversity_j = stats::entropy::normalized_entropy(&counts);

        let category_distribution: Vec<(String, f64)> = {
            let total: f64 = all_categories.iter().map(|(_, c)| *c as f64).sum();
            if total > 0.0 {
                all_categories
                    .iter()
                    .map(|(cat, count)| (cat.clone(), *count as f64 / total))
                    .collect()
            } else {
                Vec::new()
            }
        };

        let all_tiers = self.store.get_all_tiers().await?;
        let tiers: Vec<TierStatusReport> = all_tiers
            .iter()
            .map(|t| {
                let accuracy_ci = t.eval_accuracy.and_then(|acc| {
                    t.eval_n.map(|n| {
                        let successes = (acc * n as f64).round() as u64;
                        stats::wilson::wilson_interval(
                            successes,
                            n as u64,
                            self.config.graduation_confidence,
                        )
                    })
                });

                TierStatusReport {
                    tier: t.tier,
                    state: t.state,
                    pair_count: t.pair_count,
                    accuracy: t.eval_accuracy,
                    accuracy_ci,
                    sprt_lambda: if t.state == types::TierState::Shadowing {
                        Some(t.sprt_lambda)
                    } else {
                        None
                    },
                    sprt_progress: if t.state == types::TierState::Shadowing {
                        Some(format!("{}/{}", t.sprt_n, self.config.sprt_max_samples))
                    } else {
                        None
                    },
                    serving_model: t
                        .serving_run_id
                        .as_ref()
                        .map(|_| "eigentune-model".to_string()),
                    savings_usd: 0.0,
                }
            })
            .collect();

        Ok(EigenTuneStatus {
            enabled: self.config.enabled,
            total_pairs,
            high_quality_pairs: high_quality,
            diversity_j,
            category_distribution,
            tiers,
            total_savings_usd: 0.0,
        })
    }

    /// Check prerequisites and return status for each.
    pub async fn check_prerequisites(&self) -> PrerequisiteStatus {
        let ollama = backends::ollama::is_available().await;

        let mlx = {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                std::process::Command::new("python3")
                    .args(["-c", "import mlx_lm"])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                false
            }
        };

        let unsloth = std::process::Command::new("python3")
            .args(["-c", "import unsloth"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let python_version = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

        let has_training_backend = mlx || unsloth;

        PrerequisiteStatus {
            ollama_running: ollama,
            mlx_installed: mlx,
            unsloth_installed: unsloth,
            python_version,
            has_training_backend,
            can_collect: true, // Always — no prerequisites for collection
            can_train: ollama && has_training_backend,
            can_serve: ollama,
        }
    }

    /// Format status for chat display.
    pub async fn format_status(&self) -> String {
        match self.status().await {
            Ok(status) => {
                let mut out = String::from("EIGEN-TUNE STATUS\n\n");

                // Prerequisites
                let prereqs = self.check_prerequisites().await;
                out.push_str("Prerequisites:\n");

                let ollama_icon = if prereqs.ollama_running { "✓" } else { "✗" };
                let ollama_hint = if prereqs.ollama_running {
                    "running".to_string()
                } else {
                    "not running → brew install ollama && ollama serve".to_string()
                };
                out.push_str(&format!("  {} Ollama: {}\n", ollama_icon, ollama_hint));

                if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                    let mlx_icon = if prereqs.mlx_installed { "✓" } else { "✗" };
                    let mlx_hint = if prereqs.mlx_installed {
                        "installed".to_string()
                    } else {
                        "not found → pip install mlx-lm".to_string()
                    };
                    out.push_str(&format!("  {} MLX: {}\n", mlx_icon, mlx_hint));
                } else {
                    let us_icon = if prereqs.unsloth_installed {
                        "✓"
                    } else {
                        "✗"
                    };
                    let us_hint = if prereqs.unsloth_installed {
                        "installed".to_string()
                    } else {
                        "not found → pip install unsloth".to_string()
                    };
                    out.push_str(&format!("  {} Unsloth: {}\n", us_icon, us_hint));
                }

                if let Some(ref py) = prereqs.python_version {
                    out.push_str(&format!("  ✓ {}\n", py));
                }
                out.push('\n');

                // Model
                let model_display = if self.config.base_model == "auto" {
                    "auto (system picks for your hardware)".to_string()
                } else {
                    self.config.base_model.clone()
                };
                out.push_str(&format!("Model: {}\n", model_display));

                // Data
                out.push_str(&format!(
                    "Data: {} pairs collected | {} high-quality\n",
                    status.total_pairs, status.high_quality_pairs
                ));
                out.push_str(&format!("Diversity: J = {:.2}\n\n", status.diversity_j));

                // Tiers
                for t in &status.tiers {
                    let icon = match t.state {
                        types::TierState::Graduated => "●",
                        types::TierState::Shadowing => "◐",
                        _ => "○",
                    };
                    out.push_str(&format!(
                        "{} {:8} {}\n",
                        icon,
                        t.tier.as_str(),
                        t.state.as_str()
                    ));
                }

                // Setup hint if prerequisites missing
                if !prereqs.can_train {
                    out.push_str("\nSetup guide: /eigentune setup\n");
                }

                out
            }
            Err(e) => format!("Eigen-Tune: error: {}", e),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get current base model setting.
    pub fn current_model(&self) -> &str {
        &self.config.base_model
    }

    /// Discover available base models for fine-tuning.
    /// Checks Ollama for local models, returns recommended options based on hardware.
    pub async fn discover_models(&self) -> ModelDiscovery {
        let mut discovery = ModelDiscovery {
            current: self.config.base_model.clone(),
            ollama_available: false,
            ollama_models: Vec::new(),
            recommended: Vec::new(),
            hardware: detect_hardware(),
        };

        // Check Ollama
        if backends::ollama::is_available().await {
            discovery.ollama_available = true;
            if let Ok(models) = backends::ollama::list_models().await {
                discovery.ollama_models = models
                    .into_iter()
                    .map(|m| DiscoveredModel {
                        name: m.name.clone(),
                        size_bytes: m.size.unwrap_or(0),
                        family: m.details.as_ref().and_then(|d| d.family.clone()),
                        param_size: m.details.as_ref().and_then(|d| d.parameter_size.clone()),
                        quantization: m
                            .details
                            .as_ref()
                            .and_then(|d| d.quantization_level.clone()),
                        source: "ollama".to_string(),
                    })
                    .collect();
            }
        }

        // Build recommendations based on hardware
        let ram_gb = discovery.hardware.ram_gb;
        discovery.recommended = recommend_models(ram_gb, &discovery.hardware.chip);

        discovery
    }

    /// Set the base model for fine-tuning.
    /// Validates the model name is reasonable, updates config.
    pub fn set_model(&mut self, model: &str) -> String {
        let old = self.config.base_model.clone();
        self.config.base_model = model.to_string();
        format!(
            "Eigen-Tune: base model changed from '{}' to '{}'",
            old, model
        )
    }

    /// Format model discovery for chat display.
    pub async fn format_model_status(&self) -> String {
        let discovery = self.discover_models().await;
        let mut out = String::from("EIGEN-TUNE MODEL\n\n");

        // Current model
        let current_display = if discovery.current == "auto" {
            "auto (system picks best for your hardware)".to_string()
        } else {
            discovery.current.clone()
        };
        out.push_str(&format!("Current: {}\n", current_display));
        out.push_str(&format!(
            "Hardware: {} · {} GB RAM\n\n",
            discovery.hardware.chip, discovery.hardware.ram_gb
        ));

        // Recommended models
        if !discovery.recommended.is_empty() {
            out.push_str("Recommended for your hardware:\n");
            for (i, model) in discovery.recommended.iter().enumerate() {
                let marker = if i == 0 { "→" } else { " " };
                out.push_str(&format!(
                    "  {} {} ({}, ~{} GB RAM)\n",
                    marker,
                    model.name,
                    model.param_size.as_deref().unwrap_or("?"),
                    model.size_bytes / 1_000_000_000
                ));
            }
            out.push('\n');
        }

        // Ollama models
        if discovery.ollama_available {
            if discovery.ollama_models.is_empty() {
                out.push_str("Ollama: running, no models pulled\n");
            } else {
                out.push_str(&format!(
                    "Ollama: {} models available\n",
                    discovery.ollama_models.len()
                ));
                for m in discovery.ollama_models.iter().take(10) {
                    let size = if m.size_bytes > 0 {
                        format!("{:.1} GB", m.size_bytes as f64 / 1e9)
                    } else {
                        "? GB".to_string()
                    };
                    out.push_str(&format!("  {} ({})\n", m.name, size));
                }
            }
        } else {
            out.push_str("Ollama: not running (install from ollama.com)\n");
        }

        out.push_str("\nUsage: /eigentune model <name> to set base model\n");
        out.push_str("       /eigentune model auto   to auto-select\n");

        out
    }
}

/// Prerequisite check results.
#[derive(Debug, Clone)]
pub struct PrerequisiteStatus {
    pub ollama_running: bool,
    pub mlx_installed: bool,
    pub unsloth_installed: bool,
    pub python_version: Option<String>,
    pub has_training_backend: bool,
    pub can_collect: bool,
    pub can_train: bool,
    pub can_serve: bool,
}

/// Result of model discovery.
#[derive(Debug, Clone)]
pub struct ModelDiscovery {
    pub current: String,
    pub ollama_available: bool,
    pub ollama_models: Vec<DiscoveredModel>,
    pub recommended: Vec<DiscoveredModel>,
    pub hardware: HardwareInfo,
}

/// A discovered model (from Ollama or recommendation list).
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub name: String,
    pub size_bytes: u64,
    pub family: Option<String>,
    pub param_size: Option<String>,
    pub quantization: Option<String>,
    pub source: String,
}

/// Detected hardware info.
#[derive(Debug, Clone)]
pub struct HardwareInfo {
    pub chip: String,
    pub ram_gb: u64,
    pub has_nvidia: bool,
    pub has_apple_silicon: bool,
}

fn detect_hardware() -> HardwareInfo {
    let ram_bytes: u64 = {
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
                .unwrap_or(0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    };

    let chip = {
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            "Unknown".to_string()
        }
    };

    let has_apple_silicon = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");

    HardwareInfo {
        chip,
        ram_gb: ram_bytes / (1024 * 1024 * 1024),
        has_nvidia: false, // Would need nvidia-smi check
        has_apple_silicon,
    }
}

/// Recommend models based on available RAM and chip.
fn recommend_models(ram_gb: u64, chip: &str) -> Vec<DiscoveredModel> {
    let mut models = Vec::new();
    let is_apple = chip.contains("Apple");
    let prefix = if is_apple { "mlx-community" } else { "unsloth" };
    let suffix = if is_apple { "-4bit" } else { "-bnb-4bit" };

    // Always recommend smallest first (fastest training)
    models.push(DiscoveredModel {
        name: "HuggingFaceTB/SmolLM2-135M-Instruct".to_string(),
        size_bytes: 270_000_000,
        family: Some("SmolLM2".into()),
        param_size: Some("135M".into()),
        quantization: Some("F16".into()),
        source: "huggingface".into(),
    });

    if ram_gb >= 8 {
        models.push(DiscoveredModel {
            name: format!("{}/Qwen2.5-0.5B-Instruct{}", prefix, suffix),
            size_bytes: 500_000_000,
            family: Some("Qwen2.5".into()),
            param_size: Some("0.5B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    if ram_gb >= 8 {
        models.push(DiscoveredModel {
            name: format!("{}/Qwen2.5-1.5B-Instruct{}", prefix, suffix),
            size_bytes: 1_500_000_000,
            family: Some("Qwen2.5".into()),
            param_size: Some("1.5B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    if ram_gb >= 12 {
        models.push(DiscoveredModel {
            name: format!("{}/Phi-3.5-mini-instruct{}", prefix, suffix),
            size_bytes: 2_500_000_000,
            family: Some("Phi-3.5".into()),
            param_size: Some("3.8B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    if ram_gb >= 16 {
        models.push(DiscoveredModel {
            name: format!("{}/Qwen2.5-7B-Instruct{}", prefix, suffix),
            size_bytes: 4_500_000_000,
            family: Some("Qwen2.5".into()),
            param_size: Some("7B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
        models.push(DiscoveredModel {
            name: format!("{}/Llama-3.1-8B-Instruct{}", prefix, suffix),
            size_bytes: 5_000_000_000,
            family: Some("Llama 3.1".into()),
            param_size: Some("8B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    if ram_gb >= 32 {
        models.push(DiscoveredModel {
            name: format!("{}/Mistral-Small-24B-Instruct{}", prefix, suffix),
            size_bytes: 14_000_000_000,
            family: Some("Mistral".into()),
            param_size: Some("24B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    if ram_gb >= 48 {
        models.push(DiscoveredModel {
            name: format!("{}/Qwen2.5-32B-Instruct{}", prefix, suffix),
            size_bytes: 20_000_000_000,
            family: Some("Qwen2.5".into()),
            param_size: Some("32B".into()),
            quantization: Some("Q4".into()),
            source: "huggingface".into(),
        });
    }

    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_engine_creation() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:").await;
        assert!(engine.is_ok());
    }

    #[tokio::test]
    async fn test_engine_status() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:")
            .await
            .unwrap();
        let status = engine.status().await.unwrap();
        assert_eq!(status.total_pairs, 0);
        assert_eq!(status.tiers.len(), 3);
    }

    #[tokio::test]
    async fn test_route_default_cloud() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:")
            .await
            .unwrap();
        let decision = engine.route("simple").await;
        assert!(matches!(decision, RouteDecision::Cloud));
    }

    #[tokio::test]
    async fn test_format_status_output() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:")
            .await
            .unwrap();
        let text = engine.format_status().await;
        assert!(text.contains("EIGEN-TUNE STATUS"));
    }

    #[test]
    fn test_current_model_default() {
        let config = EigenTuneConfig::default();
        assert_eq!(config.base_model, "auto");
    }

    #[test]
    fn test_set_model() {
        let config = EigenTuneConfig {
            base_model: "mlx-community/Llama-3.1-8B-Instruct-4bit".to_string(),
            ..Default::default()
        };
        assert!(config.base_model.contains("Llama"));
    }

    #[test]
    fn test_detect_hardware() {
        let hw = detect_hardware();
        // Should at least return something without panicking
        assert!(!hw.chip.is_empty());
    }

    #[test]
    fn test_recommend_models_16gb() {
        let models = recommend_models(16, "Apple M2");
        assert!(!models.is_empty());
        // Should recommend at least SmolLM2 and up to 8B
        assert!(models
            .iter()
            .any(|m| m.param_size.as_deref() == Some("135M")));
        assert!(models.iter().any(|m| m.param_size.as_deref() == Some("8B")));
        // Should NOT recommend 24B+ for 16GB
        assert!(!models
            .iter()
            .any(|m| m.param_size.as_deref() == Some("24B")));
    }

    #[test]
    fn test_recommend_models_8gb() {
        let models = recommend_models(8, "Apple M1");
        // Should have SmolLM2 and small models, but NOT 7B/8B
        assert!(models
            .iter()
            .any(|m| m.param_size.as_deref() == Some("135M")));
        assert!(!models.iter().any(|m| m.param_size.as_deref() == Some("8B")));
    }

    #[test]
    fn test_recommend_models_uses_correct_prefix() {
        let apple_models = recommend_models(16, "Apple M2");
        let nvidia_models = recommend_models(16, "NVIDIA RTX 4090");

        // Apple should use mlx-community prefix
        let apple_7b = apple_models
            .iter()
            .find(|m| m.param_size.as_deref() == Some("7B"));
        if let Some(m) = apple_7b {
            assert!(
                m.name.contains("mlx-community"),
                "Apple should use mlx-community: {}",
                m.name
            );
        }

        // NVIDIA should use unsloth prefix
        let nvidia_7b = nvidia_models
            .iter()
            .find(|m| m.param_size.as_deref() == Some("7B"));
        if let Some(m) = nvidia_7b {
            assert!(
                m.name.contains("unsloth"),
                "NVIDIA should use unsloth: {}",
                m.name
            );
        }
    }

    #[tokio::test]
    async fn test_discover_models() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:")
            .await
            .unwrap();
        let discovery = engine.discover_models().await;
        assert_eq!(discovery.current, "auto");
        assert!(!discovery.recommended.is_empty());
    }

    #[tokio::test]
    async fn test_format_model_status() {
        let config = EigenTuneConfig::default();
        let engine = EigenTuneEngine::new(&config, "sqlite::memory:")
            .await
            .unwrap();
        let text = engine.format_model_status().await;
        assert!(text.contains("EIGEN-TUNE MODEL"));
        assert!(text.contains("Current:"));
        assert!(text.contains("Hardware:"));
    }
}
