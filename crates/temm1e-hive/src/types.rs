//! Core types for the TEMM1E Hive swarm intelligence runtime.
//!
//! All shared data structures used across the Hive subsystem are defined here.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

// ---------------------------------------------------------------------------
// Task Status
// ---------------------------------------------------------------------------

/// Status of a task in the Hive's DAG-based Blackboard.
///
/// State machine:
/// ```text
/// PENDING → READY → ACTIVE → COMPLETE
///                      │
///                      ├→ BLOCKED → RETRY → (READY)
///                      │              └→ ESCALATE
///                      └→ ESCALATE
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveTaskStatus {
    /// Waiting for dependency tasks to complete.
    Pending,
    /// All dependencies met — available for a worker to claim.
    Ready,
    /// Claimed by a worker, execution in progress.
    Active,
    /// Finished successfully.
    Complete,
    /// Worker hit a blocker, awaiting retry or escalation.
    Blocked,
    /// Being re-attempted by a fresh worker.
    Retry,
    /// Exceeded max retries — requires fallback (single-agent or human).
    Escalate,
}

impl HiveTaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::Retry => "retry",
            Self::Escalate => "escalate",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            "active" => Some(Self::Active),
            "complete" => Some(Self::Complete),
            "blocked" => Some(Self::Blocked),
            "retry" => Some(Self::Retry),
            "escalate" => Some(Self::Escalate),
            _ => None,
        }
    }

    /// Whether this status is a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Escalate)
    }
}

impl fmt::Display for HiveTaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Hive Task
// ---------------------------------------------------------------------------

/// A single decomposed subtask in the Hive's Blackboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveTask {
    pub id: String,
    pub order_id: String,
    pub description: String,
    pub status: HiveTaskStatus,
    pub claimed_by: Option<String>,
    pub dependencies: Vec<String>,
    pub context_tags: Vec<String>,
    pub estimated_tokens: u32,
    pub actual_tokens: u32,
    pub result_summary: Option<String>,
    pub artifacts: Vec<String>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub error_log: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Order Status
// ---------------------------------------------------------------------------

/// Status of an overall order (the user's original request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveOrderStatus {
    Active,
    Completed,
    Failed,
}

impl HiveOrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

impl fmt::Display for HiveOrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Hive Order
// ---------------------------------------------------------------------------

/// Tracks the overall user request through the swarm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveOrder {
    pub id: String,
    pub chat_id: String,
    pub original_message: String,
    pub task_count: usize,
    pub completed_count: usize,
    pub status: HiveOrderStatus,
    pub total_tokens: u64,
    pub queen_tokens: u64,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// Pheromone Signal Types
// ---------------------------------------------------------------------------

/// The fixed alphabet of pheromone signal types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    /// Task finished successfully.
    Completion,
    /// Task attempt failed.
    Failure,
    /// Worker is struggling with this task.
    Difficulty,
    /// Task has been waiting — intensity GROWS over time (negative decay).
    Urgency,
    /// Worker heartbeat: "I'm alive on this task."
    Progress,
    /// Worker requesting specialist assistance.
    HelpWanted,
}

impl SignalType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Failure => "failure",
            Self::Difficulty => "difficulty",
            Self::Urgency => "urgency",
            Self::Progress => "progress",
            Self::HelpWanted => "help_wanted",
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "completion" => Some(Self::Completion),
            "failure" => Some(Self::Failure),
            "difficulty" => Some(Self::Difficulty),
            "urgency" => Some(Self::Urgency),
            "progress" => Some(Self::Progress),
            "help_wanted" => Some(Self::HelpWanted),
            _ => None,
        }
    }

    /// Default decay rate per second.
    pub fn default_decay_rate(&self) -> f64 {
        match self {
            Self::Completion => 0.003,
            Self::Failure => 0.002,
            Self::Difficulty => 0.006,
            Self::Urgency => -0.001, // grows over time
            Self::Progress => 0.035,
            Self::HelpWanted => 0.006,
        }
    }

    /// Default initial intensity.
    pub fn default_intensity(&self) -> f64 {
        match self {
            Self::Completion => 1.0,
            Self::Failure => 1.0,
            Self::Difficulty => 0.7,
            Self::Urgency => 0.1,
            Self::Progress => 0.5,
            Self::HelpWanted => 1.0,
        }
    }
}

impl fmt::Display for SignalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Pheromone Signal
// ---------------------------------------------------------------------------

/// A single pheromone signal in the field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PheromoneSignal {
    pub id: i64,
    pub signal_type: SignalType,
    pub target: String,
    pub intensity: f64,
    pub decay_rate: f64,
    pub emitter: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: i64,
}

impl PheromoneSignal {
    /// Compute intensity at a given time (unix epoch ms).
    pub fn intensity_at(&self, now_ms: i64) -> f64 {
        let dt_secs = (now_ms - self.created_at) as f64 / 1000.0;
        if dt_secs < 0.0 {
            return self.intensity;
        }
        let value = self.intensity * (-self.decay_rate * dt_secs).exp();
        if self.decay_rate < 0.0 {
            // Urgency: grows over time, capped at 5.0
            value.min(5.0)
        } else {
            value
        }
    }
}

// ---------------------------------------------------------------------------
// Decomposition Types
// ---------------------------------------------------------------------------

/// Result of the Queen's decomposition of a user request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionResult {
    pub tasks: Vec<DecomposedTask>,
    pub single_agent_recommended: bool,
    pub reasoning: String,
}

/// A single task as produced by decomposition (pre-Blackboard).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposedTask {
    pub id: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub context_tags: Vec<String>,
    pub estimated_tokens: u32,
}

// ---------------------------------------------------------------------------
// Selection Exponents
// ---------------------------------------------------------------------------

/// Tunable exponents for the worker task selection equation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionExponents {
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
    pub delta: f64,
    pub zeta: f64,
}

impl Default for SelectionExponents {
    fn default() -> Self {
        Self {
            alpha: 2.0,
            beta: 1.5,
            gamma: 1.0,
            delta: 0.8,
            zeta: 1.2,
        }
    }
}

// ---------------------------------------------------------------------------
// Worker State
// ---------------------------------------------------------------------------

/// Tracks what a worker is doing and its accumulated expertise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerState {
    pub id: String,
    pub current_task: Option<String>,
    pub recent_tags: HashSet<String>,
    pub tasks_completed: u32,
    pub tokens_used: u64,
}

impl WorkerState {
    pub fn new(id: String) -> Self {
        Self {
            id,
            current_task: None,
            recent_tags: HashSet::new(),
            tasks_completed: 0,
            tokens_used: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Swarm Result
// ---------------------------------------------------------------------------

/// The aggregated result of a swarm execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmResult {
    /// Aggregated text response to the user.
    pub text: String,
    /// Total tokens consumed across all workers + queen.
    pub total_tokens: u64,
    /// Number of tasks that completed successfully.
    pub tasks_completed: usize,
    /// Number of tasks that escalated (fell back to single-agent or human).
    pub tasks_escalated: usize,
    /// Total wall-clock time in milliseconds.
    pub wall_clock_ms: u64,
    /// Number of workers used.
    pub workers_used: usize,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_roundtrip() {
        for status in [
            HiveTaskStatus::Pending,
            HiveTaskStatus::Ready,
            HiveTaskStatus::Active,
            HiveTaskStatus::Complete,
            HiveTaskStatus::Blocked,
            HiveTaskStatus::Retry,
            HiveTaskStatus::Escalate,
        ] {
            let s = status.as_str();
            assert_eq!(HiveTaskStatus::parse_str(s), Some(status));
        }
        assert_eq!(HiveTaskStatus::parse_str("bogus"), None);
    }

    #[test]
    fn terminal_states() {
        assert!(HiveTaskStatus::Complete.is_terminal());
        assert!(HiveTaskStatus::Escalate.is_terminal());
        assert!(!HiveTaskStatus::Pending.is_terminal());
        assert!(!HiveTaskStatus::Ready.is_terminal());
        assert!(!HiveTaskStatus::Active.is_terminal());
    }

    #[test]
    fn order_status_roundtrip() {
        for status in [
            HiveOrderStatus::Active,
            HiveOrderStatus::Completed,
            HiveOrderStatus::Failed,
        ] {
            let s = status.as_str();
            assert_eq!(HiveOrderStatus::parse_str(s), Some(status));
        }
    }

    #[test]
    fn signal_type_roundtrip() {
        for st in [
            SignalType::Completion,
            SignalType::Failure,
            SignalType::Difficulty,
            SignalType::Urgency,
            SignalType::Progress,
            SignalType::HelpWanted,
        ] {
            let s = st.as_str();
            assert_eq!(SignalType::parse_str(s), Some(st));
        }
    }

    #[test]
    fn pheromone_decay() {
        let signal = PheromoneSignal {
            id: 1,
            signal_type: SignalType::Completion,
            target: "t1".into(),
            intensity: 1.0,
            decay_rate: 0.003,
            emitter: None,
            metadata: None,
            created_at: 0,
        };

        // At t=0, intensity should be 1.0
        assert!((signal.intensity_at(0) - 1.0).abs() < 1e-9);

        // After ~231 seconds (half-life for ρ=0.003), intensity ≈ 0.5
        let half_life_ms = (0.693 / 0.003 * 1000.0) as i64;
        let at_half = signal.intensity_at(half_life_ms);
        assert!((at_half - 0.5).abs() < 0.01, "got {at_half}");

        // After long time, intensity → 0
        let at_far = signal.intensity_at(10_000_000); // 10,000 seconds
        assert!(at_far < 0.01, "got {at_far}");
    }

    #[test]
    fn urgency_grows_and_caps() {
        let signal = PheromoneSignal {
            id: 2,
            signal_type: SignalType::Urgency,
            target: "t1".into(),
            intensity: 0.1,
            decay_rate: -0.001, // negative = grows
            emitter: None,
            metadata: None,
            created_at: 0,
        };

        // At t=0
        assert!((signal.intensity_at(0) - 0.1).abs() < 1e-9);

        // After 5 seconds, should be slightly more
        let at_5s = signal.intensity_at(5_000);
        assert!(at_5s > 0.1, "urgency should grow, got {at_5s}");

        // After very long time, capped at 5.0
        let at_far = signal.intensity_at(100_000_000);
        assert!(
            (at_far - 5.0).abs() < 0.01,
            "should cap at 5.0, got {at_far}"
        );
    }

    #[test]
    fn worker_state_new() {
        let ws = WorkerState::new("w1".into());
        assert_eq!(ws.id, "w1");
        assert!(ws.current_task.is_none());
        assert!(ws.recent_tags.is_empty());
        assert_eq!(ws.tasks_completed, 0);
    }

    #[test]
    fn selection_exponents_default() {
        let exp = SelectionExponents::default();
        assert!((exp.alpha - 2.0).abs() < 1e-9);
        assert!((exp.beta - 1.5).abs() < 1e-9);
        assert!((exp.gamma - 1.0).abs() < 1e-9);
        assert!((exp.delta - 0.8).abs() < 1e-9);
        assert!((exp.zeta - 1.2).abs() < 1e-9);
    }

    #[test]
    fn decomposition_serde() {
        let result = DecompositionResult {
            tasks: vec![DecomposedTask {
                id: "t1".into(),
                description: "Build API".into(),
                dependencies: vec![],
                context_tags: vec!["rust".into(), "api".into()],
                estimated_tokens: 3000,
            }],
            single_agent_recommended: false,
            reasoning: "Complex task".into(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: DecompositionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tasks.len(), 1);
        assert_eq!(parsed.tasks[0].id, "t1");
        assert!(!parsed.single_agent_recommended);
    }
}
