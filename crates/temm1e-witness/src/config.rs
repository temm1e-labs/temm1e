//! Witness configuration.
//!
//! Phase 1 uses a simple struct with sensible defaults. Phase 2+ loads from
//! `witness.toml` via the `toml` crate.

use serde::{Deserialize, Serialize};

/// Top-level Witness configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessConfig {
    pub enabled: bool,
    pub activate_on: ActivateOn,
    pub override_strictness: OverrideStrictness,
    pub max_overhead_pct: f64,
    pub degrade_to_tier0_on_cap: bool,
    pub tier1_enabled: bool,
    pub tier1_calls_per_subtask: u32,
    pub tier2_enabled: bool,
    pub tier2_advisory_only: bool,
    pub show_per_task_readout: bool,
    pub ledger_path: Option<String>,
    pub live_root_path: Option<String>,
    pub sealed_root_path: Option<String>,
}

impl Default for WitnessConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Default off — must be explicitly enabled
            activate_on: ActivateOn::StandardAndComplex,
            override_strictness: OverrideStrictness::Auto,
            max_overhead_pct: 15.0,
            degrade_to_tier0_on_cap: true,
            tier1_enabled: true,
            tier1_calls_per_subtask: 2,
            tier2_enabled: true,
            tier2_advisory_only: true,
            show_per_task_readout: true,
            ledger_path: None,
            live_root_path: None,
            sealed_root_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivateOn {
    /// Only trivial tasks (debug)
    Simple,
    /// Standard + Complex tasks (default)
    StandardAndComplex,
    /// Complex tasks only
    Complex,
    /// All tasks
    All,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideStrictness {
    /// Use strictness derived from task complexity (default)
    Auto,
    /// Observe only — record verdicts, never affect final reply
    Observe,
    /// Warn — record verdicts, append note to final reply on FAIL
    Warn,
    /// Block — record verdicts, rewrite final reply on FAIL to reflect honestly
    Block,
}

/// Strictness level for a single subtask.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WitnessStrictness {
    /// L1: record to Ledger, never affect final reply
    Observe,
    /// L2: record, append a note on FAIL
    Warn,
    /// L3: record, rewrite final reply on FAIL
    Block,
    /// L5: Block + auto-retry loop (opt-in)
    BlockWithRetry,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = WitnessConfig::default();
        assert!(!c.enabled, "default should be off");
        assert_eq!(c.max_overhead_pct, 15.0);
        assert!(c.tier1_enabled);
        assert!(c.tier2_advisory_only);
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let c = WitnessConfig::default();
        let json = serde_json::to_string(&c).unwrap();
        let back: WitnessConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.enabled, c.enabled);
        assert_eq!(back.activate_on, c.activate_on);
    }
}
