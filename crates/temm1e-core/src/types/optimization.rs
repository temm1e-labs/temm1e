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
///
/// Note (v5.3.6): `max_iterations` and `skip_tool_loop` were verified dead
/// (never read at runtime) and removed in the P4 sweep. Loop ceilings are
/// enforced by `AgentRuntime.max_tool_rounds` (default 0 = unlimited) plus
/// stagnation detection + budget + duration.
#[derive(Debug, Clone)]
pub struct ExecutionProfile {
    /// The prompt tier to use for system prompt construction.
    pub prompt_tier: PromptTier,
    /// How verification is performed.
    pub verify_mode: VerifyMode,
    /// Whether the LEARN phase runs after this task.
    pub use_learn: bool,
    /// Maximum tool output chars (complexity-aware cap).
    pub max_tool_output_chars: usize,
}

impl ExecutionProfile {
    /// Profile for Trivial tasks: minimal prompt, no verification.
    pub fn trivial() -> Self {
        Self {
            prompt_tier: PromptTier::Minimal,
            verify_mode: VerifyMode::Skip,
            use_learn: false,
            max_tool_output_chars: 5_000,
        }
    }

    /// Profile for Simple tasks: rule-based verify, compact output.
    pub fn simple() -> Self {
        Self {
            prompt_tier: PromptTier::Basic,
            verify_mode: VerifyMode::RuleBased,
            use_learn: false,
            max_tool_output_chars: 5_000,
        }
    }

    /// Profile for Standard tasks: full prompt + LLM verification.
    pub fn standard() -> Self {
        Self {
            prompt_tier: PromptTier::Standard,
            verify_mode: VerifyMode::LlmVerify,
            use_learn: true,
            max_tool_output_chars: 15_000,
        }
    }

    /// Profile for Complex tasks: full prompt with higher output cap.
    pub fn complex() -> Self {
        Self {
            prompt_tier: PromptTier::Full,
            verify_mode: VerifyMode::LlmVerify,
            use_learn: true,
            max_tool_output_chars: 30_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_profile_skips_verification() {
        let p = ExecutionProfile::trivial();
        assert!(!p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Minimal);
        assert_eq!(p.verify_mode, VerifyMode::Skip);
    }

    #[test]
    fn simple_profile_uses_rule_based_verify() {
        let p = ExecutionProfile::simple();
        assert!(!p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Basic);
        assert_eq!(p.verify_mode, VerifyMode::RuleBased);
    }

    #[test]
    fn standard_profile_uses_llm_verify_and_learn() {
        let p = ExecutionProfile::standard();
        assert!(p.use_learn);
        assert_eq!(p.prompt_tier, PromptTier::Standard);
        assert_eq!(p.verify_mode, VerifyMode::LlmVerify);
    }

    #[test]
    fn complex_profile_highest_output_cap() {
        let p = ExecutionProfile::complex();
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
