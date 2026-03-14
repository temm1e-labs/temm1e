//! Conversation history pruning with semantic importance scoring.
//!
//! Scores each message by semantic importance and prunes low-importance
//! messages first, regardless of age. Pruned segments are summarized so
//! the model retains awareness of earlier context.
//!
//! # Scoring heuristics
//!
//! Each message is assigned an [`MessageImportance`] level based on its
//! content. A position bonus is added to the most recent messages to
//! ensure conversational continuity. When in doubt, the scorer is
//! conservative — it keeps the message.
//!
//! # Usage
//!
//! ```ignore
//! use temm1e_agent::history_pruning::{prune_history, score_message};
//!
//! let scored: Vec<_> = history
//!     .iter()
//!     .enumerate()
//!     .map(|(i, msg)| score_message(msg, i, history.len()))
//!     .collect();
//!
//! let pruned = prune_history(&history, 20);
//! println!("Kept {} messages, dropped {}", pruned.kept_messages.len(), pruned.dropped_count);
//! ```

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use temm1e_core::types::message::{ChatMessage, ContentPart, MessageContent, Role};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of most-recent messages that receive a position bonus.
const RECENCY_WINDOW: usize = 4;

/// Score bonus applied to messages within the recency window.
const RECENCY_BONUS: f32 = 3.0;

/// Short-message threshold (character count). Messages shorter than this
/// from non-user roles may be scored as trivial if their content is not
/// otherwise significant.
const SHORT_MESSAGE_THRESHOLD: usize = 20;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Importance level assigned to a message during scoring.
///
/// Each variant carries a base numeric score used for ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MessageImportance {
    /// Trivial content — greetings, acknowledgments, very short filler.
    Trivial = 1,
    /// Low importance — status updates, empty tool results.
    Low = 3,
    /// Medium importance — regular tool results, substantive responses.
    Medium = 5,
    /// High importance — tool errors, code blocks, structured data.
    High = 7,
    /// Critical — user instructions/decisions, error messages, explicit markers.
    Critical = 10,
}

impl MessageImportance {
    /// Returns the base numeric score for this importance level.
    pub fn base_score(self) -> f32 {
        self as u8 as f32
    }
}

/// A scored message with its importance classification and reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMessage {
    /// Index of the message within the original history slice.
    pub index: usize,
    /// Assigned importance level.
    pub importance: MessageImportance,
    /// Final numeric score (base score + position bonus).
    pub score: f32,
    /// Human-readable reason for the assigned importance.
    pub reason: String,
}

/// Result of pruning a conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrunedHistory {
    /// Messages that survived pruning, in their original order.
    pub kept_messages: Vec<ChatMessage>,
    /// Brief summary of what was pruned.
    pub summary: String,
    /// Number of messages that were dropped.
    pub dropped_count: usize,
}

// ---------------------------------------------------------------------------
// Atomic turn grouping — prevents orphaned tool_result messages
// ---------------------------------------------------------------------------

/// A group of messages that form an atomic conversation turn.
///
/// Tool-use and tool-result pairs are kept together as indivisible units.
/// Pruning operates on turns, not individual messages, to prevent orphaned
/// `tool_result` messages that cause provider API errors (e.g., Anthropic
/// 400: `unexpected tool_use_id found in tool_result blocks`).
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    /// Indices into the original message array.
    pub indices: Vec<usize>,
}

/// Extract tool_use IDs from a message's content parts.
fn extract_tool_use_ids(msg: &ChatMessage) -> Vec<String> {
    match &msg.content {
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Extract tool_result tool_use_ids from a message's content parts.
fn extract_tool_result_ids(msg: &ChatMessage) -> Vec<String> {
    match &msg.content {
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Group messages into atomic conversation turns.
///
/// Messages containing `tool_use` are grouped with their corresponding
/// `tool_result` messages. This ensures pruning never orphans a `tool_result`
/// by dropping its `tool_use` (or vice versa), which would cause provider
/// API errors.
///
/// The algorithm walks forward through messages:
/// 1. When an Assistant message contains `tool_use` parts, it starts a group.
/// 2. Subsequent messages with matching `tool_result` IDs are added to the group.
/// 3. All other messages form single-message groups.
pub fn group_into_turns(messages: &[ChatMessage]) -> Vec<ConversationTurn> {
    let mut turns: Vec<ConversationTurn> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let tool_use_ids = extract_tool_use_ids(&messages[i]);

        if !tool_use_ids.is_empty() {
            // This message has tool_use parts — group with matching tool_results
            let mut indices = vec![i];
            let mut pending_ids: HashSet<String> = tool_use_ids.into_iter().collect();

            // Look ahead for matching tool_result messages
            let mut j = i + 1;
            while j < messages.len() && !pending_ids.is_empty() {
                let result_ids = extract_tool_result_ids(&messages[j]);
                if result_ids.iter().any(|id| pending_ids.contains(id)) {
                    indices.push(j);
                    for id in &result_ids {
                        pending_ids.remove(id);
                    }
                    j += 1;
                } else {
                    break;
                }
            }

            turns.push(ConversationTurn { indices });
            i = j;
        } else {
            // Non-tool-use message — standalone turn
            turns.push(ConversationTurn { indices: vec![i] });
            i += 1;
        }
    }

    turns
}

/// Remove orphaned `tool_result` messages whose `tool_use_id` doesn't match
/// any `tool_use` in the message list. This is a safety net applied after
/// all pruning to catch pre-existing orphans from crashes or prior bugs.
pub fn remove_orphaned_tool_results(messages: &mut Vec<ChatMessage>) {
    // Collect all tool_use IDs present in the messages
    let tool_use_ids: HashSet<String> = messages
        .iter()
        .flat_map(|msg| match &msg.content {
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect();

    let before = messages.len();

    // Remove messages that contain only tool_results referencing missing tool_use IDs
    messages.retain(|msg| {
        let result_ids = extract_tool_result_ids(msg);
        if result_ids.is_empty() {
            return true; // Not a tool_result message — keep
        }
        // Keep if ALL tool_result IDs have matching tool_use IDs
        result_ids.iter().all(|id| tool_use_ids.contains(id))
    });

    let removed = before - messages.len();
    if removed > 0 {
        warn!(
            removed,
            "Removed orphaned tool_result messages (no matching tool_use)"
        );
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Score a single message by semantic importance.
///
/// The scoring considers the message role, content patterns, and position
/// within the conversation. The heuristics are intentionally conservative:
/// when in doubt, the message receives a higher importance score.
///
/// # Arguments
///
/// * `msg` — The message to score.
/// * `index` — Zero-based index of the message in the history.
/// * `total` — Total number of messages in the history.
pub fn score_message(msg: &ChatMessage, index: usize, total: usize) -> ScoredMessage {
    let (importance, reason) = classify_message(msg);
    let mut score = importance.base_score();

    // Position bonus: most recent messages always get a boost.
    if total > 0 && index >= total.saturating_sub(RECENCY_WINDOW) {
        score += RECENCY_BONUS;
    }

    ScoredMessage {
        index,
        importance,
        score,
        reason,
    }
}

/// Classify a message into an importance level with a reason string.
fn classify_message(msg: &ChatMessage) -> (MessageImportance, String) {
    match &msg.content {
        MessageContent::Text(text) => classify_text_message(&msg.role, text),
        MessageContent::Parts(parts) => classify_parts_message(&msg.role, parts),
    }
}

/// Classify a plain-text message.
fn classify_text_message(role: &Role, text: &str) -> (MessageImportance, String) {
    let text_lower = text.to_lowercase();
    let trimmed = text.trim();

    match role {
        Role::User => classify_user_text(&text_lower, trimmed),
        Role::Assistant => classify_assistant_text(&text_lower, trimmed),
        Role::System => (MessageImportance::Critical, "System message".to_string()),
        Role::Tool => classify_tool_text(&text_lower, trimmed),
    }
}

/// Classify user text. User messages default to at least Medium importance
/// because they represent the human's intent.
fn classify_user_text(text_lower: &str, trimmed: &str) -> (MessageImportance, String) {
    // Check for explicit importance markers first.
    if contains_importance_marker(text_lower) {
        return (
            MessageImportance::Critical,
            "User message with importance marker".to_string(),
        );
    }

    // Check for instructions / decisions.
    if contains_instruction_pattern(text_lower) {
        return (
            MessageImportance::Critical,
            "User message with instructions/decisions".to_string(),
        );
    }

    // Code blocks indicate substantive content.
    if contains_code_block(trimmed) {
        return (
            MessageImportance::High,
            "User message with code block".to_string(),
        );
    }

    // Structured data (JSON, YAML, etc.)
    if contains_structured_data(trimmed) {
        return (
            MessageImportance::High,
            "User message with structured data".to_string(),
        );
    }

    // Trivial greetings / acknowledgments.
    if is_greeting_or_ack(text_lower, trimmed) {
        return (
            MessageImportance::Trivial,
            "User greeting/acknowledgment".to_string(),
        );
    }

    // Very short messages that aren't commands — conservative: keep as Low
    // rather than Trivial so we err on the side of keeping.
    if trimmed.len() < SHORT_MESSAGE_THRESHOLD && !looks_like_command(text_lower) {
        return (MessageImportance::Low, "Short user message".to_string());
    }

    // Default: user messages are at least Medium.
    (
        MessageImportance::Medium,
        "Regular user message".to_string(),
    )
}

/// Classify assistant text.
fn classify_assistant_text(text_lower: &str, trimmed: &str) -> (MessageImportance, String) {
    // Error reports from the assistant are important.
    if contains_error_pattern(text_lower) {
        return (
            MessageImportance::High,
            "Assistant message reporting error".to_string(),
        );
    }

    // Code blocks indicate substantive content.
    if contains_code_block(trimmed) {
        return (
            MessageImportance::High,
            "Assistant message with code block".to_string(),
        );
    }

    // Structured data.
    if contains_structured_data(trimmed) {
        return (
            MessageImportance::High,
            "Assistant message with structured data".to_string(),
        );
    }

    // Status updates.
    if is_status_update(text_lower) {
        return (
            MessageImportance::Low,
            "Assistant status update".to_string(),
        );
    }

    // Very short trivial responses.
    if trimmed.len() < SHORT_MESSAGE_THRESHOLD && is_trivial_response(text_lower) {
        return (
            MessageImportance::Trivial,
            "Trivial assistant response".to_string(),
        );
    }

    // Substantive assistant responses.
    (
        MessageImportance::Medium,
        "Substantive assistant response".to_string(),
    )
}

/// Classify tool result text (when encoded as a plain text message with Role::Tool).
fn classify_tool_text(text_lower: &str, trimmed: &str) -> (MessageImportance, String) {
    // Error results are important.
    if contains_error_pattern(text_lower) {
        return (
            MessageImportance::High,
            "Tool result with error".to_string(),
        );
    }

    // Empty or trivial results.
    if trimmed.is_empty() || is_trivial_tool_result(text_lower, trimmed) {
        return (MessageImportance::Low, "Trivial tool result".to_string());
    }

    // Default: regular tool result.
    (MessageImportance::Medium, "Regular tool result".to_string())
}

/// Classify a message composed of content parts.
fn classify_parts_message(role: &Role, parts: &[ContentPart]) -> (MessageImportance, String) {
    let mut max_importance = MessageImportance::Trivial;
    let mut reason = String::new();

    for part in parts {
        let (imp, r) = classify_content_part(role, part);
        if imp > max_importance {
            max_importance = imp;
            reason = r;
        }
    }

    // If we found nothing, default conservatively.
    if reason.is_empty() {
        reason = "Message with content parts".to_string();
        max_importance = MessageImportance::Medium;
    }

    (max_importance, reason)
}

/// Classify a single content part.
fn classify_content_part(role: &Role, part: &ContentPart) -> (MessageImportance, String) {
    match part {
        ContentPart::Text { text } => {
            let text_lower = text.to_lowercase();
            let trimmed = text.trim();
            match role {
                Role::User => classify_user_text(&text_lower, trimmed),
                Role::Assistant => classify_assistant_text(&text_lower, trimmed),
                Role::System => (MessageImportance::Critical, "System text part".to_string()),
                Role::Tool => classify_tool_text(&text_lower, trimmed),
            }
        }
        ContentPart::ToolUse { name, .. } => {
            (MessageImportance::Medium, format!("Tool use: {name}"))
        }
        ContentPart::ToolResult {
            is_error, content, ..
        } => {
            if *is_error {
                (
                    MessageImportance::High,
                    "Tool result indicating error/failure".to_string(),
                )
            } else if content.trim().is_empty()
                || is_trivial_tool_result(&content.to_lowercase(), content.trim())
            {
                (MessageImportance::Low, "Trivial tool result".to_string())
            } else {
                (
                    MessageImportance::Medium,
                    "Successful tool result".to_string(),
                )
            }
        }
        ContentPart::Image { .. } => (MessageImportance::High, "Image content".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Pattern detectors
// ---------------------------------------------------------------------------

/// Check for explicit importance markers in text.
fn contains_importance_marker(text_lower: &str) -> bool {
    const MARKERS: &[&str] = &[
        "decision:",
        "important:",
        "remember:",
        "critical:",
        "must:",
        "requirement:",
        "constraint:",
    ];
    MARKERS.iter().any(|m| text_lower.contains(m))
}

/// Check for instruction / decision patterns.
fn contains_instruction_pattern(text_lower: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "please do",
        "please make",
        "you must",
        "you should",
        "do not ",
        "don't ",
        "make sure",
        "always ",
        "never ",
        "deploy ",
        "configure ",
        "install ",
        "set up",
        "change the",
        "update the",
        "fix the",
        "create a",
        "delete the",
        "remove the",
        "implement",
        "refactor",
    ];
    PATTERNS.iter().any(|p| text_lower.contains(p))
}

/// Check for error patterns.
fn contains_error_pattern(text_lower: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "error",
        "failed",
        "failure",
        "exception",
        "panic",
        "crash",
        "traceback",
        "stack trace",
        "errno",
        "segfault",
        "permission denied",
        "not found",
        "timed out",
        "timeout",
    ];
    PATTERNS.iter().any(|p| text_lower.contains(p))
}

/// Check for code blocks (fenced with triple backticks).
fn contains_code_block(text: &str) -> bool {
    text.contains("```")
}

/// Check for structured data patterns (JSON objects/arrays, YAML-like).
fn contains_structured_data(text: &str) -> bool {
    let trimmed = text.trim();
    // JSON object or array
    (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        // YAML-like key-value pairs (at least two lines with "key: value")
        || {
            let kv_lines = trimmed
                .lines()
                .filter(|l| {
                    let l = l.trim();
                    l.contains(": ") && !l.starts_with('#') && l.len() > 3
                })
                .count();
            kv_lines >= 3
        }
}

/// Check if text is a greeting or acknowledgment.
fn is_greeting_or_ack(text_lower: &str, trimmed: &str) -> bool {
    const GREETINGS: &[&str] = &[
        "hello",
        "hi",
        "hey",
        "thanks",
        "thank you",
        "thx",
        "ok",
        "okay",
        "sure",
        "yes",
        "no",
        "yep",
        "nope",
        "cool",
        "great",
        "nice",
        "good",
        "got it",
        "understood",
        "bye",
        "goodbye",
        "see you",
        "cheers",
    ];

    // Must be short enough to be a pure greeting (not a sentence that happens
    // to start with "hi").
    if trimmed.len() > 40 {
        return false;
    }

    // Strip trailing punctuation/emoji for matching.
    let cleaned: String = text_lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
        .collect();
    let cleaned = cleaned.trim();

    GREETINGS.contains(&cleaned)
}

/// Check if text looks like a command (starts with / or !).
fn looks_like_command(text_lower: &str) -> bool {
    let trimmed = text_lower.trim();
    trimmed.starts_with('/') || trimmed.starts_with('!')
}

/// Check if assistant text is a status update.
fn is_status_update(text_lower: &str) -> bool {
    const STATUS_PATTERNS: &[&str] = &[
        "running",
        "checking",
        "processing",
        "loading",
        "waiting",
        "searching",
        "fetching",
        "downloading",
        "uploading",
        "installing",
        "building",
        "compiling",
        "starting",
        "connecting",
    ];

    let trimmed = text_lower.trim();
    // Must be short — a full paragraph that mentions "running" is substantive.
    if trimmed.len() > 80 {
        return false;
    }

    STATUS_PATTERNS
        .iter()
        .any(|p| trimmed.starts_with(p) || trimmed.ends_with("...") && trimmed.contains(p))
}

/// Check if assistant text is a trivial response.
fn is_trivial_response(text_lower: &str) -> bool {
    const TRIVIAL: &[&str] = &[
        "ok",
        "okay",
        "sure",
        "done",
        "got it",
        "understood",
        "alright",
        "noted",
        "will do",
        "on it",
    ];
    let cleaned: String = text_lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
        .collect();
    let cleaned = cleaned.trim();
    TRIVIAL.contains(&cleaned)
}

/// Check if a tool result is trivial (empty, "ok", etc.).
fn is_trivial_tool_result(text_lower: &str, trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return true;
    }
    const TRIVIAL_RESULTS: &[&str] = &["ok", "okay", "success", "done", "true", "null", "none"];
    TRIVIAL_RESULTS
        .iter()
        .any(|t| trimmed == *t || text_lower == *t)
}

// ---------------------------------------------------------------------------
// Pruning
// ---------------------------------------------------------------------------

/// Prune a conversation history to at most `max_messages`, keeping the most
/// semantically important messages and summarizing what was dropped.
///
/// # Algorithm
///
/// 1. Score every message.
/// 2. Sort by score ascending (lowest importance first).
/// 3. Mark the lowest-scored messages for removal until we are within budget.
/// 4. Reconstruct the kept messages in their original order.
/// 5. Generate a summary of what was pruned.
/// 6. User messages are never silently dropped — they are always included
///    in the summary.
///
/// # Arguments
///
/// * `history` — The full conversation history to prune.
/// * `max_messages` — Maximum number of messages to keep.
///
/// # Returns
///
/// A [`PrunedHistory`] containing the kept messages, a summary, and the
/// number of dropped messages.
pub fn prune_history(history: &[ChatMessage], max_messages: usize) -> PrunedHistory {
    let total = history.len();

    // Nothing to prune.
    if total <= max_messages {
        return PrunedHistory {
            kept_messages: history.to_vec(),
            summary: String::new(),
            dropped_count: 0,
        };
    }

    // Group messages into atomic conversation turns so tool_use/tool_result
    // pairs are never split during pruning.
    let turns = group_into_turns(history);

    // Score each turn by the maximum score of its constituent messages.
    // A turn is only as droppable as its most important member.
    let mut scored_turns: Vec<(usize, f32)> = turns
        .iter()
        .enumerate()
        .map(|(turn_idx, turn)| {
            let max_score = turn
                .indices
                .iter()
                .map(|&msg_idx| score_message(&history[msg_idx], msg_idx, total).score)
                .fold(f32::NEG_INFINITY, f32::max);
            (turn_idx, max_score)
        })
        .collect();

    // Sort by score ascending so we drop lowest-scored turns first.
    scored_turns.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    // Drop lowest-scored turns until we've removed enough messages.
    let msgs_to_drop = total - max_messages;
    let mut drop_msg_count = 0;
    let mut drop_turn_indices: Vec<usize> = Vec::new();

    for &(turn_idx, _score) in &scored_turns {
        if drop_msg_count >= msgs_to_drop {
            break;
        }
        drop_turn_indices.push(turn_idx);
        drop_msg_count += turns[turn_idx].indices.len();
    }

    // Collect all message indices to drop.
    let drop_msg_indices: HashSet<usize> = drop_turn_indices
        .iter()
        .flat_map(|&turn_idx| turns[turn_idx].indices.iter().copied())
        .collect();

    // Collect dropped messages for summary generation.
    let mut sorted_drop_indices: Vec<usize> = drop_msg_indices.iter().copied().collect();
    sorted_drop_indices.sort_unstable();
    let dropped_msgs: Vec<&ChatMessage> =
        sorted_drop_indices.iter().map(|&i| &history[i]).collect();

    // Build the summary.
    let summary = generate_pruned_summary(&dropped_msgs);

    // Build the kept list in original order.
    let kept_messages: Vec<ChatMessage> = history
        .iter()
        .enumerate()
        .filter(|(i, _)| !drop_msg_indices.contains(i))
        .map(|(_, msg)| msg.clone())
        .collect();

    let actual_dropped = drop_msg_indices.len();

    debug!(
        total,
        kept = kept_messages.len(),
        dropped = actual_dropped,
        turns = turns.len(),
        "Pruned conversation history by semantic importance (turn-aware)"
    );

    PrunedHistory {
        kept_messages,
        summary,
        dropped_count: actual_dropped,
    }
}

/// Generate a brief summary of pruned messages.
///
/// The summary captures:
/// - Topics discussed by the user
/// - Tools that were used
/// - Key outcomes (errors, completions)
///
/// User messages are always represented in the summary so the model does
/// not lose track of what the user asked for.
pub fn generate_pruned_summary(dropped: &[&ChatMessage]) -> String {
    if dropped.is_empty() {
        return String::new();
    }

    let mut user_topics: Vec<String> = Vec::new();
    let mut tools_used: Vec<String> = Vec::new();
    let mut had_errors = false;
    let mut had_tool_results = false;

    for msg in dropped {
        match &msg.content {
            MessageContent::Text(text) => {
                if matches!(msg.role, Role::User) && text.trim().len() > 3 {
                    let topic = if text.len() > 60 {
                        let end = text
                            .char_indices()
                            .map(|(i, _)| i)
                            .take_while(|&i| i <= 57)
                            .last()
                            .unwrap_or(0);
                        format!("{}...", &text[..end])
                    } else {
                        text.clone()
                    };
                    user_topics.push(topic);
                }
                if contains_error_pattern(&text.to_lowercase()) {
                    had_errors = true;
                }
            }
            MessageContent::Parts(parts) => {
                for part in parts {
                    match part {
                        ContentPart::ToolUse { name, .. } => {
                            if !tools_used.contains(name) {
                                tools_used.push(name.clone());
                            }
                        }
                        ContentPart::ToolResult { is_error, .. } => {
                            had_tool_results = true;
                            if *is_error {
                                had_errors = true;
                            }
                        }
                        ContentPart::Text { text } => {
                            if contains_error_pattern(&text.to_lowercase()) {
                                had_errors = true;
                            }
                        }
                        ContentPart::Image { .. } => {}
                    }
                }
            }
        }
    }

    let mut parts = Vec::new();

    if !user_topics.is_empty() {
        let topics_str = user_topics
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        parts.push(format!("discussed: {topics_str}"));
    }

    if !tools_used.is_empty() {
        parts.push(format!("used tools: {}", tools_used.join(", ")));
    }

    let mut outcomes = Vec::new();
    if had_errors {
        outcomes.push("encountered errors");
    }
    if had_tool_results {
        outcomes.push("received tool results");
    }
    if !outcomes.is_empty() {
        parts.push(format!("key outcomes: {}", outcomes.join(", ")));
    }

    if parts.is_empty() {
        format!(
            "Earlier context: {} messages were pruned (low importance).",
            dropped.len()
        )
    } else {
        format!(
            "Earlier context ({} messages pruned): {}.",
            dropped.len(),
            parts.join("; ")
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- Helpers -----------------------------------------------------------

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn assistant_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn system_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::System,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn tool_use_msg(name: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: "tu-1".to_string(),
                name: name.to_string(),
                input: json!({"command": "ls"}),
            }]),
        }
    }

    fn tool_result_msg(content: &str, is_error: bool) -> ChatMessage {
        ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: "tu-1".to_string(),
                content: content.to_string(),
                is_error,
            }]),
        }
    }

    // -- Importance level tests --------------------------------------------

    #[test]
    fn critical_user_messages_with_markers() {
        let markers = [
            "decision: we'll use PostgreSQL",
            "important: do not change the API",
            "remember: the port is 5432",
            "critical: this must be done before release",
        ];
        for text in &markers {
            let scored = score_message(&user_msg(text), 0, 10);
            assert_eq!(
                scored.importance,
                MessageImportance::Critical,
                "Expected Critical for: {text}"
            );
        }
    }

    #[test]
    fn critical_user_messages_with_instructions() {
        let instructions = [
            "please do the migration first",
            "you must restart the server after deploy",
            "make sure the tests pass",
            "implement the new feature",
        ];
        for text in &instructions {
            let scored = score_message(&user_msg(text), 0, 10);
            assert_eq!(
                scored.importance,
                MessageImportance::Critical,
                "Expected Critical for: {text}"
            );
        }
    }

    #[test]
    fn critical_system_messages() {
        let scored = score_message(&system_msg("You are a helpful assistant"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::Critical);
    }

    #[test]
    fn high_messages_with_code_blocks() {
        let code_msg = user_msg("Here's the config:\n```toml\n[server]\nport = 8080\n```");
        let scored = score_message(&code_msg, 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn high_messages_with_structured_data() {
        let json_msg = user_msg("{\"key\": \"value\", \"nested\": {\"a\": 1}}");
        let scored = score_message(&json_msg, 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn high_tool_error_results() {
        let scored = score_message(&tool_result_msg("Error: command not found", true), 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn high_assistant_error_report() {
        let scored = score_message(&assistant_msg("The command failed with exit code 1"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn medium_regular_user_message() {
        let scored = score_message(&user_msg("What is the status of the deployment?"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::Medium);
    }

    #[test]
    fn medium_substantive_assistant_response() {
        let scored = score_message(
            &assistant_msg("The deployment is running on port 8080 and all health checks pass."),
            0,
            10,
        );
        assert_eq!(scored.importance, MessageImportance::Medium);
    }

    #[test]
    fn medium_tool_use_message() {
        let scored = score_message(&tool_use_msg("shell"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::Medium);
    }

    #[test]
    fn medium_successful_tool_result() {
        let scored = score_message(
            &tool_result_msg("file1.txt\nfile2.txt\nfile3.txt", false),
            0,
            10,
        );
        assert_eq!(scored.importance, MessageImportance::Medium);
    }

    #[test]
    fn low_status_updates() {
        let status_msgs = [
            "Running the tests...",
            "Checking configuration...",
            "Processing your request...",
        ];
        for text in &status_msgs {
            let scored = score_message(&assistant_msg(text), 0, 10);
            assert_eq!(
                scored.importance,
                MessageImportance::Low,
                "Expected Low for: {text}"
            );
        }
    }

    #[test]
    fn low_trivial_tool_results() {
        let trivial_results = ["ok", "success", "done", "true"];
        for text in &trivial_results {
            let scored = score_message(&tool_result_msg(text, false), 0, 10);
            assert_eq!(
                scored.importance,
                MessageImportance::Low,
                "Expected Low for tool result: {text}"
            );
        }
    }

    #[test]
    fn low_short_user_message() {
        // Short messages that aren't commands or greetings
        let scored = score_message(&user_msg("test"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::Low);
    }

    #[test]
    fn trivial_greetings() {
        let greetings = ["hello", "hi", "hey", "thanks", "bye", "ok"];
        for text in &greetings {
            let scored = score_message(&user_msg(text), 0, 10);
            assert_eq!(
                scored.importance,
                MessageImportance::Trivial,
                "Expected Trivial for: {text}"
            );
        }
    }

    #[test]
    fn trivial_assistant_acknowledgments() {
        let scored = score_message(&assistant_msg("ok"), 0, 10);
        assert_eq!(scored.importance, MessageImportance::Trivial);
    }

    // -- Position bonus tests ----------------------------------------------

    #[test]
    fn position_bonus_recent_messages() {
        let total = 10;
        // Last 4 messages (indices 6..10) should get the bonus.
        for i in 6..10 {
            let scored = score_message(&user_msg("hello"), i, total);
            assert!(
                scored.score >= MessageImportance::Trivial.base_score() + RECENCY_BONUS,
                "Index {i} should have position bonus"
            );
        }
        // Index 5 should NOT get the bonus.
        let scored = score_message(&user_msg("hello"), 5, total);
        assert!(
            scored.score < MessageImportance::Trivial.base_score() + RECENCY_BONUS,
            "Index 5 should not have position bonus"
        );
    }

    #[test]
    fn position_bonus_small_history() {
        // When total <= RECENCY_WINDOW, all messages get the bonus.
        let total = 3;
        for i in 0..3 {
            let scored = score_message(&user_msg("hello"), i, total);
            assert!(
                scored.score >= MessageImportance::Trivial.base_score() + RECENCY_BONUS,
                "Index {i} in small history should have position bonus"
            );
        }
    }

    // -- Pruning tests -----------------------------------------------------

    #[test]
    fn prune_no_op_when_under_limit() {
        let history = vec![user_msg("Hello"), assistant_msg("Hi there")];
        let pruned = prune_history(&history, 10);
        assert_eq!(pruned.kept_messages.len(), 2);
        assert_eq!(pruned.dropped_count, 0);
        assert!(pruned.summary.is_empty());
    }

    #[test]
    fn prune_preserves_high_importance_messages() {
        let history = vec![
            user_msg("hello"),                                     // Trivial
            assistant_msg("ok"),                                   // Trivial
            user_msg("decision: use PostgreSQL for the database"), // Critical
            assistant_msg("Running tests..."),                     // Low
            user_msg("thanks"),                                    // Trivial
            assistant_msg("The command failed with an error"),     // High
            user_msg("What happened?"),                            // Medium
            assistant_msg("Here's the result with details."),      // Medium
        ];

        let pruned = prune_history(&history, 4);

        // Should have dropped 4 messages.
        assert_eq!(pruned.dropped_count, 4);
        assert_eq!(pruned.kept_messages.len(), 4);

        // The critical "decision:" message should be kept.
        let has_decision = pruned.kept_messages.iter().any(|m| {
            if let MessageContent::Text(t) = &m.content {
                t.contains("decision:")
            } else {
                false
            }
        });
        assert!(has_decision, "Critical decision message should be kept");

        // The error message should be kept (High importance + recency bonus).
        let has_error = pruned.kept_messages.iter().any(|m| {
            if let MessageContent::Text(t) = &m.content {
                t.contains("failed")
            } else {
                false
            }
        });
        assert!(has_error, "Error message should be kept");
    }

    #[test]
    fn prune_maintains_original_order() {
        let history = vec![
            user_msg("first instruction: deploy the app"),
            assistant_msg("ok"),
            user_msg("hello"),
            assistant_msg("Running check..."),
            user_msg("thanks"),
            user_msg("second instruction: implement the API"),
        ];

        let pruned = prune_history(&history, 3);

        // Check that remaining messages maintain their original relative order.
        let texts: Vec<String> = pruned
            .kept_messages
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect();

        // Verify order is maintained (each subsequent message's original index
        // should be greater than the previous).
        for window in texts.windows(2) {
            let first_original = history.iter().position(|m| {
                if let MessageContent::Text(t) = &m.content {
                    t == &window[0]
                } else {
                    false
                }
            });
            let second_original = history.iter().position(|m| {
                if let MessageContent::Text(t) = &m.content {
                    t == &window[1]
                } else {
                    false
                }
            });
            assert!(
                first_original < second_original,
                "Messages should maintain original order"
            );
        }
    }

    #[test]
    fn prune_exact_limit() {
        let history = vec![user_msg("a"), user_msg("b"), user_msg("c")];
        let pruned = prune_history(&history, 3);
        assert_eq!(pruned.kept_messages.len(), 3);
        assert_eq!(pruned.dropped_count, 0);
    }

    #[test]
    fn prune_to_one() {
        let history = vec![
            user_msg("hello"),
            assistant_msg("hi"),
            user_msg("decision: important choice"),
        ];

        let pruned = prune_history(&history, 1);
        assert_eq!(pruned.kept_messages.len(), 1);
        assert_eq!(pruned.dropped_count, 2);

        // The critical decision message should survive (highest base score + recency).
        match &pruned.kept_messages[0].content {
            MessageContent::Text(t) => assert!(t.contains("decision:")),
            _ => panic!("Expected text message"),
        }
    }

    // -- Summary generation tests ------------------------------------------

    #[test]
    fn summary_empty_when_no_drops() {
        let dropped: Vec<&ChatMessage> = vec![];
        let summary = generate_pruned_summary(&dropped);
        assert!(summary.is_empty());
    }

    #[test]
    fn summary_includes_user_topics() {
        let msgs = [user_msg("Deploy the application to staging")];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        assert!(summary.contains("Deploy the application"));
    }

    #[test]
    fn summary_includes_tools_used() {
        let msgs = [tool_use_msg("shell")];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        assert!(summary.contains("shell"));
    }

    #[test]
    fn summary_includes_error_outcome() {
        let msgs = [tool_result_msg("Error: connection refused", true)];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        assert!(summary.contains("encountered errors"));
    }

    #[test]
    fn summary_truncates_long_topics() {
        let long_text = "a".repeat(100);
        let msgs = [user_msg(&long_text)];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        // Should be truncated to ~60 chars with "..."
        assert!(summary.contains("..."));
    }

    #[test]
    fn summary_limits_topics_to_three() {
        let msgs = [
            user_msg("Topic one is about deployment"),
            user_msg("Topic two is about databases"),
            user_msg("Topic three is about security"),
            user_msg("Topic four is about monitoring"),
        ];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        // Should include at most 3 topics.
        assert!(summary.contains("Topic one"));
        assert!(summary.contains("Topic three"));
        assert!(!summary.contains("Topic four"));
    }

    #[test]
    fn summary_for_only_trivial_drops() {
        let msgs = [assistant_msg("ok"), assistant_msg("sure")];
        let refs: Vec<&ChatMessage> = msgs.iter().collect();
        let summary = generate_pruned_summary(&refs);
        assert!(
            summary.contains("2 messages were pruned"),
            "Summary should mention dropped count, got: {summary}"
        );
    }

    // -- Edge cases --------------------------------------------------------

    #[test]
    fn prune_empty_history() {
        let pruned = prune_history(&[], 5);
        assert!(pruned.kept_messages.is_empty());
        assert_eq!(pruned.dropped_count, 0);
        assert!(pruned.summary.is_empty());
    }

    #[test]
    fn prune_all_same_importance() {
        // When all messages have the same importance, recent ones should be
        // preferred due to the position bonus.
        let history: Vec<ChatMessage> = (0..10)
            .map(|i| {
                user_msg(&format!(
                    "Regular message number {i} with enough length to be medium"
                ))
            })
            .collect();

        let pruned = prune_history(&history, 5);
        assert_eq!(pruned.kept_messages.len(), 5);

        // The last 4 messages get a recency bonus, so they should be kept.
        let last_kept = &pruned.kept_messages[pruned.kept_messages.len() - 1];
        match &last_kept.content {
            MessageContent::Text(t) => assert!(t.contains("number 9")),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn score_message_zero_total() {
        // Should not panic with total = 0.
        let scored = score_message(&user_msg("test"), 0, 0);
        assert!(scored.score > 0.0);
    }

    #[test]
    fn importance_ordering() {
        assert!(MessageImportance::Trivial < MessageImportance::Low);
        assert!(MessageImportance::Low < MessageImportance::Medium);
        assert!(MessageImportance::Medium < MessageImportance::High);
        assert!(MessageImportance::High < MessageImportance::Critical);
    }

    #[test]
    fn serde_roundtrip_scored_message() {
        let scored = ScoredMessage {
            index: 5,
            importance: MessageImportance::High,
            score: 10.0,
            reason: "Test reason".to_string(),
        };
        let json = serde_json::to_string(&scored).unwrap();
        let restored: ScoredMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.index, 5);
        assert_eq!(restored.importance, MessageImportance::High);
        assert!((restored.score - 10.0).abs() < f32::EPSILON);
        assert_eq!(restored.reason, "Test reason");
    }

    #[test]
    fn serde_roundtrip_pruned_history() {
        let pruned = PrunedHistory {
            kept_messages: vec![user_msg("kept")],
            summary: "Earlier context: 3 messages pruned.".to_string(),
            dropped_count: 3,
        };
        let json = serde_json::to_string(&pruned).unwrap();
        let restored: PrunedHistory = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.kept_messages.len(), 1);
        assert_eq!(restored.dropped_count, 3);
    }

    #[test]
    fn serde_roundtrip_message_importance() {
        let levels = [
            MessageImportance::Trivial,
            MessageImportance::Low,
            MessageImportance::Medium,
            MessageImportance::High,
            MessageImportance::Critical,
        ];
        for level in &levels {
            let json = serde_json::to_string(level).unwrap();
            let restored: MessageImportance = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, restored);
        }
    }

    #[test]
    fn conservative_scoring_ambiguous_messages() {
        // A longer message that might be ambiguous should default to Medium,
        // not Low or Trivial — conservative approach.
        let scored = score_message(
            &user_msg("Can you check if the server is responding properly on port 3000?"),
            0,
            10,
        );
        assert!(
            scored.importance >= MessageImportance::Medium,
            "Ambiguous user messages should be at least Medium"
        );
    }

    #[test]
    fn tool_result_with_error_flag_is_high() {
        // Even if the content looks benign, the is_error flag makes it High.
        let scored = score_message(&tool_result_msg("ok", true), 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn parts_message_uses_highest_part_importance() {
        // A message with mixed parts should use the highest importance among them.
        let msg = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Running...".to_string(),
                },
                ContentPart::ToolUse {
                    id: "tu-1".to_string(),
                    name: "shell".to_string(),
                    input: json!({}),
                },
                ContentPart::ToolResult {
                    tool_use_id: "tu-1".to_string(),
                    content: "Error: permission denied".to_string(),
                    is_error: true,
                },
            ]),
        };
        let scored = score_message(&msg, 0, 10);
        assert_eq!(scored.importance, MessageImportance::High);
    }

    #[test]
    fn prune_history_with_mixed_importance_keeps_critical() {
        let history = vec![
            user_msg("hi"),                                     // Trivial
            assistant_msg("Hello!"),                            // Medium-ish
            user_msg("ok"),                                     // Trivial
            assistant_msg("Running..."),                        // Low
            tool_result_msg("ok", false),                       // Low
            user_msg("requirement: all APIs must return JSON"), // Critical
            assistant_msg("I'll make sure of that."),           // Medium
            user_msg("What's next?"),                           // Medium
        ];

        let pruned = prune_history(&history, 3);

        // The critical requirement message must survive.
        let has_requirement = pruned.kept_messages.iter().any(|m| {
            if let MessageContent::Text(t) = &m.content {
                t.contains("requirement:")
            } else {
                false
            }
        });
        assert!(has_requirement, "Critical requirement message must be kept");
    }

    #[test]
    fn long_greeting_not_classified_as_trivial() {
        // "hi" in a longer message should not be trivial.
        let scored = score_message(
            &user_msg("Hi, I need you to deploy the staging environment with the new config"),
            0,
            10,
        );
        assert!(
            scored.importance > MessageImportance::Trivial,
            "Long message starting with 'Hi' should not be trivial"
        );
    }

    // -- Turn grouping tests --------------------------------------------------

    fn tool_use_msg_with_id(name: &str, id: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: json!({"command": "ls"}),
            }]),
        }
    }

    fn tool_result_msg_with_id(id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: false,
            }]),
        }
    }

    #[test]
    fn group_into_turns_no_tools() {
        let history = vec![user_msg("Hello"), assistant_msg("Hi")];
        let turns = group_into_turns(&history);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].indices, vec![0]);
        assert_eq!(turns[1].indices, vec![1]);
    }

    #[test]
    fn group_into_turns_pairs_tool_use_with_result() {
        let history = vec![
            user_msg("Run ls"),                              // 0
            tool_use_msg_with_id("shell", "tu-1"),           // 1
            tool_result_msg_with_id("tu-1", "file1\nfile2"), // 2
            assistant_msg("Here are your files"),            // 3
        ];
        let turns = group_into_turns(&history);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].indices, vec![0]); // user msg alone
        assert_eq!(turns[1].indices, vec![1, 2]); // tool_use + tool_result grouped
        assert_eq!(turns[2].indices, vec![3]); // assistant text alone
    }

    #[test]
    fn group_into_turns_multiple_tool_uses() {
        let history = vec![
            user_msg("Do two things"),                        // 0
            tool_use_msg_with_id("shell", "tu-1"),            // 1
            tool_result_msg_with_id("tu-1", "result1"),       // 2
            tool_use_msg_with_id("file_read", "tu-2"),        // 3
            tool_result_msg_with_id("tu-2", "file contents"), // 4
            assistant_msg("Done with both"),                  // 5
        ];
        let turns = group_into_turns(&history);
        assert_eq!(turns.len(), 4);
        assert_eq!(turns[0].indices, vec![0]); // user msg
        assert_eq!(turns[1].indices, vec![1, 2]); // first tool pair
        assert_eq!(turns[2].indices, vec![3, 4]); // second tool pair
        assert_eq!(turns[3].indices, vec![5]); // assistant text
    }

    #[test]
    fn group_into_turns_multi_tool_use_in_single_message() {
        // An assistant message with two tool_use parts
        let msg = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::ToolUse {
                    id: "tu-a".to_string(),
                    name: "shell".to_string(),
                    input: json!({"command": "ls"}),
                },
                ContentPart::ToolUse {
                    id: "tu-b".to_string(),
                    name: "file_read".to_string(),
                    input: json!({"path": "test.txt"}),
                },
            ]),
        };
        let history = vec![
            user_msg("Do things"),                        // 0
            msg,                                          // 1 (two tool_uses)
            tool_result_msg_with_id("tu-a", "ls output"), // 2
            tool_result_msg_with_id("tu-b", "file text"), // 3
            assistant_msg("All done"),                    // 4
        ];
        let turns = group_into_turns(&history);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].indices, vec![0]); // user msg
        assert_eq!(turns[1].indices, vec![1, 2, 3]); // tool_use + both results
        assert_eq!(turns[2].indices, vec![4]); // assistant text
    }

    #[test]
    fn group_into_turns_empty() {
        let turns = group_into_turns(&[]);
        assert!(turns.is_empty());
    }

    // -- Orphan prevention in pruning tests -----------------------------------

    #[test]
    fn prune_never_orphans_tool_result() {
        // Build a history where a tool_use+result pair is low importance
        // but must be kept/dropped together.
        let history = vec![
            user_msg("hello"),                                   // 0: Trivial
            tool_use_msg_with_id("shell", "tu-1"),               // 1: Medium (tool use)
            tool_result_msg_with_id("tu-1", "ok"),               // 2: Low (trivial result)
            user_msg("decision: use PostgreSQL"),                // 3: Critical
            assistant_msg("The deployment is done and working"), // 4: Medium + recency
        ];

        let pruned = prune_history(&history, 3);

        // Check: if any tool_result is present, its matching tool_use must also be present
        let kept_tool_use_ids: Vec<String> = pruned
            .kept_messages
            .iter()
            .flat_map(extract_tool_use_ids)
            .collect();
        let kept_tool_result_ids: Vec<String> = pruned
            .kept_messages
            .iter()
            .flat_map(extract_tool_result_ids)
            .collect();

        for result_id in &kept_tool_result_ids {
            assert!(
                kept_tool_use_ids.contains(result_id),
                "Orphaned tool_result '{result_id}' — its tool_use was dropped!"
            );
        }
    }

    #[test]
    fn prune_drops_tool_pair_together() {
        // Ensure when a tool pair is dropped, BOTH messages go
        let history = vec![
            user_msg("hello"),                                    // 0: Trivial
            assistant_msg("ok"),                                  // 1: Trivial
            tool_use_msg_with_id("shell", "tu-1"),                // 2: Medium
            tool_result_msg_with_id("tu-1", "ok"),                // 3: Low
            user_msg("decision: critical requirement here"),      // 4: Critical + recency
            assistant_msg("Understood, I will follow that rule"), // 5: Medium + recency
        ];

        let pruned = prune_history(&history, 3);

        // The tool pair should be dropped together or kept together
        let has_tool_use = pruned
            .kept_messages
            .iter()
            .any(|m| !extract_tool_use_ids(m).is_empty());
        let has_tool_result = pruned
            .kept_messages
            .iter()
            .any(|m| !extract_tool_result_ids(m).is_empty());

        // Either both present or both absent
        assert_eq!(
            has_tool_use, has_tool_result,
            "Tool use and tool result must be kept/dropped together"
        );
    }

    // -- remove_orphaned_tool_results tests -----------------------------------

    #[test]
    fn remove_orphaned_tool_results_removes_orphans() {
        let mut messages = vec![
            user_msg("Hello"),
            // This tool_result has no matching tool_use — orphaned
            tool_result_msg_with_id("tu-missing", "some output"),
            assistant_msg("Done"),
        ];

        remove_orphaned_tool_results(&mut messages);
        assert_eq!(messages.len(), 2); // orphan removed
                                       // Verify the remaining messages are correct
        assert!(matches!(messages[0].role, Role::User));
        assert!(matches!(messages[1].role, Role::Assistant));
    }

    #[test]
    fn remove_orphaned_tool_results_keeps_matched() {
        let mut messages = vec![
            user_msg("Run something"),
            tool_use_msg_with_id("shell", "tu-1"),
            tool_result_msg_with_id("tu-1", "output"),
            assistant_msg("Done"),
        ];

        remove_orphaned_tool_results(&mut messages);
        assert_eq!(messages.len(), 4); // nothing removed
    }

    #[test]
    fn remove_orphaned_tool_results_empty() {
        let mut messages: Vec<ChatMessage> = vec![];
        remove_orphaned_tool_results(&mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn remove_orphaned_tool_results_mixed_matched_and_orphaned() {
        let mut messages = vec![
            tool_use_msg_with_id("shell", "tu-1"),
            tool_result_msg_with_id("tu-1", "good output"), // matched
            tool_result_msg_with_id("tu-gone", "bad output"), // orphaned
            assistant_msg("Done"),
        ];

        remove_orphaned_tool_results(&mut messages);
        assert_eq!(messages.len(), 3); // orphan removed
    }
}
