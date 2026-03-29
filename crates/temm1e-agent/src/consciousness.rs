//! Tem Conscious — consciousness observation types and data collection.
//!
//! Defines the `TurnObservation` struct that captures the agent's internal
//! state after each turn, and the `ConsciousnessIntervention` enum that
//! represents what the consciousness sub-agent decides to do.

use serde::{Deserialize, Serialize};

/// A snapshot of the agent's internal state at the end of a turn.
/// This is what the consciousness sub-agent observes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnObservation {
    /// Turn number in this session (1-based).
    pub turn_number: u32,
    /// Session identifier.
    pub session_id: String,

    // -- User input --
    /// First 200 chars of the user's message.
    pub user_message_preview: String,

    // -- Classification --
    /// Message category (Chat, Order, Stop).
    pub category: String,
    /// Task difficulty (Simple, Standard, Complex).
    pub difficulty: String,

    // -- Provider call --
    /// Model used for this turn.
    pub model_used: String,
    /// Tokens consumed this turn.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Cost of this turn in USD.
    pub cost_usd: f64,

    // -- Budget --
    /// Cumulative cost across the session.
    pub cumulative_cost_usd: f64,
    /// Budget limit (0 = unlimited).
    pub budget_limit_usd: f64,

    // -- Tool execution --
    /// Tools called this turn.
    pub tools_called: Vec<String>,
    /// Tool results: "success" or error message.
    pub tool_results: Vec<String>,
    /// Consecutive failures for any tool.
    pub max_consecutive_failures: u32,
    /// Number of strategy rotations triggered.
    pub strategy_rotations: u32,

    // -- Response --
    /// First 200 chars of the agent's response.
    pub response_preview: String,

    // -- Circuit breaker --
    /// Circuit breaker state: "closed", "open", "half_open".
    pub circuit_breaker_state: String,

    // -- Consciousness history --
    /// Notes from consciousness in this session so far.
    pub previous_notes: Vec<String>,
}

/// What the consciousness sub-agent decides to do after observing a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsciousnessIntervention {
    /// No intervention needed — the turn looks fine.
    NoAction,
    /// Inject a context note into the next turn's system prompt.
    /// The note is ephemeral — consumed after one use.
    Whisper(String),
    /// Trigger a targeted memory recall for the next turn.
    Redirect {
        /// Query to search λ-Memory with.
        memory_query: String,
    },
    /// Block a specific tool call in the next turn (safety override).
    Override {
        /// Tool name to block.
        block_tool: String,
        /// Reason for blocking.
        reason: String,
    },
}

/// Configuration for the consciousness (consciousness) system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsciousnessConfig {
    /// Enable consciousness observation. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum confidence to inject a whisper (0.0-1.0).
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    /// Maximum interventions per session before consciousness goes quiet.
    #[serde(default = "default_max_interventions")]
    pub max_interventions_per_session: u32,
    /// Observation mode: "rules_first" (default), "always_llm", "rules_only".
    #[serde(default = "default_observation_mode")]
    pub observation_mode: String,
}

impl Default for ConsciousnessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            confidence_threshold: default_confidence_threshold(),
            max_interventions_per_session: default_max_interventions(),
            observation_mode: default_observation_mode(),
        }
    }
}

fn default_confidence_threshold() -> f64 {
    0.7
}
fn default_max_interventions() -> u32 {
    10
}
fn default_observation_mode() -> String {
    "rules_first".into()
}

/// Helper to truncate a string safely at char boundaries.
pub fn safe_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    // Find the last char boundary at or before max_len
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consciousness_config_defaults() {
        let config = ConsciousnessConfig::default();
        assert!(!config.enabled);
        assert!((config.confidence_threshold - 0.7).abs() < f64::EPSILON);
        assert_eq!(config.max_interventions_per_session, 10);
        assert_eq!(config.observation_mode, "rules_first");
    }

    #[test]
    fn test_safe_preview_short() {
        assert_eq!(safe_preview("hello", 10), "hello");
    }

    #[test]
    fn test_safe_preview_truncated() {
        let result = safe_preview("hello world this is a long message", 10);
        assert!(result.len() <= 13); // 10 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_safe_preview_utf8() {
        // Vietnamese text with multi-byte chars
        let text = "Xin chào thế giới";
        let result = safe_preview(text, 8);
        assert!(result.ends_with("..."));
        // Must not panic on char boundary
    }

    #[test]
    fn test_no_action_serialization() {
        let intervention = ConsciousnessIntervention::NoAction;
        let json = serde_json::to_string(&intervention).unwrap();
        assert!(json.contains("NoAction"));
    }

    #[test]
    fn test_whisper_serialization() {
        let intervention = ConsciousnessIntervention::Whisper("check your budget".to_string());
        let json = serde_json::to_string(&intervention).unwrap();
        assert!(json.contains("Whisper"));
        assert!(json.contains("check your budget"));
    }

    #[test]
    fn test_turn_observation_creation() {
        let obs = TurnObservation {
            turn_number: 1,
            session_id: "test-session".into(),
            user_message_preview: "hello".into(),
            category: "Chat".into(),
            difficulty: "Simple".into(),
            model_used: "gemini-3-flash-preview".into(),
            input_tokens: 500,
            output_tokens: 100,
            cost_usd: 0.001,
            cumulative_cost_usd: 0.001,
            budget_limit_usd: 0.0,
            tools_called: vec![],
            tool_results: vec![],
            max_consecutive_failures: 0,
            strategy_rotations: 0,
            response_preview: "hi there".into(),
            circuit_breaker_state: "closed".into(),
            previous_notes: vec![],
        };
        assert_eq!(obs.turn_number, 1);
        assert_eq!(obs.category, "Chat");
    }
}
