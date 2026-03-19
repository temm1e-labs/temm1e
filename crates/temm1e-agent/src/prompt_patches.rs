//! Adaptive System Prompt — Self-Tuning Agent (Roadmap 6.4)
//!
//! After task completion, the agent evaluates what instruction would have made
//! the task easier. Proposed prompt modifications are stored as "prompt patches"
//! in memory. On the next session, relevant patches are injected into the system
//! prompt. Users can review and approve/reject patches.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{ChatMessage, ContentPart, MessageContent, Role};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// What kind of prompt modification a patch represents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatchType {
    /// "Prefer X tool for Y tasks"
    ToolUsageHint,
    /// "When doing X, avoid Y because Z"
    ErrorAvoidance,
    /// "For tasks involving X, follow this sequence: ..."
    WorkflowPattern,
    /// "In this workspace/project, X means Y"
    DomainKnowledge,
    /// "User prefers X format/style"
    StylePreference,
}

impl std::fmt::Display for PatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchType::ToolUsageHint => write!(f, "Tool Usage Hint"),
            PatchType::ErrorAvoidance => write!(f, "Error Avoidance"),
            PatchType::WorkflowPattern => write!(f, "Workflow Pattern"),
            PatchType::DomainKnowledge => write!(f, "Domain Knowledge"),
            PatchType::StylePreference => write!(f, "Style Preference"),
        }
    }
}

/// Lifecycle status of a prompt patch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatchStatus {
    /// Newly created, awaiting user review.
    Proposed,
    /// Approved by user (or auto-approved); will be injected into prompts.
    Approved,
    /// Rejected by user; will not be injected.
    Rejected,
    /// Expired due to underperformance or staleness.
    Expired,
}

impl std::fmt::Display for PatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchStatus::Proposed => write!(f, "Proposed"),
            PatchStatus::Approved => write!(f, "Approved"),
            PatchStatus::Rejected => write!(f, "Rejected"),
            PatchStatus::Expired => write!(f, "Expired"),
        }
    }
}

/// A single prompt patch — a proposed modification to the system prompt based
/// on lessons learned from a completed task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPatch {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// What kind of modification this patch represents.
    pub patch_type: PatchType,
    /// The actual prompt text to inject into the system prompt.
    pub content: String,
    /// Description of the task that generated this patch.
    pub source_task: String,
    /// Confidence score (0.0–1.0) — how confident the system is that this
    /// patch improves outcomes.
    pub confidence: f32,
    /// Current lifecycle status.
    pub status: PatchStatus,
    /// When this patch was created.
    pub created_at: DateTime<Utc>,
    /// How many times this patch has been applied (injected into a prompt).
    pub applied_count: usize,
    /// Running success rate for tasks that had this patch active.
    pub success_rate: f32,
}

impl PromptPatch {
    /// Create a new prompt patch with `Proposed` status.
    pub fn new(
        patch_type: PatchType,
        content: String,
        source_task: String,
        confidence: f32,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            patch_type,
            content,
            source_task,
            confidence: confidence.clamp(0.0, 1.0),
            status: PatchStatus::Proposed,
            created_at: Utc::now(),
            applied_count: 0,
            success_rate: 0.0,
        }
    }

    /// Returns `true` if this patch type is considered low-risk for
    /// auto-approval purposes.
    fn is_low_risk(&self) -> bool {
        matches!(
            self.patch_type,
            PatchType::ToolUsageHint | PatchType::StylePreference
        )
    }
}

// ---------------------------------------------------------------------------
// PromptPatchManager
// ---------------------------------------------------------------------------

/// Manages the lifecycle of prompt patches: proposal, approval, injection,
/// outcome tracking, and expiration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPatchManager {
    /// All patches (any status).
    patches: Vec<PromptPatch>,
    /// Maximum number of patches to retain (prevents prompt bloat).
    pub max_patches: usize,
    /// Confidence threshold above which low-risk patches are auto-approved.
    pub auto_approve_threshold: f32,
}

impl Default for PromptPatchManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptPatchManager {
    /// Create a new manager with default limits.
    pub fn new() -> Self {
        Self {
            patches: Vec::new(),
            max_patches: 20,
            auto_approve_threshold: 0.8,
        }
    }

    /// Propose a new patch. If the patch is low-risk and above the
    /// auto-approve threshold, it is automatically approved. If the manager
    /// is at capacity, the lowest-confidence expired/rejected patch is evicted
    /// first.
    ///
    /// Returns the patch ID on success.
    pub fn propose_patch(&mut self, mut patch: PromptPatch) -> Result<String, Temm1eError> {
        if patch.content.is_empty() {
            return Err(Temm1eError::Config(
                "Prompt patch content cannot be empty".to_string(),
            ));
        }

        // Enforce capacity: evict lowest-confidence expired/rejected first.
        if self.patches.len() >= self.max_patches && !self.evict_one() {
            return Err(Temm1eError::Config(format!(
                "Prompt patch limit ({}) reached and no evictable patches found",
                self.max_patches
            )));
        }

        // Auto-approve low-risk patches above threshold.
        if patch.is_low_risk() && patch.confidence >= self.auto_approve_threshold {
            patch.status = PatchStatus::Approved;
            tracing::info!(
                id = %patch.id,
                patch_type = %patch.patch_type,
                confidence = %patch.confidence,
                "Auto-approved low-risk prompt patch"
            );
        } else {
            tracing::debug!(
                id = %patch.id,
                patch_type = %patch.patch_type,
                confidence = %patch.confidence,
                "Proposed prompt patch (manual approval required)"
            );
        }

        let id = patch.id.clone();
        self.patches.push(patch);
        Ok(id)
    }

    /// Approve a proposed patch by ID.
    pub fn approve_patch(&mut self, id: &str) -> Result<(), Temm1eError> {
        let patch = self
            .patches
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Temm1eError::NotFound(format!("Prompt patch not found: {id}")))?;

        if patch.status != PatchStatus::Proposed {
            return Err(Temm1eError::Config(format!(
                "Cannot approve patch with status '{}' (must be Proposed)",
                patch.status
            )));
        }

        patch.status = PatchStatus::Approved;
        tracing::info!(id = %id, "Approved prompt patch");
        Ok(())
    }

    /// Reject a proposed patch by ID.
    pub fn reject_patch(&mut self, id: &str) -> Result<(), Temm1eError> {
        let patch = self
            .patches
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Temm1eError::NotFound(format!("Prompt patch not found: {id}")))?;

        if patch.status != PatchStatus::Proposed {
            return Err(Temm1eError::Config(format!(
                "Cannot reject patch with status '{}' (must be Proposed)",
                patch.status
            )));
        }

        patch.status = PatchStatus::Rejected;
        tracing::info!(id = %id, "Rejected prompt patch");
        Ok(())
    }

    /// List patches, optionally filtering by status.
    pub fn list_patches(&self, status_filter: Option<PatchStatus>) -> Vec<&PromptPatch> {
        self.patches
            .iter()
            .filter(|p| status_filter.as_ref().is_none_or(|s| &p.status == s))
            .collect()
    }

    /// Return approved patches sorted by confidence (highest first).
    pub fn get_active_patches(&self) -> Vec<&PromptPatch> {
        let mut active: Vec<&PromptPatch> = self
            .patches
            .iter()
            .filter(|p| p.status == PatchStatus::Approved)
            .collect();
        active.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        active
    }

    /// Format all active (approved) patches as a system prompt section ready
    /// for injection.
    pub fn format_patches_for_prompt(&self) -> String {
        let active = self.get_active_patches();
        if active.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push("Learned prompt patches (apply where relevant):".to_string());

        for (i, patch) in active.iter().enumerate() {
            lines.push(format!(
                "  {}. [{}] {}",
                i + 1,
                patch.patch_type,
                patch.content
            ));
        }

        lines.join("\n")
    }

    /// Record the outcome of a task for all patches that were active during it.
    /// Updates `applied_count` and `success_rate` using a running average.
    pub fn record_task_outcome(&mut self, patch_ids: &[String], success: bool) {
        for patch in &mut self.patches {
            if patch_ids.contains(&patch.id) && patch.status == PatchStatus::Approved {
                let old_count = patch.applied_count as f32;
                let new_count = old_count + 1.0;
                let success_value = if success { 1.0 } else { 0.0 };
                // Running average: new_rate = (old_rate * old_count + success_value) / new_count
                patch.success_rate = (patch.success_rate * old_count + success_value) / new_count;
                patch.applied_count += 1;

                tracing::debug!(
                    id = %patch.id,
                    applied_count = patch.applied_count,
                    success_rate = %patch.success_rate,
                    "Updated prompt patch outcome"
                );
            }
        }
    }

    /// Expire patches that have been applied at least `min_applications` times
    /// but have a success rate below `min_success_rate`.
    pub fn expire_underperforming(&mut self, min_applications: usize, min_success_rate: f32) {
        for patch in &mut self.patches {
            if patch.status == PatchStatus::Approved
                && patch.applied_count >= min_applications
                && patch.success_rate < min_success_rate
            {
                tracing::warn!(
                    id = %patch.id,
                    applied_count = patch.applied_count,
                    success_rate = %patch.success_rate,
                    "Expiring underperforming prompt patch"
                );
                patch.status = PatchStatus::Expired;
            }
        }
    }

    /// Evict the lowest-confidence expired or rejected patch. Returns `true` if
    /// a patch was evicted.
    fn evict_one(&mut self) -> bool {
        // Find the index of the lowest-confidence expired/rejected patch.
        let evict_idx = self
            .patches
            .iter()
            .enumerate()
            .filter(|(_, p)| matches!(p.status, PatchStatus::Expired | PatchStatus::Rejected))
            .min_by(|(_, a), (_, b)| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i);

        if let Some(idx) = evict_idx {
            let removed = self.patches.remove(idx);
            tracing::debug!(
                id = %removed.id,
                confidence = %removed.confidence,
                status = %removed.status,
                "Evicted prompt patch to make room"
            );
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Conversation analysis — extract patch proposals
// ---------------------------------------------------------------------------

/// Analyse a completed conversation to propose prompt patches.
///
/// Examines the tool usage patterns, error sequences, and strategy rotations
/// in the history to suggest patches that would help future similar tasks.
pub fn extract_prompt_patches(history: &[ChatMessage], tools_used: &[String]) -> Vec<PromptPatch> {
    let mut patches = Vec::new();

    if tools_used.is_empty() {
        return patches;
    }

    let mut tool_errors: Vec<(String, String)> = Vec::new(); // (tool_name, error)
    let mut tool_sequence: Vec<String> = Vec::new();
    let mut had_strategy_rotation = false;
    let mut tools_tried_before_success: Vec<String> = Vec::new();
    let mut found_success = false;

    for msg in history {
        match &msg.role {
            Role::Assistant => {
                if let MessageContent::Parts(parts) = &msg.content {
                    for part in parts {
                        if let ContentPart::ToolUse { name, .. } = part {
                            tool_sequence.push(name.clone());
                            if !found_success {
                                tools_tried_before_success.push(name.clone());
                            }
                        }
                    }
                }
                // Check text content for strategy rotation markers.
                let text = match &msg.content {
                    MessageContent::Text(t) => t.as_str(),
                    MessageContent::Parts(parts) => {
                        // Check first text part only for efficiency.
                        if let Some(ContentPart::Text { text }) = parts.first() {
                            text.as_str()
                        } else {
                            ""
                        }
                    }
                };
                if text.contains("STRATEGY ROTATION") {
                    had_strategy_rotation = true;
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
                                let tool_name = tool_sequence.last().cloned().unwrap_or_default();
                                tool_errors.push((tool_name, truncate(content, 200)));
                            } else {
                                found_success = true;
                            }
                            if content.contains("STRATEGY ROTATION") {
                                had_strategy_rotation = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Derive a task description from the first user message.
    let source_task = history
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .and_then(|m| match &m.content {
            MessageContent::Text(t) => Some(truncate(t, 120)),
            MessageContent::Parts(parts) => parts.iter().find_map(|p| {
                if let ContentPart::Text { text } = p {
                    Some(truncate(text, 120))
                } else {
                    None
                }
            }),
        })
        .unwrap_or_else(|| "unknown task".to_string());

    // Pattern 1: Strategy rotation occurred → ErrorAvoidance patch
    if had_strategy_rotation {
        let errors_summary: String = tool_errors
            .iter()
            .take(3)
            .map(|(tool, err)| format!("{tool}: {err}"))
            .collect::<Vec<_>>()
            .join("; ");

        let content = format!(
            "When encountering repeated failures, switch strategy earlier. Previous errors: {}",
            if errors_summary.is_empty() {
                "unspecified".to_string()
            } else {
                errors_summary
            }
        );

        patches.push(PromptPatch::new(
            PatchType::ErrorAvoidance,
            content,
            source_task.clone(),
            0.7,
        ));
    }

    // Pattern 2: A specific tool sequence worked → WorkflowPattern patch
    if tool_sequence.len() >= 2 && found_success {
        // Deduplicate consecutive identical tools.
        let deduped: Vec<&String> =
            tool_sequence
                .iter()
                .fold(Vec::new(), |mut acc: Vec<&String>, t| {
                    if acc.last().is_none_or(|last| *last != t) {
                        acc.push(t);
                    }
                    acc
                });

        if deduped.len() >= 2 {
            let sequence_str = deduped
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" → ");
            let content = format!(
                "For similar tasks, consider this tool sequence: {}",
                sequence_str
            );

            patches.push(PromptPatch::new(
                PatchType::WorkflowPattern,
                content,
                source_task.clone(),
                0.5 + (0.1 * deduped.len().min(5) as f32), // more tools → higher confidence
            ));
        }
    }

    // Pattern 3: Multiple tools tried before finding the right one → ToolUsageHint
    if tools_tried_before_success.len() >= 2 && found_success {
        let unique_tried: Vec<String> = {
            let mut seen = Vec::new();
            for t in &tools_tried_before_success {
                if !seen.contains(t) {
                    seen.push(t.clone());
                }
            }
            seen
        };

        if unique_tried.len() >= 2 {
            let final_tool = tool_sequence
                .iter()
                .rev()
                .find(|t| !tool_errors.iter().any(|(et, _)| et == *t))
                .cloned()
                .unwrap_or_else(|| tool_sequence.last().cloned().unwrap_or_default());

            let content = format!(
                "Prefer '{}' for this type of task (tried {} tools before finding it: {})",
                final_tool,
                unique_tried.len(),
                unique_tried.join(", ")
            );

            patches.push(PromptPatch::new(
                PatchType::ToolUsageHint,
                content,
                source_task.clone(),
                0.6 + (0.05 * unique_tried.len().min(4) as f32),
            ));
        }
    }

    patches
}

// ---------------------------------------------------------------------------
// Display formatting
// ---------------------------------------------------------------------------

/// Format a list of patches for the `/patches` command output.
pub fn format_patches_command_output(patches: &[PromptPatch]) -> String {
    if patches.is_empty() {
        return "No prompt patches found.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!("Prompt Patches ({} total):", patches.len()));
    lines.push("─".repeat(60));

    for patch in patches {
        let short_id = if patch.id.len() > 8 {
            &patch.id[..8]
        } else {
            &patch.id
        };
        lines.push(format!(
            "[{}] {} ({})",
            short_id, patch.patch_type, patch.status
        ));
        lines.push(format!("  Confidence: {:.0}%", patch.confidence * 100.0));
        lines.push(format!(
            "  Applied: {} times | Success rate: {:.0}%",
            patch.applied_count,
            patch.success_rate * 100.0
        ));
        lines.push(format!("  Content: {}", truncate(&patch.content, 80)));
        lines.push(format!("  Source: {}", truncate(&patch.source_task, 60)));
        lines.push(String::new());
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers ----

    fn make_patch(patch_type: PatchType, content: &str, confidence: f32) -> PromptPatch {
        PromptPatch::new(
            patch_type,
            content.to_string(),
            "test task".to_string(),
            confidence,
        )
    }

    fn make_tool_use_msg(tool_name: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: format!("tu-{tool_name}"),
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

    // ---- PromptPatch creation ----

    #[test]
    fn prompt_patch_creation() {
        let patch = make_patch(PatchType::ToolUsageHint, "Use shell for file ops", 0.9);
        assert_eq!(patch.patch_type, PatchType::ToolUsageHint);
        assert_eq!(patch.content, "Use shell for file ops");
        assert_eq!(patch.status, PatchStatus::Proposed);
        assert_eq!(patch.applied_count, 0);
        assert!((patch.confidence - 0.9).abs() < f32::EPSILON);
        assert!(!patch.id.is_empty());
    }

    #[test]
    fn prompt_patch_clamps_confidence() {
        let high = make_patch(PatchType::ToolUsageHint, "test", 1.5);
        assert!((high.confidence - 1.0).abs() < f32::EPSILON);

        let low = make_patch(PatchType::ToolUsageHint, "test", -0.5);
        assert!(low.confidence.abs() < f32::EPSILON);
    }

    // ---- Manager CRUD ----

    #[test]
    fn propose_patch_adds_to_manager() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::DomainKnowledge, "X means Y", 0.6);
        let id = mgr.propose_patch(patch).unwrap();
        assert!(!id.is_empty());
        assert_eq!(mgr.list_patches(None).len(), 1);
    }

    #[test]
    fn approve_patch_changes_status() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ErrorAvoidance, "Avoid X", 0.5);
        let id = mgr.propose_patch(patch).unwrap();

        mgr.approve_patch(&id).unwrap();
        let p = mgr.list_patches(None)[0];
        assert_eq!(p.status, PatchStatus::Approved);
    }

    #[test]
    fn reject_patch_changes_status() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ErrorAvoidance, "Avoid X", 0.5);
        let id = mgr.propose_patch(patch).unwrap();

        mgr.reject_patch(&id).unwrap();
        let p = mgr.list_patches(None)[0];
        assert_eq!(p.status, PatchStatus::Rejected);
    }

    #[test]
    fn approve_non_proposed_patch_fails() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ErrorAvoidance, "Avoid X", 0.5);
        let id = mgr.propose_patch(patch).unwrap();
        mgr.reject_patch(&id).unwrap();

        let result = mgr.approve_patch(&id);
        assert!(result.is_err());
    }

    #[test]
    fn reject_non_proposed_patch_fails() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ErrorAvoidance, "Avoid X", 0.5);
        let id = mgr.propose_patch(patch).unwrap();
        mgr.approve_patch(&id).unwrap();

        let result = mgr.reject_patch(&id);
        assert!(result.is_err());
    }

    #[test]
    fn approve_nonexistent_patch_fails() {
        let mut mgr = PromptPatchManager::new();
        let result = mgr.approve_patch("nonexistent-id");
        assert!(result.is_err());
    }

    #[test]
    fn list_patches_with_status_filter() {
        let mut mgr = PromptPatchManager::new();

        let p1 = make_patch(PatchType::DomainKnowledge, "A", 0.5);
        let p2 = make_patch(PatchType::DomainKnowledge, "B", 0.5);
        let p3 = make_patch(PatchType::DomainKnowledge, "C", 0.5);

        let id1 = mgr.propose_patch(p1).unwrap();
        let _id2 = mgr.propose_patch(p2).unwrap();
        let id3 = mgr.propose_patch(p3).unwrap();

        mgr.approve_patch(&id1).unwrap();
        mgr.reject_patch(&id3).unwrap();

        assert_eq!(mgr.list_patches(Some(PatchStatus::Proposed)).len(), 1);
        assert_eq!(mgr.list_patches(Some(PatchStatus::Approved)).len(), 1);
        assert_eq!(mgr.list_patches(Some(PatchStatus::Rejected)).len(), 1);
        assert_eq!(mgr.list_patches(None).len(), 3);
    }

    #[test]
    fn get_active_patches_returns_approved_sorted_by_confidence() {
        let mut mgr = PromptPatchManager::new();

        let p_low = make_patch(PatchType::DomainKnowledge, "Low", 0.3);
        let p_high = make_patch(PatchType::DomainKnowledge, "High", 0.7);
        let p_mid = make_patch(PatchType::DomainKnowledge, "Mid", 0.5);

        let id_low = mgr.propose_patch(p_low).unwrap();
        let id_high = mgr.propose_patch(p_high).unwrap();
        let id_mid = mgr.propose_patch(p_mid).unwrap();

        mgr.approve_patch(&id_low).unwrap();
        mgr.approve_patch(&id_high).unwrap();
        mgr.approve_patch(&id_mid).unwrap();

        let active = mgr.get_active_patches();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0].content, "High");
        assert_eq!(active[1].content, "Mid");
        assert_eq!(active[2].content, "Low");
    }

    // ---- Auto-approve threshold ----

    #[test]
    fn auto_approve_low_risk_above_threshold() {
        let mut mgr = PromptPatchManager::new();
        mgr.auto_approve_threshold = 0.8;

        let patch = make_patch(PatchType::ToolUsageHint, "Use shell", 0.85);
        let id = mgr.propose_patch(patch).unwrap();

        let p = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.status, PatchStatus::Approved);
    }

    #[test]
    fn no_auto_approve_low_risk_below_threshold() {
        let mut mgr = PromptPatchManager::new();
        mgr.auto_approve_threshold = 0.8;

        let patch = make_patch(PatchType::ToolUsageHint, "Use shell", 0.7);
        let id = mgr.propose_patch(patch).unwrap();

        let p = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.status, PatchStatus::Proposed);
    }

    #[test]
    fn auto_approve_style_preference_above_threshold() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::StylePreference, "Use bullet points", 0.9);
        let id = mgr.propose_patch(patch).unwrap();

        let p = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.status, PatchStatus::Approved);
    }

    // ---- Manual approval required for high-risk types ----

    #[test]
    fn no_auto_approve_error_avoidance() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ErrorAvoidance, "Avoid X", 0.95);
        let id = mgr.propose_patch(patch).unwrap();

        let p = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.status, PatchStatus::Proposed);
    }

    #[test]
    fn no_auto_approve_workflow_pattern() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::WorkflowPattern, "Do A then B", 0.95);
        let id = mgr.propose_patch(patch).unwrap();

        let p = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.status, PatchStatus::Proposed);
    }

    // ---- Max patches / eviction ----

    #[test]
    fn max_patches_eviction_removes_lowest_confidence_rejected() {
        let mut mgr = PromptPatchManager::new();
        mgr.max_patches = 3;

        // Fill to capacity with three patches; reject one to make it evictable.
        let p1 = make_patch(PatchType::DomainKnowledge, "A", 0.5);
        let p2 = make_patch(PatchType::DomainKnowledge, "B", 0.3);
        let p3 = make_patch(PatchType::DomainKnowledge, "C", 0.7);

        let _id1 = mgr.propose_patch(p1).unwrap();
        let id2 = mgr.propose_patch(p2).unwrap();
        let _id3 = mgr.propose_patch(p3).unwrap();

        // Reject B (confidence 0.3) — it becomes evictable.
        mgr.reject_patch(&id2).unwrap();

        // Now propose a 4th patch — B should be evicted.
        let p4 = make_patch(PatchType::DomainKnowledge, "D", 0.6);
        mgr.propose_patch(p4).unwrap();

        assert_eq!(mgr.patches.len(), 3);
        assert!(!mgr.patches.iter().any(|p| p.content == "B"));
    }

    #[test]
    fn max_patches_no_evictable_returns_error() {
        let mut mgr = PromptPatchManager::new();
        mgr.max_patches = 2;

        // Fill with Proposed patches (not evictable).
        let p1 = make_patch(PatchType::DomainKnowledge, "A", 0.5);
        let p2 = make_patch(PatchType::DomainKnowledge, "B", 0.5);
        mgr.propose_patch(p1).unwrap();
        mgr.propose_patch(p2).unwrap();

        let p3 = make_patch(PatchType::DomainKnowledge, "C", 0.5);
        let result = mgr.propose_patch(p3);
        assert!(result.is_err());
    }

    // ---- format_patches_for_prompt ----

    #[test]
    fn format_patches_for_prompt_only_approved() {
        let mut mgr = PromptPatchManager::new();

        let p1 = make_patch(PatchType::DomainKnowledge, "Approved content", 0.6);
        let p2 = make_patch(PatchType::DomainKnowledge, "Proposed content", 0.5);

        let id1 = mgr.propose_patch(p1).unwrap();
        let _id2 = mgr.propose_patch(p2).unwrap();

        mgr.approve_patch(&id1).unwrap();

        let output = mgr.format_patches_for_prompt();
        assert!(output.contains("Approved content"));
        assert!(!output.contains("Proposed content"));
        assert!(output.contains("Learned prompt patches"));
    }

    #[test]
    fn format_patches_for_prompt_empty_when_no_approved() {
        let mut mgr = PromptPatchManager::new();
        let p = make_patch(PatchType::DomainKnowledge, "Proposed only", 0.5);
        let _id = mgr.propose_patch(p).unwrap();

        let output = mgr.format_patches_for_prompt();
        assert!(output.is_empty());
    }

    // ---- record_task_outcome ----

    #[test]
    fn record_task_outcome_updates_success_rate() {
        let mut mgr = PromptPatchManager::new();

        let p = make_patch(PatchType::DomainKnowledge, "Test", 0.6);
        let id = mgr.propose_patch(p).unwrap();
        mgr.approve_patch(&id).unwrap();

        // Record two successes and one failure.
        mgr.record_task_outcome(std::slice::from_ref(&id), true);
        mgr.record_task_outcome(std::slice::from_ref(&id), true);
        mgr.record_task_outcome(std::slice::from_ref(&id), false);

        let patch = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(patch.applied_count, 3);
        // (1.0 + 1.0 + 0.0) / 3 ≈ 0.667
        assert!((patch.success_rate - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn record_task_outcome_ignores_non_approved() {
        let mut mgr = PromptPatchManager::new();

        let p = make_patch(PatchType::DomainKnowledge, "Test", 0.5);
        let id = mgr.propose_patch(p).unwrap();
        // Patch is still Proposed — should not be updated.

        mgr.record_task_outcome(std::slice::from_ref(&id), true);

        let patch = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(patch.applied_count, 0);
    }

    // ---- expire_underperforming ----

    #[test]
    fn expire_underperforming_expires_bad_patches() {
        let mut mgr = PromptPatchManager::new();

        let p = make_patch(PatchType::DomainKnowledge, "Bad advice", 0.6);
        let id = mgr.propose_patch(p).unwrap();
        mgr.approve_patch(&id).unwrap();

        // Record 5 failures.
        for _ in 0..5 {
            mgr.record_task_outcome(std::slice::from_ref(&id), false);
        }

        mgr.expire_underperforming(3, 0.5);

        let patch = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(patch.status, PatchStatus::Expired);
    }

    #[test]
    fn expire_underperforming_keeps_good_patches() {
        let mut mgr = PromptPatchManager::new();

        let p = make_patch(PatchType::DomainKnowledge, "Good advice", 0.6);
        let id = mgr.propose_patch(p).unwrap();
        mgr.approve_patch(&id).unwrap();

        // Record 5 successes.
        for _ in 0..5 {
            mgr.record_task_outcome(std::slice::from_ref(&id), true);
        }

        mgr.expire_underperforming(3, 0.5);

        let patch = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(patch.status, PatchStatus::Approved);
    }

    #[test]
    fn expire_underperforming_ignores_insufficient_applications() {
        let mut mgr = PromptPatchManager::new();

        let p = make_patch(PatchType::DomainKnowledge, "New patch", 0.6);
        let id = mgr.propose_patch(p).unwrap();
        mgr.approve_patch(&id).unwrap();

        // Only 1 application (below min_applications threshold of 3).
        mgr.record_task_outcome(std::slice::from_ref(&id), false);

        mgr.expire_underperforming(3, 0.5);

        let patch = mgr.patches.iter().find(|p| p.id == id).unwrap();
        assert_eq!(patch.status, PatchStatus::Approved);
    }

    // ---- extract_prompt_patches ----

    #[test]
    fn extract_patches_strategy_rotation() {
        let history = vec![
            make_text_msg(Role::User, "Fix the build"),
            make_tool_use_msg("shell"),
            make_tool_result("Error: build failed\n[STRATEGY ROTATION]", true),
            make_tool_use_msg("shell"),
            make_tool_result("Build succeeded", false),
            make_text_msg(Role::Assistant, "Fixed the build."),
        ];
        let tools_used = vec!["shell".to_string()];

        let patches = extract_prompt_patches(&history, &tools_used);
        assert!(
            patches
                .iter()
                .any(|p| p.patch_type == PatchType::ErrorAvoidance),
            "Expected an ErrorAvoidance patch from strategy rotation"
        );
    }

    #[test]
    fn extract_patches_tool_sequence() {
        let history = vec![
            make_text_msg(Role::User, "Deploy the app"),
            make_tool_use_msg("shell"),
            make_tool_result("compiled", false),
            make_tool_use_msg("file_write"),
            make_tool_result("config written", false),
            make_tool_use_msg("shell"),
            make_tool_result("deployed", false),
            make_text_msg(Role::Assistant, "Deployed successfully."),
        ];
        let tools_used = vec!["shell".to_string(), "file_write".to_string()];

        let patches = extract_prompt_patches(&history, &tools_used);
        assert!(
            patches
                .iter()
                .any(|p| p.patch_type == PatchType::WorkflowPattern),
            "Expected a WorkflowPattern patch from tool sequence"
        );

        let wf = patches
            .iter()
            .find(|p| p.patch_type == PatchType::WorkflowPattern)
            .unwrap();
        assert!(wf.content.contains("shell"));
        assert!(wf.content.contains("file_write"));
    }

    #[test]
    fn extract_patches_multiple_tools_tried() {
        let history = vec![
            make_text_msg(Role::User, "Find the config file"),
            make_tool_use_msg("browser"),
            make_tool_result("Error: cannot browse local files", true),
            make_tool_use_msg("web_fetch"),
            make_tool_result("Error: not a URL", true),
            make_tool_use_msg("shell"),
            make_tool_result("/etc/config.toml", false),
            make_text_msg(Role::Assistant, "Found it at /etc/config.toml"),
        ];
        let tools_used = vec![
            "browser".to_string(),
            "web_fetch".to_string(),
            "shell".to_string(),
        ];

        let patches = extract_prompt_patches(&history, &tools_used);
        assert!(
            patches
                .iter()
                .any(|p| p.patch_type == PatchType::ToolUsageHint),
            "Expected a ToolUsageHint patch when multiple tools tried"
        );

        let hint = patches
            .iter()
            .find(|p| p.patch_type == PatchType::ToolUsageHint)
            .unwrap();
        assert!(hint.content.contains("shell"));
    }

    #[test]
    fn extract_patches_no_tools_returns_empty() {
        let history = vec![
            make_text_msg(Role::User, "Hello"),
            make_text_msg(Role::Assistant, "Hi!"),
        ];
        let patches = extract_prompt_patches(&history, &[]);
        assert!(patches.is_empty());
    }

    // ---- format_patches_command_output ----

    #[test]
    fn format_patches_command_output_empty() {
        let output = format_patches_command_output(&[]);
        assert_eq!(output, "No prompt patches found.");
    }

    #[test]
    fn format_patches_command_output_displays_details() {
        let patch = make_patch(PatchType::ToolUsageHint, "Prefer shell", 0.75);
        let output = format_patches_command_output(&[patch]);

        assert!(output.contains("Prompt Patches (1 total)"));
        assert!(output.contains("Tool Usage Hint"));
        assert!(output.contains("Proposed"));
        assert!(output.contains("75%"));
        assert!(output.contains("Prefer shell"));
    }

    // ---- Status transitions ----

    #[test]
    fn status_transition_proposed_to_approved() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::DomainKnowledge, "Test", 0.5);
        let id = mgr.propose_patch(patch).unwrap();

        assert_eq!(mgr.patches[0].status, PatchStatus::Proposed);
        mgr.approve_patch(&id).unwrap();
        assert_eq!(mgr.patches[0].status, PatchStatus::Approved);
    }

    #[test]
    fn status_transition_proposed_to_rejected() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::DomainKnowledge, "Test", 0.5);
        let id = mgr.propose_patch(patch).unwrap();

        assert_eq!(mgr.patches[0].status, PatchStatus::Proposed);
        mgr.reject_patch(&id).unwrap();
        assert_eq!(mgr.patches[0].status, PatchStatus::Rejected);
    }

    #[test]
    fn status_transition_approved_to_expired() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::DomainKnowledge, "Will expire", 0.5);
        let id = mgr.propose_patch(patch).unwrap();
        mgr.approve_patch(&id).unwrap();

        // Drive success_rate to 0 by recording all failures.
        for _ in 0..5 {
            mgr.record_task_outcome(std::slice::from_ref(&id), false);
        }
        mgr.expire_underperforming(3, 0.5);

        assert_eq!(mgr.patches[0].status, PatchStatus::Expired);
    }

    // ---- Serde roundtrip ----

    #[test]
    fn prompt_patch_serde_roundtrip() {
        let patch = make_patch(PatchType::WorkflowPattern, "Do A then B", 0.65);
        let json = serde_json::to_string(&patch).unwrap();
        let restored: PromptPatch = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, patch.id);
        assert_eq!(restored.patch_type, PatchType::WorkflowPattern);
        assert_eq!(restored.content, "Do A then B");
        assert!((restored.confidence - 0.65).abs() < f32::EPSILON);
        assert_eq!(restored.status, PatchStatus::Proposed);
    }

    #[test]
    fn propose_empty_content_fails() {
        let mut mgr = PromptPatchManager::new();
        let patch = make_patch(PatchType::ToolUsageHint, "", 0.9);
        // Override content to empty (make_patch sets non-empty).
        let mut empty_patch = patch;
        empty_patch.content = String::new();

        let result = mgr.propose_patch(empty_patch);
        assert!(result.is_err());
    }
}
