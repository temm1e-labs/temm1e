//! Configuration types for the TEMM1E Hive swarm runtime.
//!
//! All config structs use `#[serde(default)]` so that existing TOML configs
//! without a `[hive]` section continue to parse without error.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level Hive Config
// ---------------------------------------------------------------------------

/// Configuration for the Hive swarm intelligence runtime.
///
/// When `enabled = false` (the default), the Hive is completely inert —
/// no SQLite tables are created, no background tasks are spawned,
/// and the agent runtime behaves identically to pre-Hive TEMM1E.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveConfig {
    /// Master switch. Enabled by default since v3.0.0.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Minimum workers to keep alive when swarm is active.
    #[serde(default = "default_min_workers")]
    pub min_workers: usize,

    /// Maximum workers to spawn for a single order.
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,

    /// Minimum theoretical speedup (S_max) required to activate swarm.
    /// If decomposition doesn't yield at least this much parallelism,
    /// the order is handled by a single agent.
    #[serde(default = "default_swarm_threshold_speedup")]
    pub swarm_threshold_speedup: f64,

    /// Maximum fraction of estimated single-agent cost that the Queen's
    /// decomposition may consume. If decomposition is too expensive
    /// relative to the task, fall back to single-agent.
    #[serde(default = "default_queen_cost_ratio_max")]
    pub queen_cost_ratio_max: f64,

    /// Maximum allowed cost ratio: C_swarm / C_single (Axiom A5).
    /// If the swarm would exceed this, it's not activated.
    #[serde(default = "default_budget_overhead_max")]
    pub budget_overhead_max: f64,

    /// Pheromone field configuration.
    #[serde(default)]
    pub pheromone: PheromoneConfig,

    /// Worker task selection configuration.
    #[serde(default)]
    pub selection: SelectionConfig,

    /// Blocker resolution configuration.
    #[serde(default)]
    pub blocker: BlockerConfig,
}

impl Default for HiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_workers: default_min_workers(),
            max_workers: default_max_workers(),
            swarm_threshold_speedup: default_swarm_threshold_speedup(),
            queen_cost_ratio_max: default_queen_cost_ratio_max(),
            budget_overhead_max: default_budget_overhead_max(),
            pheromone: PheromoneConfig::default(),
            selection: SelectionConfig::default(),
            blocker: BlockerConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pheromone Config
// ---------------------------------------------------------------------------

/// Configuration for the pheromone field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PheromoneConfig {
    /// Seconds between garbage collection sweeps.
    #[serde(default = "default_gc_interval_secs")]
    pub gc_interval_secs: u64,

    /// Signals with intensity below this threshold are deleted during GC.
    #[serde(default = "default_evaporation_threshold")]
    pub evaporation_threshold: f64,

    /// Maximum intensity for urgency signals.
    #[serde(default = "default_urgency_cap")]
    pub urgency_cap: f64,
}

impl Default for PheromoneConfig {
    fn default() -> Self {
        Self {
            gc_interval_secs: default_gc_interval_secs(),
            evaporation_threshold: default_evaporation_threshold(),
            urgency_cap: default_urgency_cap(),
        }
    }
}

// ---------------------------------------------------------------------------
// Selection Config
// ---------------------------------------------------------------------------

/// Configuration for the worker task selection equation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionConfig {
    /// Affinity exponent — strong preference for expertise match.
    #[serde(default = "default_alpha")]
    pub alpha: f64,
    /// Urgency exponent — moderate pressure to pick waiting tasks.
    #[serde(default = "default_beta")]
    pub beta: f64,
    /// Difficulty exponent — avoidance of hard tasks (linear).
    #[serde(default = "default_gamma")]
    pub gamma: f64,
    /// Failure exponent — mild avoidance of previously-failed tasks.
    #[serde(default = "default_delta")]
    pub delta: f64,
    /// Downstream reward exponent — preference for unblocking work.
    #[serde(default = "default_zeta")]
    pub zeta: f64,
    /// Scores within this fraction of the max are treated as tied (random pick).
    #[serde(default = "default_tie_threshold")]
    pub tie_threshold: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            alpha: default_alpha(),
            beta: default_beta(),
            gamma: default_gamma(),
            delta: default_delta(),
            zeta: default_zeta(),
            tie_threshold: default_tie_threshold(),
        }
    }
}

// ---------------------------------------------------------------------------
// Blocker Config
// ---------------------------------------------------------------------------

/// Configuration for blocker resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerConfig {
    /// Maximum retry attempts per task before escalation.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Maximum wall-clock seconds a single task may run before timeout.
    #[serde(default = "default_max_task_duration_secs")]
    pub max_task_duration_secs: u64,
}

impl Default for BlockerConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            max_task_duration_secs: default_max_task_duration_secs(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default value functions
// ---------------------------------------------------------------------------

fn default_enabled() -> bool {
    true
}
fn default_min_workers() -> usize {
    1
}
fn default_max_workers() -> usize {
    3
}
fn default_swarm_threshold_speedup() -> f64 {
    1.3
}
fn default_queen_cost_ratio_max() -> f64 {
    0.10
}
fn default_budget_overhead_max() -> f64 {
    1.15
}

fn default_gc_interval_secs() -> u64 {
    10
}
fn default_evaporation_threshold() -> f64 {
    0.01
}
fn default_urgency_cap() -> f64 {
    5.0
}

fn default_alpha() -> f64 {
    2.0
}
fn default_beta() -> f64 {
    1.5
}
fn default_gamma() -> f64 {
    1.0
}
fn default_delta() -> f64 {
    0.8
}
fn default_zeta() -> f64 {
    1.2
}
fn default_tie_threshold() -> f64 {
    0.05
}

fn default_max_retries() -> u32 {
    3
}
fn default_max_task_duration_secs() -> u64 {
    300
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_enabled() {
        let config = HiveConfig::default();
        assert!(config.enabled);
    }

    #[test]
    fn serde_empty_toml() {
        // An empty TOML string should parse to defaults (enabled by default)
        let config: HiveConfig = toml::from_str("").unwrap();
        assert!(config.enabled);
        assert_eq!(config.max_workers, 3);
        assert!((config.swarm_threshold_speedup - 1.3).abs() < 1e-9);
    }

    #[test]
    fn serde_partial_toml() {
        let toml_str = r#"
        enabled = true
        max_workers = 5
        "#;
        let config: HiveConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.max_workers, 5);
        // Defaults for unspecified fields
        assert_eq!(config.min_workers, 1);
        assert!((config.pheromone.urgency_cap - 5.0).abs() < 1e-9);
    }

    #[test]
    fn serde_full_toml() {
        let toml_str = r#"
        enabled = true
        min_workers = 2
        max_workers = 8
        swarm_threshold_speedup = 1.5
        queen_cost_ratio_max = 0.15
        budget_overhead_max = 1.20

        [pheromone]
        gc_interval_secs = 20
        evaporation_threshold = 0.05
        urgency_cap = 3.0

        [selection]
        alpha = 3.0
        beta = 2.0
        gamma = 1.5
        delta = 1.0
        zeta = 1.5
        tie_threshold = 0.10

        [blocker]
        max_retries = 5
        max_task_duration_secs = 600
        "#;
        let config: HiveConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.max_workers, 8);
        assert!((config.selection.alpha - 3.0).abs() < 1e-9);
        assert_eq!(config.blocker.max_retries, 5);
        assert_eq!(config.pheromone.gc_interval_secs, 20);
    }
}
