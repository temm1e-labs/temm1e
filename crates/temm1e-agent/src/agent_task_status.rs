//! Real-time agent task status — observable state for the Interceptor architecture.
//!
//! The agent loop emits `AgentTaskStatus` updates via a `tokio::sync::watch` channel
//! at every state transition. External observers (future Interceptor, dispatcher, CLI)
//! can subscribe via `watch::Receiver<AgentTaskStatus>` and read the current state
//! at any time — zero-cost if nobody is listening.
//!
//! Phase 1: status emission + CancellationToken infrastructure.
//! Phase 2: mid-stream cancellation via `tokio::select!`.
//! Phase 3: full Interceptor with message classification.
//! v4.8.0: enriched tool phase events (args preview, duration, result).

use std::time::Instant;

/// What the agent is doing right now.
#[derive(Debug, Clone)]
pub enum AgentTaskPhase {
    /// Parsing user input, loading images, detecting credentials.
    Preparing,
    /// Classifying message complexity (V2 optimization).
    Classifying,
    /// Building context and sending request to LLM provider.
    CallingProvider { round: u32 },
    /// Executing a tool call.
    ExecutingTool {
        round: u32,
        tool_name: String,
        tool_index: u32,
        tool_total: u32,
        /// JSON-serialized tool args, truncated to ~80 chars for UI display.
        args_preview: String,
        /// Milliseconds elapsed since the agent task started when this
        /// tool began executing (monotonic — safe from wall-clock shifts).
        started_at_ms: u64,
    },
    /// A tool call just finished (success or failure). Emitted AFTER
    /// `ExecutingTool` and BEFORE the next `CallingProvider` round.
    ToolCompleted {
        round: u32,
        tool_name: String,
        tool_index: u32,
        tool_total: u32,
        /// Wall-clock duration of the tool execution in milliseconds.
        duration_ms: u64,
        /// True if the tool returned Ok(output) with is_error == false.
        ok: bool,
        /// First non-empty line of the tool output, truncated to ~80 chars.
        result_preview: String,
    },
    /// Agent loop exited — building final reply.
    Finishing,
    /// Done — result returned to caller.
    Done,
    /// Interrupted by user or system.
    Interrupted { round: u32 },
}

/// Observable snapshot of agent task progress.
///
/// Updated via `watch::Sender::send_modify()` which is infallible —
/// no new panic paths introduced. All fields are `Send + Sync + Clone`.
#[derive(Debug, Clone)]
pub struct AgentTaskStatus {
    /// Current phase of the agent loop.
    pub phase: AgentTaskPhase,
    /// When this task started processing.
    pub started_at: Instant,
    /// Number of completed tool-use rounds (provider call + tool execution).
    pub rounds_completed: u32,
    /// Total tools executed across all rounds.
    pub tools_executed: u32,
    /// Cumulative input tokens consumed.
    pub input_tokens: u32,
    /// Cumulative output tokens consumed.
    pub output_tokens: u32,
    /// Cumulative cost in USD.
    pub cost_usd: f64,
}

impl Default for AgentTaskStatus {
    fn default() -> Self {
        Self {
            phase: AgentTaskPhase::Preparing,
            started_at: Instant::now(),
            rounds_completed: 0,
            tools_executed: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }
    }
}

impl std::fmt::Display for AgentTaskPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preparing => write!(f, "Preparing"),
            Self::Classifying => write!(f, "Classifying request"),
            Self::CallingProvider { round } => write!(f, "Thinking (round {round})"),
            Self::ExecutingTool {
                round,
                tool_name,
                tool_index,
                tool_total,
                ..
            } => write!(
                f,
                "Running {tool_name} ({}/{tool_total}, round {round})",
                tool_index + 1
            ),
            Self::ToolCompleted {
                tool_name,
                duration_ms,
                ok,
                ..
            } => {
                let sym = if *ok { "✓" } else { "✗" };
                write!(f, "{sym} {tool_name} ({duration_ms}ms)")
            }
            Self::Finishing => write!(f, "Finishing up"),
            Self::Done => write!(f, "Done"),
            Self::Interrupted { round } => write!(f, "Interrupted at round {round}"),
        }
    }
}

impl AgentTaskStatus {
    /// Create a new status with the current timestamp.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_preparing() {
        let status = AgentTaskStatus::default();
        assert!(matches!(status.phase, AgentTaskPhase::Preparing));
        assert_eq!(status.rounds_completed, 0);
        assert_eq!(status.tools_executed, 0);
        assert_eq!(status.input_tokens, 0);
        assert_eq!(status.output_tokens, 0);
        assert!((status.cost_usd - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn status_is_clone() {
        let status = AgentTaskStatus::default();
        let cloned = status.clone();
        assert!(matches!(cloned.phase, AgentTaskPhase::Preparing));
        assert_eq!(cloned.rounds_completed, 0);
    }

    #[test]
    fn phase_variants_clone_correctly() {
        let phases = vec![
            AgentTaskPhase::Preparing,
            AgentTaskPhase::Classifying,
            AgentTaskPhase::CallingProvider { round: 3 },
            AgentTaskPhase::ExecutingTool {
                round: 2,
                tool_name: "shell".to_string(),
                tool_index: 1,
                tool_total: 3,
                args_preview: "{\"command\":\"ls\"}".to_string(),
                started_at_ms: 500,
            },
            AgentTaskPhase::ToolCompleted {
                round: 2,
                tool_name: "shell".to_string(),
                tool_index: 1,
                tool_total: 3,
                duration_ms: 42,
                ok: true,
                result_preview: "hello".to_string(),
            },
            AgentTaskPhase::Finishing,
            AgentTaskPhase::Done,
            AgentTaskPhase::Interrupted { round: 5 },
        ];
        for phase in phases {
            let _cloned = phase.clone();
        }
    }

    #[test]
    fn display_tool_completed() {
        let phase = AgentTaskPhase::ToolCompleted {
            round: 1,
            tool_name: "shell".to_string(),
            tool_index: 0,
            tool_total: 1,
            duration_ms: 42,
            ok: true,
            result_preview: "hello".to_string(),
        };
        assert_eq!(phase.to_string(), "✓ shell (42ms)");

        let phase = AgentTaskPhase::ToolCompleted {
            round: 1,
            tool_name: "shell".to_string(),
            tool_index: 0,
            tool_total: 1,
            duration_ms: 100,
            ok: false,
            result_preview: "error".to_string(),
        };
        assert_eq!(phase.to_string(), "✗ shell (100ms)");
    }

    #[test]
    fn status_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentTaskStatus>();
        assert_send_sync::<AgentTaskPhase>();
    }

    #[test]
    fn new_equals_default() {
        let a = AgentTaskStatus::new();
        let b = AgentTaskStatus::default();
        assert_eq!(a.rounds_completed, b.rounds_completed);
        assert_eq!(a.tools_executed, b.tools_executed);
        assert!(matches!(a.phase, AgentTaskPhase::Preparing));
    }
}
