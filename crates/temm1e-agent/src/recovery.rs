//! State Recovery from Durable Storage — on startup, checks for incomplete
//! tasks in SQLite, loads their checkpoints, classifies the appropriate
//! recovery action, and formats user notifications for resumed tasks.

use std::collections::HashSet;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, info, warn};

use crate::task_queue::{TaskEntry, TaskQueue, TaskStatus};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Tasks older than this are abandoned automatically.
const MAX_TASK_AGE_HOURS: i64 = 24;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The action to take when recovering an incomplete task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    /// Resume from checkpoint — the task has valid checkpoint data.
    Resume,
    /// Restart from scratch — the task was pending with no checkpoint.
    Restart,
    /// Abandon — the task is too old or otherwise unrecoverable.
    Abandon {
        /// Human-readable reason for abandonment.
        reason: String,
    },
}

/// A single step that was completed before the interruption, extracted
/// from checkpoint data. Used to prevent re-execution of completed steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveredStep {
    /// Unique identifier for the step within the task.
    pub step_id: String,
    /// The tool that was executed in this step.
    pub tool_name: String,
    /// Whether the step completed successfully.
    pub completed: bool,
}

/// Deserialized checkpoint payload — the structured form of the JSON
/// stored in `TaskEntry.checkpoint_data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointData {
    /// The raw session history (kept as serde_json::Value for flexibility).
    pub history: serde_json::Value,
    /// Steps extracted from the history for idempotency tracking.
    pub completed_steps: Vec<RecoveredStep>,
}

/// A plan for recovering a single task, produced by the recovery manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryPlan {
    /// The task ID from the persistent queue.
    pub task_id: String,
    /// The chat/conversation this task belongs to.
    pub chat_id: String,
    /// The original user goal.
    pub goal: String,
    /// Deserialized checkpoint data (if available).
    pub checkpoint: Option<CheckpointData>,
    /// The recovery action to take.
    pub recovery_action: RecoveryAction,
    /// When the task was originally created.
    pub created_at: DateTime<Utc>,
    /// When the task was last updated.
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// RecoveryManager
// ---------------------------------------------------------------------------

/// Manages task recovery on startup. Tracks which tasks have already been
/// recovered to prevent double-recovery across multiple calls.
pub struct RecoveryManager {
    /// Set of task IDs that have already been recovered (idempotency guard).
    recovered_ids: Mutex<HashSet<String>>,
}

impl RecoveryManager {
    /// Create a new `RecoveryManager` with an empty recovered-ID set.
    pub fn new() -> Self {
        Self {
            recovered_ids: Mutex::new(HashSet::new()),
        }
    }

    /// Scan the task queue for incomplete tasks and produce a recovery plan
    /// for each one. Tasks that have already been recovered (by task ID) are
    /// skipped to guarantee idempotency.
    pub async fn recover_incomplete_tasks(
        &self,
        task_queue: &TaskQueue,
    ) -> Result<Vec<RecoveryPlan>, Temm1eError> {
        info!("Scanning for incomplete tasks to recover");

        let incomplete = task_queue.load_incomplete().await?;

        if incomplete.is_empty() {
            info!("No incomplete tasks found — clean startup");
            return Ok(Vec::new());
        }

        info!(count = incomplete.len(), "Found incomplete tasks");

        let mut plans = Vec::new();

        for task in &incomplete {
            // Idempotency: skip tasks we have already recovered
            {
                let recovered = self
                    .recovered_ids
                    .lock()
                    .map_err(|e| Temm1eError::Internal(format!("Lock poisoned: {e}")))?;
                if recovered.contains(&task.task_id) {
                    debug!(
                        task_id = %task.task_id,
                        "Skipping already-recovered task"
                    );
                    continue;
                }
            }

            let action = classify_recovery(task);
            let checkpoint = parse_checkpoint(&task.checkpoint_data);

            let plan = RecoveryPlan {
                task_id: task.task_id.clone(),
                chat_id: task.chat_id.clone(),
                goal: task.goal.clone(),
                checkpoint,
                recovery_action: action.clone(),
                created_at: task.created_at,
                updated_at: task.updated_at,
            };

            info!(
                task_id = %task.task_id,
                chat_id = %task.chat_id,
                action = ?action,
                "Recovery plan created"
            );

            // Mark this task as recovered
            {
                let mut recovered = self
                    .recovered_ids
                    .lock()
                    .map_err(|e| Temm1eError::Internal(format!("Lock poisoned: {e}")))?;
                recovered.insert(task.task_id.clone());
            }

            plans.push(plan);
        }

        info!(
            total = plans.len(),
            resumed = plans
                .iter()
                .filter(|p| p.recovery_action == RecoveryAction::Resume)
                .count(),
            restarted = plans
                .iter()
                .filter(|p| p.recovery_action == RecoveryAction::Restart)
                .count(),
            abandoned = plans
                .iter()
                .filter(|p| matches!(p.recovery_action, RecoveryAction::Abandon { .. }))
                .count(),
            "Recovery scan complete"
        );

        Ok(plans)
    }

    /// Check whether a task has already been recovered.
    pub fn is_recovered(&self, task_id: &str) -> bool {
        self.recovered_ids
            .lock()
            .map(|set| set.contains(task_id))
            .unwrap_or(false)
    }

    /// Return the number of tasks that have been recovered so far.
    pub fn recovered_count(&self) -> usize {
        self.recovered_ids.lock().map(|set| set.len()).unwrap_or(0)
    }

    /// Mark incomplete tasks as abandoned in the task queue. This updates
    /// the status in SQLite so they are no longer returned by `load_incomplete`.
    pub async fn apply_abandonments(
        &self,
        task_queue: &TaskQueue,
        plans: &[RecoveryPlan],
    ) -> Result<usize, Temm1eError> {
        let mut count = 0;
        for plan in plans {
            if let RecoveryAction::Abandon { ref reason } = plan.recovery_action {
                warn!(
                    task_id = %plan.task_id,
                    reason = %reason,
                    "Abandoning stale task"
                );
                task_queue
                    .update_status(&plan.task_id, TaskStatus::Failed)
                    .await?;
                count += 1;
            }
        }
        Ok(count)
    }
}

impl Default for RecoveryManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Classification logic
// ---------------------------------------------------------------------------

/// Classify the recovery action for a single task based on its state.
///
/// Rules:
/// - If the task is older than 24 hours -> Abandon
/// - If checkpoint data exists and is valid JSON -> Resume
/// - If no checkpoint but status is Pending -> Restart
/// - If no checkpoint and status is Running (crashed mid-execution) -> Restart
pub fn classify_recovery(task: &TaskEntry) -> RecoveryAction {
    let now = Utc::now();
    let age = now.signed_duration_since(task.created_at);

    // Age-based abandonment
    if age > Duration::hours(MAX_TASK_AGE_HOURS) {
        let hours = age.num_hours();
        return RecoveryAction::Abandon {
            reason: format!("Task is {hours} hours old (limit: {MAX_TASK_AGE_HOURS}h)"),
        };
    }

    // Check for valid checkpoint data
    if let Some(ref data) = task.checkpoint_data {
        if !data.is_empty() {
            // Validate that it's parseable JSON
            if serde_json::from_str::<serde_json::Value>(data).is_ok() {
                return RecoveryAction::Resume;
            }
            // Checkpoint exists but is corrupt — restart
            warn!(
                task_id = %task.task_id,
                "Checkpoint data is not valid JSON — will restart"
            );
            return RecoveryAction::Restart;
        }
    }

    // No checkpoint — restart regardless of whether it was Pending or Running
    match task.status {
        TaskStatus::Pending => RecoveryAction::Restart,
        TaskStatus::Running => {
            // Was running but has no checkpoint — crashed before first checkpoint
            RecoveryAction::Restart
        }
        // Should not happen (load_incomplete only returns Pending/Running),
        // but handle gracefully
        _ => RecoveryAction::Abandon {
            reason: format!("Unexpected status {:?} in incomplete list", task.status),
        },
    }
}

// ---------------------------------------------------------------------------
// Checkpoint parsing
// ---------------------------------------------------------------------------

/// Parse checkpoint JSON into a structured `CheckpointData`. The checkpoint
/// is the serialized session history (a JSON array of ChatMessage). We wrap
/// it and extract completed tool steps for idempotency.
fn parse_checkpoint(raw: &Option<String>) -> Option<CheckpointData> {
    let data = raw.as_ref()?;
    if data.is_empty() {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(data).ok()?;

    // Extract completed steps from the history. The history is an array of
    // ChatMessage objects. Tool results appear as objects with role "Tool"
    // and content containing ToolResult parts.
    let completed_steps = extract_completed_steps(&value);

    Some(CheckpointData {
        history: value,
        completed_steps,
    })
}

/// Walk the session history JSON and extract tool executions that completed
/// successfully. Each tool_use followed by a non-error tool_result is
/// considered a completed step.
fn extract_completed_steps(history: &serde_json::Value) -> Vec<RecoveredStep> {
    let mut steps = Vec::new();

    let entries = match history.as_array() {
        Some(arr) => arr,
        None => return steps,
    };

    // Track tool_use IDs and names from assistant messages
    let mut tool_use_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for entry in entries {
        let role = entry.get("role").and_then(|r| r.as_str()).unwrap_or("");

        let parts = match entry.get("content") {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            Some(serde_json::Value::Object(obj)) => {
                // Single content part wrapped in an object
                vec![serde_json::Value::Object(obj.clone())]
            }
            _ => continue,
        };

        if role == "Assistant" || role == "assistant" {
            for part in &parts {
                if let (Some(id), Some(name)) = (
                    part.get("id").and_then(|v| v.as_str()),
                    part.get("name").and_then(|v| v.as_str()),
                ) {
                    // This is a ToolUse part
                    if part.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                        || part.get("ToolUse").is_some()
                        || !name.is_empty()
                    {
                        tool_use_map.insert(id.to_string(), name.to_string());
                    }
                }
            }
        }

        if role == "Tool" || role == "tool" {
            for part in &parts {
                let tool_use_id = part
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_error = part
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if !tool_use_id.is_empty() {
                    let tool_name = tool_use_map
                        .get(tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());

                    steps.push(RecoveredStep {
                        step_id: tool_use_id.to_string(),
                        tool_name,
                        completed: !is_error,
                    });
                }
            }
        }
    }

    steps
}

// ---------------------------------------------------------------------------
// Notification formatting
// ---------------------------------------------------------------------------

/// Format a user-facing notification summarising recovered tasks. This message
/// is intended to be sent to each affected chat so users know their tasks
/// are being resumed.
///
/// Returns `None` if the plans list is empty.
pub fn format_recovery_notification(plans: &[RecoveryPlan]) -> Option<String> {
    if plans.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    lines.push("System restarted. Recovering incomplete tasks:\n".to_string());

    for plan in plans {
        let action_label = match &plan.recovery_action {
            RecoveryAction::Resume => "Resuming from checkpoint",
            RecoveryAction::Restart => "Restarting from scratch",
            RecoveryAction::Abandon { .. } => "Abandoned (too old)",
        };

        let step_info = if let Some(ref cp) = plan.checkpoint {
            let total = cp.completed_steps.len();
            let done = cp.completed_steps.iter().filter(|s| s.completed).count();
            format!(" [{done}/{total} steps completed]")
        } else {
            String::new()
        };

        lines.push(format!(
            "  - {}: \"{}\"{} -- {}",
            truncate_id(&plan.task_id),
            truncate_goal(&plan.goal, 60),
            step_info,
            action_label,
        ));
    }

    let resume_count = plans
        .iter()
        .filter(|p| p.recovery_action == RecoveryAction::Resume)
        .count();
    let restart_count = plans
        .iter()
        .filter(|p| p.recovery_action == RecoveryAction::Restart)
        .count();
    let abandon_count = plans
        .iter()
        .filter(|p| matches!(p.recovery_action, RecoveryAction::Abandon { .. }))
        .count();

    lines.push(String::new());
    lines.push(format!(
        "Summary: {resume_count} resumed, {restart_count} restarted, {abandon_count} abandoned."
    ));

    Some(lines.join("\n"))
}

/// Format per-chat notifications. Groups plans by chat_id and returns a
/// map of chat_id -> notification string.
pub fn format_per_chat_notifications(
    plans: &[RecoveryPlan],
) -> std::collections::HashMap<String, String> {
    let mut by_chat: std::collections::HashMap<String, Vec<&RecoveryPlan>> =
        std::collections::HashMap::new();

    for plan in plans {
        by_chat.entry(plan.chat_id.clone()).or_default().push(plan);
    }

    let mut result = std::collections::HashMap::new();
    for (chat_id, chat_plans) in &by_chat {
        let owned: Vec<RecoveryPlan> = chat_plans.iter().map(|p| (*p).clone()).collect();
        if let Some(notification) = format_recovery_notification(&owned) {
            result.insert(chat_id.clone(), notification);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a UUID to its first 8 characters for display.
fn truncate_id(id: &str) -> &str {
    if id.len() > 8 {
        &id[..8]
    } else {
        id
    }
}

/// Truncate a goal string to `max_len` bytes (char-boundary-safe), adding "..." if truncated.
fn truncate_goal(goal: &str, max_len: usize) -> String {
    if goal.len() <= max_len {
        goal.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !goal.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &goal[..end])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_queue::{TaskEntry, TaskQueue, TaskStatus};
    use chrono::{Duration, Utc};

    // ── Helper factories ──────────────────────────────────────

    fn make_task(
        task_id: &str,
        status: TaskStatus,
        checkpoint: Option<&str>,
        age_hours: i64,
    ) -> TaskEntry {
        let now = Utc::now();
        TaskEntry {
            task_id: task_id.to_string(),
            chat_id: "chat-1".to_string(),
            goal: "deploy the app".to_string(),
            status,
            checkpoint_data: checkpoint.map(|s| s.to_string()),
            created_at: now - Duration::hours(age_hours),
            updated_at: now - Duration::hours(age_hours),
        }
    }

    async fn make_queue() -> TaskQueue {
        TaskQueue::new("sqlite::memory:").await.unwrap()
    }

    // ── classify_recovery tests ───────────────────────────────

    #[test]
    fn classify_pending_no_checkpoint_restarts() {
        let task = make_task("task-1", TaskStatus::Pending, None, 1);
        assert_eq!(classify_recovery(&task), RecoveryAction::Restart);
    }

    #[test]
    fn classify_running_no_checkpoint_restarts() {
        let task = make_task("task-2", TaskStatus::Running, None, 1);
        assert_eq!(classify_recovery(&task), RecoveryAction::Restart);
    }

    #[test]
    fn classify_with_valid_checkpoint_resumes() {
        let checkpoint = r#"[{"role":"User","content":"hello"}]"#;
        let task = make_task("task-3", TaskStatus::Running, Some(checkpoint), 1);
        assert_eq!(classify_recovery(&task), RecoveryAction::Resume);
    }

    #[test]
    fn classify_with_invalid_json_checkpoint_restarts() {
        let task = make_task("task-4", TaskStatus::Running, Some("not-json{{{"), 1);
        assert_eq!(classify_recovery(&task), RecoveryAction::Restart);
    }

    #[test]
    fn classify_with_empty_checkpoint_restarts() {
        let task = make_task("task-5", TaskStatus::Running, Some(""), 1);
        assert_eq!(classify_recovery(&task), RecoveryAction::Restart);
    }

    #[test]
    fn classify_old_task_abandoned() {
        let task = make_task("task-6", TaskStatus::Pending, None, 25);
        match classify_recovery(&task) {
            RecoveryAction::Abandon { reason } => {
                assert!(reason.contains("25 hours"));
                assert!(reason.contains("24h"));
            }
            other => panic!("Expected Abandon, got {:?}", other),
        }
    }

    #[test]
    fn classify_old_task_with_checkpoint_still_abandoned() {
        let checkpoint = r#"[{"role":"User","content":"hello"}]"#;
        let task = make_task("task-7", TaskStatus::Running, Some(checkpoint), 48);
        match classify_recovery(&task) {
            RecoveryAction::Abandon { reason } => {
                assert!(reason.contains("48 hours"));
            }
            other => panic!("Expected Abandon, got {:?}", other),
        }
    }

    #[test]
    fn classify_just_under_24h_not_abandoned() {
        // A task created 23 hours ago should not be abandoned
        let task = make_task("task-8", TaskStatus::Pending, None, 23);
        assert_eq!(classify_recovery(&task), RecoveryAction::Restart);
    }

    #[test]
    fn classify_over_24h_abandoned() {
        // A task created 25 hours ago should be abandoned
        let task = make_task("task-9", TaskStatus::Pending, None, 25);
        match classify_recovery(&task) {
            RecoveryAction::Abandon { reason } => {
                assert!(reason.contains("25 hours"));
            }
            other => panic!("Expected Abandon, got {:?}", other),
        }
    }

    // ── parse_checkpoint tests ────────────────────────────────

    #[test]
    fn parse_checkpoint_none() {
        assert!(parse_checkpoint(&None).is_none());
    }

    #[test]
    fn parse_checkpoint_empty_string() {
        assert!(parse_checkpoint(&Some(String::new())).is_none());
    }

    #[test]
    fn parse_checkpoint_invalid_json() {
        assert!(parse_checkpoint(&Some("not json".to_string())).is_none());
    }

    #[test]
    fn parse_checkpoint_valid_json_array() {
        let data = r#"[{"role":"User","content":"hello"}]"#;
        let cp = parse_checkpoint(&Some(data.to_string())).unwrap();
        assert!(cp.history.is_array());
        assert!(cp.completed_steps.is_empty());
    }

    #[test]
    fn parse_checkpoint_extracts_tool_steps() {
        let data = serde_json::json!([
            {
                "role": "User",
                "content": {"Text": "deploy"}
            },
            {
                "role": "Assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tu-1",
                        "name": "shell",
                        "input": {"command": "ls"}
                    }
                ]
            },
            {
                "role": "Tool",
                "content": [
                    {
                        "tool_use_id": "tu-1",
                        "content": "file1.rs file2.rs",
                        "is_error": false
                    }
                ]
            }
        ]);
        let cp = parse_checkpoint(&Some(data.to_string())).unwrap();
        assert_eq!(cp.completed_steps.len(), 1);
        assert_eq!(cp.completed_steps[0].step_id, "tu-1");
        assert_eq!(cp.completed_steps[0].tool_name, "shell");
        assert!(cp.completed_steps[0].completed);
    }

    #[test]
    fn parse_checkpoint_error_tool_step_not_completed() {
        let data = serde_json::json!([
            {
                "role": "Assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tu-2",
                        "name": "file_write",
                        "input": {}
                    }
                ]
            },
            {
                "role": "Tool",
                "content": [
                    {
                        "tool_use_id": "tu-2",
                        "content": "Permission denied",
                        "is_error": true
                    }
                ]
            }
        ]);
        let cp = parse_checkpoint(&Some(data.to_string())).unwrap();
        assert_eq!(cp.completed_steps.len(), 1);
        assert!(!cp.completed_steps[0].completed);
    }

    // ── Notification formatting tests ─────────────────────────

    #[test]
    fn format_notification_empty_plans() {
        assert!(format_recovery_notification(&[]).is_none());
    }

    #[test]
    fn format_notification_single_resume() {
        let plan = RecoveryPlan {
            task_id: "abcdef12-3456-7890-abcd-ef1234567890".to_string(),
            chat_id: "chat-1".to_string(),
            goal: "deploy the app".to_string(),
            checkpoint: Some(CheckpointData {
                history: serde_json::json!([]),
                completed_steps: vec![
                    RecoveredStep {
                        step_id: "s1".to_string(),
                        tool_name: "shell".to_string(),
                        completed: true,
                    },
                    RecoveredStep {
                        step_id: "s2".to_string(),
                        tool_name: "file_write".to_string(),
                        completed: false,
                    },
                ],
            }),
            recovery_action: RecoveryAction::Resume,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let notification = format_recovery_notification(&[plan]).unwrap();
        assert!(notification.contains("Recovering incomplete tasks"));
        assert!(notification.contains("abcdef12"));
        assert!(notification.contains("deploy the app"));
        assert!(notification.contains("Resuming from checkpoint"));
        assert!(notification.contains("[1/2 steps completed]"));
        assert!(notification.contains("1 resumed, 0 restarted, 0 abandoned"));
    }

    #[test]
    fn format_notification_mixed_actions() {
        let plans = vec![
            RecoveryPlan {
                task_id: "task-resume".to_string(),
                chat_id: "chat-1".to_string(),
                goal: "task one".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Resume,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RecoveryPlan {
                task_id: "task-restart".to_string(),
                chat_id: "chat-1".to_string(),
                goal: "task two".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Restart,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RecoveryPlan {
                task_id: "task-abandon".to_string(),
                chat_id: "chat-2".to_string(),
                goal: "task three".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Abandon {
                    reason: "too old".to_string(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];

        let notification = format_recovery_notification(&plans).unwrap();
        assert!(notification.contains("1 resumed, 1 restarted, 1 abandoned"));
    }

    #[test]
    fn format_notification_long_goal_truncated() {
        let long_goal = "a".repeat(100);
        let plan = RecoveryPlan {
            task_id: "task-long".to_string(),
            chat_id: "chat-1".to_string(),
            goal: long_goal,
            checkpoint: None,
            recovery_action: RecoveryAction::Restart,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let notification = format_recovery_notification(&[plan]).unwrap();
        assert!(notification.contains("..."));
        // The truncated goal should be 60 chars + "..."
        assert!(notification.contains(&"a".repeat(60)));
    }

    // ── Per-chat notification grouping tests ──────────────────

    #[test]
    fn per_chat_notifications_groups_correctly() {
        let plans = vec![
            RecoveryPlan {
                task_id: "t1".to_string(),
                chat_id: "chat-A".to_string(),
                goal: "goal A".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Restart,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RecoveryPlan {
                task_id: "t2".to_string(),
                chat_id: "chat-B".to_string(),
                goal: "goal B".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Resume,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            RecoveryPlan {
                task_id: "t3".to_string(),
                chat_id: "chat-A".to_string(),
                goal: "goal C".to_string(),
                checkpoint: None,
                recovery_action: RecoveryAction::Abandon {
                    reason: "stale".to_string(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];

        let notifs = format_per_chat_notifications(&plans);
        assert_eq!(notifs.len(), 2);
        assert!(notifs.contains_key("chat-A"));
        assert!(notifs.contains_key("chat-B"));

        // chat-A should have 2 tasks
        let chat_a = &notifs["chat-A"];
        assert!(chat_a.contains("goal A"));
        assert!(chat_a.contains("goal C"));

        // chat-B should have 1 task
        let chat_b = &notifs["chat-B"];
        assert!(chat_b.contains("goal B"));
    }

    // ── RecoveryManager idempotency tests ─────────────────────

    #[tokio::test]
    async fn recovery_manager_idempotent() {
        let tq = make_queue().await;
        tq.create_task("chat-1", "task alpha").await.unwrap();
        tq.create_task("chat-1", "task beta").await.unwrap();

        let manager = RecoveryManager::new();

        // First recovery should find 2 tasks
        let plans1 = manager.recover_incomplete_tasks(&tq).await.unwrap();
        assert_eq!(plans1.len(), 2);
        assert_eq!(manager.recovered_count(), 2);

        // Second recovery should find 0 (both already recovered)
        let plans2 = manager.recover_incomplete_tasks(&tq).await.unwrap();
        assert_eq!(plans2.len(), 0);
    }

    #[tokio::test]
    async fn recovery_manager_is_recovered() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "test task").await.unwrap();

        let manager = RecoveryManager::new();
        assert!(!manager.is_recovered(&id));

        let _plans = manager.recover_incomplete_tasks(&tq).await.unwrap();
        assert!(manager.is_recovered(&id));
    }

    #[tokio::test]
    async fn recovery_manager_new_tasks_after_recovery() {
        let tq = make_queue().await;
        tq.create_task("chat-1", "first task").await.unwrap();

        let manager = RecoveryManager::new();

        let plans1 = manager.recover_incomplete_tasks(&tq).await.unwrap();
        assert_eq!(plans1.len(), 1);

        // Add a new task
        tq.create_task("chat-1", "second task").await.unwrap();

        // Should only find the new task
        let plans2 = manager.recover_incomplete_tasks(&tq).await.unwrap();
        assert_eq!(plans2.len(), 1);
        assert_eq!(plans2[0].goal, "second task");
    }

    // ── apply_abandonments tests ──────────────────────────────

    #[tokio::test]
    async fn apply_abandonments_updates_status() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "old task").await.unwrap();

        let plans = vec![RecoveryPlan {
            task_id: id.clone(),
            chat_id: "chat-1".to_string(),
            goal: "old task".to_string(),
            checkpoint: None,
            recovery_action: RecoveryAction::Abandon {
                reason: "too old".to_string(),
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let manager = RecoveryManager::new();
        let count = manager.apply_abandonments(&tq, &plans).await.unwrap();
        assert_eq!(count, 1);

        // The task should no longer appear in incomplete
        let incomplete = tq.load_incomplete().await.unwrap();
        assert!(incomplete.is_empty());
    }

    #[tokio::test]
    async fn apply_abandonments_skips_non_abandoned() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "active task").await.unwrap();

        let plans = vec![RecoveryPlan {
            task_id: id.clone(),
            chat_id: "chat-1".to_string(),
            goal: "active task".to_string(),
            checkpoint: None,
            recovery_action: RecoveryAction::Restart,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];

        let manager = RecoveryManager::new();
        let count = manager.apply_abandonments(&tq, &plans).await.unwrap();
        assert_eq!(count, 0);

        // The task should still be incomplete
        let incomplete = tq.load_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 1);
    }

    // ── Integration: full recovery flow ───────────────────────

    #[tokio::test]
    async fn full_recovery_flow() {
        let tq = make_queue().await;

        // Create tasks with different states
        let id_pending = tq.create_task("chat-1", "pending task").await.unwrap();
        let id_running = tq.create_task("chat-1", "running task").await.unwrap();
        let id_checkpoint = tq.create_task("chat-2", "checkpointed task").await.unwrap();
        let id_done = tq.create_task("chat-1", "completed task").await.unwrap();

        tq.update_status(&id_running, TaskStatus::Running)
            .await
            .unwrap();
        tq.update_status(&id_checkpoint, TaskStatus::Running)
            .await
            .unwrap();
        tq.update_status(&id_done, TaskStatus::Completed)
            .await
            .unwrap();

        let checkpoint_json = r#"[{"role":"User","content":"hello"}]"#;
        tq.checkpoint(&id_checkpoint, checkpoint_json)
            .await
            .unwrap();

        let manager = RecoveryManager::new();
        let plans = manager.recover_incomplete_tasks(&tq).await.unwrap();

        // Should have 3 plans (pending, running, checkpointed) — not completed
        assert_eq!(plans.len(), 3);

        // Find each plan
        let pending_plan = plans.iter().find(|p| p.task_id == id_pending).unwrap();
        let running_plan = plans.iter().find(|p| p.task_id == id_running).unwrap();
        let checkpoint_plan = plans.iter().find(|p| p.task_id == id_checkpoint).unwrap();

        assert_eq!(pending_plan.recovery_action, RecoveryAction::Restart);
        assert_eq!(running_plan.recovery_action, RecoveryAction::Restart);
        assert_eq!(checkpoint_plan.recovery_action, RecoveryAction::Resume);

        // Checkpoint plan should have parsed checkpoint data
        assert!(checkpoint_plan.checkpoint.is_some());

        // Per-chat notifications
        let notifs = format_per_chat_notifications(&plans);
        assert!(notifs.contains_key("chat-1"));
        assert!(notifs.contains_key("chat-2"));
    }

    // ── Helper function tests ─────────────────────────────────

    #[test]
    fn truncate_id_works() {
        assert_eq!(truncate_id("abcdef12-3456"), "abcdef12");
        assert_eq!(truncate_id("short"), "short");
        assert_eq!(truncate_id("12345678"), "12345678");
    }

    #[test]
    fn truncate_goal_works() {
        assert_eq!(truncate_goal("short", 60), "short");
        let long = "a".repeat(100);
        let truncated = truncate_goal(&long, 60);
        assert_eq!(truncated.len(), 63); // 60 + "..."
        assert!(truncated.ends_with("..."));
    }

    // ── RecoveryManager default trait ─────────────────────────

    #[test]
    fn recovery_manager_default() {
        let manager = RecoveryManager::default();
        assert_eq!(manager.recovered_count(), 0);
    }

    // ── extract_completed_steps edge cases ────────────────────

    #[test]
    fn extract_steps_non_array_returns_empty() {
        let val = serde_json::json!({"not": "an array"});
        assert!(extract_completed_steps(&val).is_empty());
    }

    #[test]
    fn extract_steps_empty_array() {
        let val = serde_json::json!([]);
        assert!(extract_completed_steps(&val).is_empty());
    }

    #[test]
    fn extract_steps_multiple_tools() {
        let data = serde_json::json!([
            {
                "role": "Assistant",
                "content": [
                    {"type": "tool_use", "id": "t1", "name": "shell", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "file_read", "input": {}}
                ]
            },
            {
                "role": "Tool",
                "content": [
                    {"tool_use_id": "t1", "content": "ok", "is_error": false},
                    {"tool_use_id": "t2", "content": "data", "is_error": false}
                ]
            }
        ]);
        let steps = extract_completed_steps(&data);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tool_name, "shell");
        assert_eq!(steps[1].tool_name, "file_read");
        assert!(steps.iter().all(|s| s.completed));
    }
}
