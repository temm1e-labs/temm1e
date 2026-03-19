//! Cross-Task Learning — extracts and persists learnings from completed
//! tasks. After each task, the runtime analyses the conversation history to
//! determine what worked, what failed, and distils an actionable lesson.
//! These learnings are stored in memory with a `learning:` prefix and
//! injected into future context to avoid repeating mistakes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temm1e_core::types::message::{ChatMessage, ContentPart, MessageContent, Role};

/// A single learning extracted from a completed task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskLearning {
    /// Inferred task type based on tools used (e.g. "shell+file", "browser+web_fetch").
    pub task_type: String,
    /// Sequence of tools used during the task.
    pub approach: Vec<String>,
    /// Whether the task succeeded or failed.
    pub outcome: TaskOutcome,
    /// The extracted insight — what worked or what to avoid.
    pub lesson: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskOutcome {
    Success,
    Failure,
    Partial,
}

/// Extract learnings from a completed conversation history.
///
/// Analyses the tool calls, successes, failures, and strategy rotations
/// to produce actionable lessons for future tasks.
pub fn extract_learnings(history: &[ChatMessage]) -> Vec<TaskLearning> {
    let mut learnings = Vec::new();

    // Collect all tool names used (from assistant messages with ToolUse parts)
    let mut tools_used: Vec<String> = Vec::new();
    let mut tool_failures: Vec<(String, String)> = Vec::new(); // (tool_name, error_snippet)
    let mut tool_successes: Vec<String> = Vec::new();
    let mut had_strategy_rotation = false;

    for msg in history {
        match &msg.role {
            Role::Assistant => {
                if let MessageContent::Parts(parts) = &msg.content {
                    for part in parts {
                        if let ContentPart::ToolUse { name, .. } = part {
                            if !tools_used.contains(name) {
                                tools_used.push(name.clone());
                            }
                        }
                    }
                }
            }
            Role::Tool => {
                if let MessageContent::Parts(parts) = &msg.content {
                    for part in parts {
                        if let ContentPart::ToolResult {
                            content, is_error, ..
                        } = part
                        {
                            if *is_error {
                                // Try to extract tool name from context
                                let tool_name = extract_tool_name_from_result(content);
                                let snippet = truncate_error(content, 100);
                                tool_failures.push((tool_name, snippet));
                            } else {
                                let tool_name = extract_tool_name_from_result(content);
                                if !tool_name.is_empty() {
                                    tool_successes.push(tool_name);
                                }
                            }
                            if content.contains("[STRATEGY ROTATION]") {
                                had_strategy_rotation = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Don't generate learnings for trivial conversations (no tool use)
    if tools_used.is_empty() {
        return learnings;
    }

    let task_type = infer_task_type(&tools_used);
    let outcome = determine_outcome(history, &tool_failures);

    // Generate a lesson based on the conversation pattern
    let lesson = generate_lesson(
        &task_type,
        &tools_used,
        &tool_failures,
        &tool_successes,
        had_strategy_rotation,
        &outcome,
    );

    if !lesson.is_empty() {
        learnings.push(TaskLearning {
            task_type,
            approach: tools_used,
            outcome,
            lesson,
            timestamp: Utc::now(),
        });
    }

    learnings
}

/// Format learnings for injection into context.
///
/// Produces a compact summary suitable for a system message, staying within
/// the token budget for learnings (~5% of total context).
pub fn format_learnings_context(learnings: &[TaskLearning]) -> String {
    if learnings.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    lines.push("Past task learnings (apply where relevant):".to_string());

    for (i, learning) in learnings.iter().enumerate().take(5) {
        let outcome_str = match &learning.outcome {
            TaskOutcome::Success => "OK",
            TaskOutcome::Failure => "FAIL",
            TaskOutcome::Partial => "PARTIAL",
        };
        lines.push(format!(
            "  {}. [{}] {}: {}",
            i + 1,
            outcome_str,
            learning.task_type,
            learning.lesson
        ));
    }

    lines.join("\n")
}

/// Serialize a TaskLearning for storage in the Memory backend.
pub fn serialize_learning(learning: &TaskLearning) -> String {
    format!(
        "learning:{}\n{}\ntools: {}\noutcome: {:?}\nlesson: {}",
        learning.timestamp.to_rfc3339(),
        learning.task_type,
        learning.approach.join(", "),
        learning.outcome,
        learning.lesson,
    )
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn infer_task_type(tools: &[String]) -> String {
    if tools.is_empty() {
        return "conversation".to_string();
    }

    let has_shell = tools.iter().any(|t| t == "shell");
    let has_browser = tools.iter().any(|t| t == "browser");
    let has_file = tools.iter().any(|t| t.starts_with("file"));
    let has_web = tools.iter().any(|t| t == "web_fetch");

    match (has_shell, has_browser, has_file, has_web) {
        (true, true, _, _) => "shell+browser".to_string(),
        (true, _, true, _) => "shell+file".to_string(),
        (true, _, _, true) => "shell+web".to_string(),
        (true, _, _, _) => "shell".to_string(),
        (_, true, _, _) => "browser".to_string(),
        (_, _, true, true) => "file+web".to_string(),
        (_, _, true, _) => "file".to_string(),
        (_, _, _, true) => "web".to_string(),
        _ => tools.join("+"),
    }
}

fn determine_outcome(history: &[ChatMessage], failures: &[(String, String)]) -> TaskOutcome {
    // Check the final assistant message for success/failure indicators
    let final_text = history
        .iter()
        .rev()
        .find_map(|msg| {
            if matches!(msg.role, Role::Assistant) {
                match &msg.content {
                    MessageContent::Text(t) => Some(t.clone()),
                    MessageContent::Parts(parts) => parts.iter().find_map(|p| {
                        if let ContentPart::Text { text } = p {
                            Some(text.clone())
                        } else {
                            None
                        }
                    }),
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
        .to_lowercase();

    let success_indicators = [
        "successfully",
        "completed",
        "done",
        "finished",
        "created",
        "deployed",
        "installed",
    ];
    let failure_indicators = [
        "failed",
        "error",
        "unable to",
        "cannot",
        "couldn't",
        "impossible",
    ];

    let has_success = success_indicators.iter().any(|s| final_text.contains(s));
    let has_failure = failure_indicators.iter().any(|s| final_text.contains(s));

    if has_success && !has_failure && failures.len() <= 1 {
        TaskOutcome::Success
    } else if has_failure && !has_success {
        TaskOutcome::Failure
    } else if !failures.is_empty() {
        TaskOutcome::Partial
    } else {
        TaskOutcome::Success
    }
}

fn generate_lesson(
    task_type: &str,
    tools: &[String],
    failures: &[(String, String)],
    _successes: &[String],
    had_rotation: bool,
    outcome: &TaskOutcome,
) -> String {
    let mut parts = Vec::new();

    match outcome {
        TaskOutcome::Success => {
            parts.push(format!(
                "Task type '{}' succeeded using: {}.",
                task_type,
                tools.join(" → ")
            ));
        }
        TaskOutcome::Failure => {
            parts.push(format!("Task type '{}' failed.", task_type,));
        }
        TaskOutcome::Partial => {
            parts.push(format!(
                "Task type '{}' partially completed with {} error(s).",
                task_type,
                failures.len()
            ));
        }
    }

    if !failures.is_empty() {
        let unique_errors: Vec<&str> = failures
            .iter()
            .map(|(_, err)| err.as_str())
            .take(3)
            .collect();
        parts.push(format!("Errors encountered: {}", unique_errors.join("; ")));
    }

    if had_rotation {
        parts.push(
            "Strategy rotation was triggered — initial approach failed repeatedly.".to_string(),
        );
    }

    parts.join(" ")
}

fn extract_tool_name_from_result(content: &str) -> String {
    // Tool results don't always contain the tool name directly.
    // Use heuristics from the content.
    if content.contains("command") || content.contains("exit code") || content.contains("$ ") {
        return "shell".to_string();
    }
    if content.contains("file") || content.contains("path") {
        return "file".to_string();
    }
    if content.contains("http") || content.contains("url") || content.contains("fetch") {
        return "web_fetch".to_string();
    }
    String::new()
}

fn truncate_error(content: &str, max_len: usize) -> String {
    if content.len() <= max_len {
        content.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &content[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool_use_msg(tool_name: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: "tu-1".to_string(),
                name: tool_name.to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }]),
        }
    }

    fn make_tool_result(content: &str, is_error: bool) -> ChatMessage {
        ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: "tu-1".to_string(),
                content: content.to_string(),
                is_error,
            }]),
        }
    }

    fn make_text_msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessageContent::Text(text.to_string()),
        }
    }

    #[test]
    fn no_learnings_for_empty_history() {
        let learnings = extract_learnings(&[]);
        assert!(learnings.is_empty());
    }

    #[test]
    fn no_learnings_for_text_only_conversation() {
        let history = vec![
            make_text_msg(Role::User, "Hello"),
            make_text_msg(Role::Assistant, "Hi there!"),
        ];
        let learnings = extract_learnings(&history);
        assert!(learnings.is_empty());
    }

    #[test]
    fn learning_from_successful_shell_task() {
        let history = vec![
            make_text_msg(Role::User, "List files"),
            make_tool_use_msg("shell"),
            make_tool_result("file1.txt\nfile2.txt", false),
            make_text_msg(Role::Assistant, "Successfully listed files."),
        ];
        let learnings = extract_learnings(&history);
        assert_eq!(learnings.len(), 1);
        assert_eq!(learnings[0].task_type, "shell");
        assert_eq!(learnings[0].outcome, TaskOutcome::Success);
        assert!(learnings[0].approach.contains(&"shell".to_string()));
    }

    #[test]
    fn learning_from_failed_task() {
        let history = vec![
            make_text_msg(Role::User, "Deploy app"),
            make_tool_use_msg("shell"),
            make_tool_result("Tool execution error: command not found", true),
            make_text_msg(
                Role::Assistant,
                "I was unable to deploy — the command failed.",
            ),
        ];
        let learnings = extract_learnings(&history);
        assert_eq!(learnings.len(), 1);
        assert_eq!(learnings[0].outcome, TaskOutcome::Failure);
    }

    #[test]
    fn learning_with_strategy_rotation() {
        let history = vec![
            make_text_msg(Role::User, "Fix the server"),
            make_tool_use_msg("shell"),
            make_tool_result(
                "Error: permission denied\n[STRATEGY ROTATION] Try alternative approach",
                true,
            ),
            make_tool_use_msg("shell"),
            make_tool_result("Server restarted successfully", false),
            make_text_msg(Role::Assistant, "Successfully fixed the server."),
        ];
        let learnings = extract_learnings(&history);
        assert_eq!(learnings.len(), 1);
        assert!(learnings[0].lesson.contains("Strategy rotation"));
    }

    #[test]
    fn infer_task_type_shell_and_file() {
        assert_eq!(
            infer_task_type(&["shell".to_string(), "file_read".to_string()]),
            "shell+file"
        );
    }

    #[test]
    fn infer_task_type_browser_only() {
        assert_eq!(infer_task_type(&["browser".to_string()]), "browser");
    }

    #[test]
    fn infer_task_type_web_fetch_only() {
        assert_eq!(infer_task_type(&["web_fetch".to_string()]), "web");
    }

    #[test]
    fn format_learnings_empty() {
        assert_eq!(format_learnings_context(&[]), "");
    }

    #[test]
    fn format_learnings_non_empty() {
        let learnings = vec![TaskLearning {
            task_type: "shell".to_string(),
            approach: vec!["shell".to_string()],
            outcome: TaskOutcome::Success,
            lesson: "Use shell for file ops".to_string(),
            timestamp: Utc::now(),
        }];
        let formatted = format_learnings_context(&learnings);
        assert!(formatted.contains("Past task learnings"));
        assert!(formatted.contains("[OK]"));
        assert!(formatted.contains("Use shell for file ops"));
    }

    #[test]
    fn format_learnings_capped_at_five() {
        let learnings: Vec<TaskLearning> = (0..10)
            .map(|i| TaskLearning {
                task_type: format!("type-{i}"),
                approach: vec!["shell".to_string()],
                outcome: TaskOutcome::Success,
                lesson: format!("lesson {i}"),
                timestamp: Utc::now(),
            })
            .collect();
        let formatted = format_learnings_context(&learnings);
        // Should only have entries 1-5
        assert!(formatted.contains("1."));
        assert!(formatted.contains("5."));
        assert!(!formatted.contains("6."));
    }

    #[test]
    fn serialize_learning_format() {
        let learning = TaskLearning {
            task_type: "shell".to_string(),
            approach: vec!["shell".to_string(), "file_read".to_string()],
            outcome: TaskOutcome::Success,
            lesson: "Always verify after write".to_string(),
            timestamp: Utc::now(),
        };
        let serialized = serialize_learning(&learning);
        assert!(serialized.starts_with("learning:"));
        assert!(serialized.contains("shell, file_read"));
        assert!(serialized.contains("Always verify after write"));
    }
}
