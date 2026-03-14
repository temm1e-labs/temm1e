//! Agentic core optimization types — complexity classification, prompt tiers,
//! and execution profiles for token-efficient agent behavior.

use serde::{Deserialize, Serialize};

/// Task complexity level — extends the existing Simple/Standard/Complex with Trivial.
/// Used by the runtime to select prompt tier, output caps, and loop behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PromptTier {
    /// ~300 tokens. Identity + safety rules only. For greetings, thanks, etc.
    Minimal,
    /// ~800 tokens. Minimal + tool names (no schemas) + basic guidelines.
    Basic,
    /// ~2000 tokens. Full prompt with tool schemas, verification, DONE criteria.
    Standard,
    /// ~2500 tokens. Standard + planning/delegation/learning protocols.
    Full,
}

/// Verification mode — how the runtime verifies tool output correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerifyMode {
    /// No verification at all (Trivial tasks).
    Skip,
    /// Rule-based checks only: exit code, HTTP status, schema validation.
    RuleBased,
    /// Full LLM-based verification via prompt injection.
    LlmVerify,
}

/// Execution profile derived from task complexity. Immutable once created.
/// Configures which pipeline stages activate for a given task.
#[derive(Debug, Clone)]
pub struct ExecutionProfile {
    /// The prompt tier to use for system prompt construction.
    pub prompt_tier: PromptTier,
    /// How verification is performed.
    pub verify_mode: VerifyMode,
    /// Whether the LEARN phase runs after this task.
    pub use_learn: bool,
    /// Maximum tool loop iterations.
    pub max_iterations: u32,
    /// Maximum tool output chars (complexity-aware cap).
    pub max_tool_output_chars: usize,
    /// Whether to skip the tool loop entirely (Trivial fast-path).
    pub skip_tool_loop: bool,
}

impl ExecutionProfile {
    /// Profile for Trivial tasks: skip everything, just respond.
    pub fn trivial() -> Self {
        Self {
            prompt_tier: PromptTier::Minimal,
            verify_mode: VerifyMode::Skip,
            use_learn: false,
            max_iterations: 1,
            max_tool_output_chars: 5_000,
            skip_tool_loop: true,
        }
    }

    /// Profile for Simple tasks: single tool call, rule-based verify.
    pub fn simple() -> Self {
        Self {
            prompt_tier: PromptTier::Basic,
            verify_mode: VerifyMode::RuleBased,
            use_learn: false,
            max_iterations: 2,
            max_tool_output_chars: 5_000,
            skip_tool_loop: false,
        }
    }

    /// Profile for Standard tasks: normal full loop.
    pub fn standard() -> Self {
        Self {
            prompt_tier: PromptTier::Standard,
            verify_mode: VerifyMode::LlmVerify,
            use_learn: true,
            max_iterations: 5,
            max_tool_output_chars: 15_000,
            skip_tool_loop: false,
        }
    }

    /// Profile for Complex tasks: full loop with higher limits.
    pub fn complex() -> Self {
        Self {
            prompt_tier: PromptTier::Full,
            verify_mode: VerifyMode::LlmVerify,
            use_learn: true,
            max_iterations: 10,
            max_tool_output_chars: 30_000,
            skip_tool_loop: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_profile_skips_tool_loop() {
        let p = ExecutionProfile::trivial();
        assert!(p.skip_tool_loop);
        assert!(!p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Minimal);
        assert_eq!(p.verify_mode, VerifyMode::Skip);
    }

    #[test]
    fn simple_profile_uses_rule_based_verify() {
        let p = ExecutionProfile::simple();
        assert!(!p.skip_tool_loop);
        assert!(!p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Basic);
        assert_eq!(p.verify_mode, VerifyMode::RuleBased);
    }

    #[test]
    fn standard_profile_uses_llm_verify_and_learn() {
        let p = ExecutionProfile::standard();
        assert!(!p.skip_tool_loop);
        assert!(p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Standard);
        assert_eq!(p.verify_mode, VerifyMode::LlmVerify);
    }

    #[test]
    fn complex_profile_highest_limits() {
        let p = ExecutionProfile::complex();
        assert_eq!(p.max_iterations, 10);
        assert_eq!(p.max_tool_output_chars, 30_000);
        assert_eq!(p.prompt_tier, PromptTier::Full);
    }

    #[test]
    fn output_caps_scale_with_complexity() {
        assert!(
            ExecutionProfile::trivial().max_tool_output_chars
                < ExecutionProfile::simple().max_tool_output_chars
                || ExecutionProfile::trivial().max_tool_output_chars
                    == ExecutionProfile::simple().max_tool_output_chars
        );
        assert!(
            ExecutionProfile::simple().max_tool_output_chars
                < ExecutionProfile::standard().max_tool_output_chars
        );
        assert!(
            ExecutionProfile::standard().max_tool_output_chars
                < ExecutionProfile::complex().max_tool_output_chars
        );
    }

    #[test]
    fn prompt_tier_serde_roundtrip() {
        let tier = PromptTier::Standard;
        let json = serde_json::to_string(&tier).unwrap();
        let restored: PromptTier = serde_json::from_str(&json).unwrap();
        assert_eq!(tier, restored);
    }
}
