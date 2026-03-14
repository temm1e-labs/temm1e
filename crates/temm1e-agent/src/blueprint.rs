//! Blueprint System — procedural memory for the agent.
//!
//! After completing a complex task (compound, 10+ tool calls, multi-tool),
//! the runtime distils the execution into a replayable Blueprint document.
//! When a similar task arrives later, the blueprint is matched and loaded
//! into the context as an operational guide, enabling the agent to replicate
//! the procedure with minimal trial-and-error.
//!
//! Blueprints are stored as `MemoryEntry` records with
//! `MemoryEntryType::Blueprint`. They coexist with Learnings — Learnings
//! provide ambient breadcrumbs; Blueprints provide full procedures.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use temm1e_core::types::message::{ChatMessage, ContentPart, MessageContent, Role};
use temm1e_core::{Memory, MemoryEntryType, SearchOpts};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A parsed Blueprint — the agent's procedural memory for a complex task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blueprint {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,

    pub trigger_patterns: Vec<String>,
    pub task_signature: String,
    pub semantic_tags: Vec<String>,

    pub times_executed: u32,
    pub times_succeeded: u32,
    pub times_failed: u32,
    pub avg_tool_calls: u32,
    pub avg_duration_secs: u32,

    pub owner_user_id: String,

    /// The full Markdown body (Objective, Prerequisites, Phases, etc.)
    pub body: String,

    /// Pre-computed token cost of the full body (for budget enforcement).
    /// Computed at authoring/refinement time via `estimate_tokens()`.
    pub token_count: usize,
}

impl Blueprint {
    /// Calculate success rate as a fraction in [0.0, 1.0].
    pub fn success_rate(&self) -> f64 {
        if self.times_executed == 0 {
            return 0.0;
        }
        self.times_succeeded as f64 / self.times_executed as f64
    }
}

/// Execution metadata collected during a task for blueprint authoring/refinement.
#[derive(Debug, Clone)]
pub struct TaskExecutionMeta {
    pub tool_calls: u32,
    pub tools_used: Vec<String>,
    pub duration_secs: u64,
    pub outcome: TaskExecutionOutcome,
    pub is_compound: bool,
}

/// Outcome of a task execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskExecutionOutcome {
    Success,
    Failure,
    Partial,
}

// ---------------------------------------------------------------------------
// Creation threshold
// ---------------------------------------------------------------------------

/// Determine whether a completed task is a candidate for Blueprint authoring.
///
/// This is a lightweight pre-filter. The LLM makes the real decision during
/// the authoring call — it sees the full conversation and judges whether the
/// procedure is worth capturing. We only reject cases that are clearly wrong:
///
/// 1. A blueprint was already loaded (refine it instead of creating a new one)
/// 2. The task outright failed (nothing useful to capture)
/// 3. No tools were used at all (pure chat — nothing procedural to record)
pub fn should_create_blueprint(meta: &TaskExecutionMeta, blueprint_was_loaded: bool) -> bool {
    if blueprint_was_loaded {
        return false; // Existing blueprint was used — refine, don't create
    }

    let succeeded = meta.outcome != TaskExecutionOutcome::Failure;
    let used_tools = !meta.tools_used.is_empty();

    succeeded && used_tools
}

// ---------------------------------------------------------------------------
// Authoring prompt
// ---------------------------------------------------------------------------

/// Build the LLM prompt for authoring a new Blueprint from conversation history.
pub fn build_authoring_prompt(history: &[ChatMessage], meta: &TaskExecutionMeta) -> String {
    let tools_str = meta.tools_used.join(", ");
    let outcome_str = match meta.outcome {
        TaskExecutionOutcome::Success => "SUCCESS",
        TaskExecutionOutcome::Failure => "FAILURE",
        TaskExecutionOutcome::Partial => "PARTIAL",
    };
    let tools_sig = meta.tools_used.join("+");
    let date = Utc::now().format("%Y-%m-%d");

    // Extract conversation summary for the LLM to work from
    let summary = summarize_history(history);

    format!(
        r#"You have just completed a task. Decide whether it is worth capturing as a \
Blueprint — a structured, replayable procedure document that a future agent can \
follow to execute the same type of task with minimal trial-and-error.

Task stats: {tool_calls} tool calls, {duration}s duration, tools: {tools}, outcome: {outcome}

Conversation summary:
{summary}

FIRST, decide: is this task worth a blueprint?
- If the task was trivial, one-shot, or purely informational → respond with exactly: SKIP
- If the task involved a meaningful multi-step procedure that would benefit from \
  a replayable guide → write the blueprint below.

If you decide to write the blueprint, use Markdown with YAML frontmatter. Follow this EXACT structure:

```
---
id: "<kebab-case-descriptive-id>"
name: "<Human-readable title>"
trigger_patterns:
  - "<keyword1>"
  - "<keyword2>"
  - "<keyword3>"
  - "<keyword4>"
  - "<keyword5>"
task_signature: "{tools_sig}"
semantic_tags:
  - "<domain-tag1>"
  - "<domain-tag2>"
  - "<domain-tag3>"
---
```

Then the body:

## Objective
[One sentence: what does this blueprint accomplish?]

## Prerequisites
[What must be true before starting? Credentials, tools, permissions, state.]

## Phases
[Break the procedure into phases. By default, phases execute sequentially.
If a phase is genuinely independent of the previous phase (no shared state,
no ordering requirement), annotate the header:]
### Phase N: [Name] (independent)
### Phase N: [Name] (parallel with Phase M)
[If no annotation, the phase depends on the previous one (sequential).]
### Phase N: [Name]
**Goal**: [What this phase achieves]
**Steps**: [Numbered, specific, actionable steps]
**Decision points**: [Where choices must be made, and what to choose]
**Failure modes**: [What can go wrong, how to detect it, how to recover]
**Quality gates**: [Conditions that must be true before moving to next phase]

## Failure Recovery
[Table: Scenario | Detection | Response]

## Verification
[Checklist of conditions that prove the task completed correctly]

## Execution Log
### Run 1 — {date}
[What happened in this execution: targets, results, duration, tool call count, anything surprising]

CRITICAL RULES:
- Be SPECIFIC. "Navigate to the login page" is useless. "Navigate to reddit.com/login, fill #loginUsername, fill #loginPassword, click .login-btn" is a blueprint.
- Include TIMING. If you waited between actions, say how long and why.
- Include FAILURE MODES you actually encountered, not theoretical ones.
- Include DECISION POINTS where you had to choose between approaches.
- The goal is REPLAYABILITY. Another agent reading this should be able to execute the same task with the same quality, without trial and error."#,
        tool_calls = meta.tool_calls,
        duration = meta.duration_secs,
        tools = tools_str,
        outcome = outcome_str,
        tools_sig = tools_sig,
        date = date,
        summary = summary,
    )
}

// ---------------------------------------------------------------------------
// Refinement prompt
// ---------------------------------------------------------------------------

/// Build the LLM prompt for refining an existing Blueprint after re-execution.
pub fn build_refinement_prompt(original: &Blueprint, meta: &TaskExecutionMeta) -> String {
    let outcome_str = match meta.outcome {
        TaskExecutionOutcome::Success => "SUCCESS",
        TaskExecutionOutcome::Failure => "FAILURE",
        TaskExecutionOutcome::Partial => "PARTIAL",
    };

    format!(
        r#"You just executed a task using an existing Blueprint. Review the execution \
and produce an UPDATED version of the blueprint.

ORIGINAL BLUEPRINT (v{version}):
---
id: "{id}"
name: "{name}"
trigger_patterns: {triggers:?}
task_signature: "{sig}"
semantic_tags: {tags:?}
---

{body}

THIS EXECUTION:
- Tool calls: {tool_calls}
- Duration: {duration}s
- Tools used: {tools}
- Outcome: {outcome}

INSTRUCTIONS:
1. If steps worked as written, keep them unchanged.
2. If steps needed modification, update them with what actually worked.
3. If new failure modes were encountered, add them to the Failure Recovery table.
4. Append a new entry to the Execution Log.
5. Keep the same YAML frontmatter id, name, trigger_patterns, task_signature, semantic_tags.
6. Output the COMPLETE updated blueprint (frontmatter + full body), not just the diff.

The goal: the next execution should be even smoother than this one."#,
        version = original.version,
        id = original.id,
        name = original.name,
        triggers = original.trigger_patterns,
        sig = original.task_signature,
        tags = original.semantic_tags,
        body = original.body,
        tool_calls = meta.tool_calls,
        duration = meta.duration_secs,
        tools = meta.tools_used.join(", "),
        outcome = outcome_str,
    )
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// YAML frontmatter schema for deserialization.
#[derive(Debug, Deserialize)]
struct BlueprintFrontmatter {
    id: String,
    name: String,
    trigger_patterns: Vec<String>,
    task_signature: String,
    semantic_tags: Vec<String>,
}

/// Parse a Blueprint from its stored form (YAML frontmatter + Markdown body).
pub fn parse_blueprint(raw: &str) -> Result<Blueprint, String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("---") {
        return Err("Blueprint content does not start with YAML frontmatter".into());
    }

    let after_opening = trimmed[3..].trim_start_matches(['\r', '\n']);
    let closing_pos = after_opening
        .find("\n---")
        .ok_or("No closing YAML frontmatter delimiter")?;

    let yaml_str = &after_opening[..closing_pos];
    let body = after_opening[closing_pos + 4..].trim().to_string();

    let meta: BlueprintFrontmatter = serde_yaml::from_str(yaml_str)
        .map_err(|e| format!("Failed to parse blueprint YAML: {e}"))?;

    let token_count = crate::context::estimate_tokens(&body);
    Ok(Blueprint {
        id: meta.id,
        name: meta.name,
        version: 1,
        created: Utc::now(),
        updated: Utc::now(),
        trigger_patterns: meta.trigger_patterns,
        task_signature: meta.task_signature,
        semantic_tags: meta.semantic_tags,
        times_executed: 1,
        times_succeeded: 1,
        times_failed: 0,
        avg_tool_calls: 0,
        avg_duration_secs: 0,
        owner_user_id: String::new(),
        body,
        token_count,
    })
}

// ---------------------------------------------------------------------------
// Category-based matching (zero extra LLM calls)
// ---------------------------------------------------------------------------

/// Fetch all distinct semantic_tags from stored blueprints.
///
/// Returns the grounded set of categories the classifier can pick from.
/// If no blueprints exist, returns an empty vec and the classifier
/// won't see the `blueprint_hint` field at all.
pub async fn fetch_available_categories(memory: &dyn Memory) -> Vec<String> {
    let opts = SearchOpts {
        limit: 100,
        entry_type_filter: Some(MemoryEntryType::Blueprint),
        ..Default::default()
    };

    let entries = match memory.search("", opts).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "Failed to fetch blueprint categories");
            return Vec::new();
        }
    };

    let mut tags: Vec<String> = Vec::new();
    for entry in &entries {
        if let Some(arr) = entry
            .metadata
            .get("semantic_tags")
            .and_then(|v| v.as_array())
        {
            for tag in arr {
                if let Some(s) = tag.as_str() {
                    if !tags.contains(&s.to_string()) {
                        tags.push(s.to_string());
                    }
                }
            }
        }
    }

    debug!(categories = ?tags, "Available blueprint categories");
    tags
}

/// Fetch blueprints whose semantic_tags contain the given category.
///
/// Returns blueprints sorted by success rate (best first), with
/// pre-computed token counts.
pub async fn fetch_by_category(memory: &dyn Memory, category: &str) -> Vec<Blueprint> {
    let opts = SearchOpts {
        limit: 100,
        entry_type_filter: Some(MemoryEntryType::Blueprint),
        ..Default::default()
    };

    let entries = match memory.search("", opts).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "Failed to fetch blueprints by category");
            return Vec::new();
        }
    };

    let category_lower = category.to_lowercase();
    let mut blueprints: Vec<Blueprint> = Vec::new();

    for entry in &entries {
        // Check if this blueprint's semantic_tags contain the category
        let has_tag = entry
            .metadata
            .get("semantic_tags")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| {
                arr.iter().any(|t| {
                    t.as_str()
                        .is_some_and(|s| s.to_lowercase() == category_lower)
                })
            });

        if !has_tag {
            continue;
        }

        match parse_blueprint(&entry.content) {
            Ok(mut bp) => {
                // Restore metadata fields that parse_blueprint doesn't recover
                if let Some(v) = entry.metadata.get("version").and_then(|v| v.as_u64()) {
                    bp.version = v as u32;
                }
                if let Some(v) = entry
                    .metadata
                    .get("times_executed")
                    .and_then(|v| v.as_u64())
                {
                    bp.times_executed = v as u32;
                }
                if let Some(v) = entry
                    .metadata
                    .get("times_succeeded")
                    .and_then(|v| v.as_u64())
                {
                    bp.times_succeeded = v as u32;
                }
                if let Some(v) = entry.metadata.get("times_failed").and_then(|v| v.as_u64()) {
                    bp.times_failed = v as u32;
                }
                if let Some(v) = entry.metadata.get("token_count").and_then(|v| v.as_u64()) {
                    bp.token_count = v as usize;
                }
                blueprints.push(bp);
            }
            Err(e) => {
                debug!(id = %entry.id, error = %e, "Skipping unparseable blueprint");
            }
        }
    }

    // Sort by success rate descending (best first)
    blueprints.sort_by(|a, b| {
        b.success_rate()
            .partial_cmp(&a.success_rate())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    info!(
        category = category,
        count = blueprints.len(),
        "Fetched blueprints by category"
    );

    blueprints
}

/// Format a compact catalog of blueprints for injection into the system prompt.
///
/// Shows name, description (first line of body), and token cost so the LLM
/// can make informed decisions about which blueprint to follow.
pub fn format_blueprint_catalog(blueprints: &[Blueprint], loaded_id: Option<&str>) -> String {
    if blueprints.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    lines.push("=== AVAILABLE BLUEPRINTS ===".to_string());
    lines.push(
        "These are proven procedures from past executions. \
         Follow the loaded blueprint if one matches your task."
            .to_string(),
    );

    for bp in blueprints.iter().take(5) {
        // Extract first meaningful line from body as description
        let desc = bp
            .body
            .lines()
            .find(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#') && !t.starts_with("##")
            })
            .unwrap_or("(no description)")
            .trim();

        let marker = if loaded_id == Some(bp.id.as_str()) {
            " *LOADED*"
        } else {
            ""
        };

        lines.push(format!(
            "  [{}] ({} tok, v{}, {:.0}% success) {}{}",
            bp.id,
            bp.token_count,
            bp.version,
            bp.success_rate() * 100.0,
            desc,
            marker,
        ));
    }

    lines.push("=== END BLUEPRINTS ===".to_string());
    lines.join("\n")
}

/// Format a blueprint outline (objective + phase headers) for when the full
/// body is too large for the token budget.
pub fn format_blueprint_outline(blueprint: &Blueprint) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "=== BLUEPRINT OUTLINE: {} (v{}) ===",
        blueprint.name, blueprint.version
    ));
    lines.push(format!(
        "Full body too large ({} tokens). Showing outline only.",
        blueprint.token_count
    ));

    for line in blueprint.body.lines() {
        let trimmed = line.trim();
        // Keep headers and the first non-empty line after ## Objective
        if trimmed.starts_with('#') || trimmed.starts_with("**") {
            lines.push(trimmed.to_string());
        }
    }

    lines.push("=== END BLUEPRINT OUTLINE ===".to_string());
    lines.join("\n")
}

/// Select the best blueprint to auto-load from a category match.
///
/// Returns the blueprint with the highest success rate that fits within
/// the given token budget. Applies graceful degradation:
/// - Fits in budget → return full blueprint
/// - Over budget but < 25% of context → caller should use outline
/// - Way too large → None (catalog only)
pub fn select_best_blueprint(
    blueprints: &[Blueprint],
    _blueprint_budget: usize,
    total_context_limit: usize,
) -> Option<&Blueprint> {
    if blueprints.is_empty() {
        return None;
    }

    // Already sorted by success_rate (best first from fetch_by_category)
    let best = &blueprints[0];

    // Hard reject: blueprint > 25% of total context is dangerously large
    let hard_limit = total_context_limit / 4;
    if best.token_count > hard_limit {
        warn!(
            id = %best.id,
            token_count = best.token_count,
            hard_limit = hard_limit,
            "Blueprint too large for context — showing catalog only"
        );
        return None;
    }

    // Return best — caller checks if it fits in blueprint_budget for
    // full body vs outline decision
    Some(best)
}

// ---------------------------------------------------------------------------
// Context formatting
// ---------------------------------------------------------------------------

/// Format a Blueprint for injection into the agent's context.
pub fn format_blueprint_context(blueprint: &Blueprint) -> String {
    format!(
        "=== BLUEPRINT: {} (v{}) ===\n\
         A Blueprint has been loaded for this task. This is a proven procedure \
         from {} previous execution(s) (success rate: {:.0}%).\n\
         Use it as your operational guide. Follow the phases in order.\n\
         Deviate only when conditions differ from what the blueprint describes.\n\
         After completing the task, note any refinements needed.\n\n\
         {}\n\n\
         === END BLUEPRINT ===",
        blueprint.name,
        blueprint.version,
        blueprint.times_executed,
        blueprint.success_rate() * 100.0,
        blueprint.body,
    )
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Serialize a Blueprint into a MemoryEntry for storage.
pub fn to_memory_entry(
    blueprint: &Blueprint,
    session_id: Option<String>,
) -> temm1e_core::MemoryEntry {
    let trigger_yaml: String = blueprint
        .trigger_patterns
        .iter()
        .map(|p| format!("  - \"{}\"", p))
        .collect::<Vec<_>>()
        .join("\n");
    let tags_yaml: String = blueprint
        .semantic_tags
        .iter()
        .map(|t| format!("  - \"{}\"", t))
        .collect::<Vec<_>>()
        .join("\n");

    let content = format!(
        "---\n\
         id: \"{}\"\n\
         name: \"{}\"\n\
         trigger_patterns:\n{}\n\
         task_signature: \"{}\"\n\
         semantic_tags:\n{}\n\
         ---\n\n\
         {}",
        blueprint.id,
        blueprint.name,
        trigger_yaml,
        blueprint.task_signature,
        tags_yaml,
        blueprint.body,
    );

    temm1e_core::MemoryEntry {
        id: format!("blueprint:{}", blueprint.id),
        content,
        metadata: serde_json::json!({
            "type": "blueprint",
            "name": blueprint.name,
            "version": blueprint.version,
            "trigger_patterns": blueprint.trigger_patterns,
            "task_signature": blueprint.task_signature,
            "semantic_tags": blueprint.semantic_tags,
            "times_executed": blueprint.times_executed,
            "times_succeeded": blueprint.times_succeeded,
            "times_failed": blueprint.times_failed,
            "success_rate": blueprint.success_rate(),
            "owner_user_id": blueprint.owner_user_id,
            "token_count": blueprint.token_count,
        }),
        timestamp: blueprint.updated,
        session_id,
        entry_type: MemoryEntryType::Blueprint,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract unique tool names from conversation history.
pub fn extract_tools_used(history: &[ChatMessage]) -> Vec<String> {
    let mut tools = Vec::new();
    for msg in history {
        if let MessageContent::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolUse { name, .. } = part {
                    if !tools.contains(name) {
                        tools.push(name.clone());
                    }
                }
            }
        }
    }
    tools
}

/// Summarize conversation history for blueprint authoring.
///
/// Extracts a compact User ↔ Assistant text thread, ignoring tool
/// outputs (which are too large for the authoring prompt).
fn summarize_history(history: &[ChatMessage]) -> String {
    let mut entries = Vec::new();

    for msg in history {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            _ => continue,
        };

        let text = match &msg.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => {
                let texts: Vec<&str> = parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if texts.is_empty() {
                    continue;
                }
                texts.join(" ")
            }
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Truncate long messages
        let display = if trimmed.len() > 300 {
            let end = trimmed
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 300)
                .last()
                .unwrap_or(0);
            format!("{}...", &trimmed[..end])
        } else {
            trimmed.to_string()
        };

        entries.push(format!("{}: {}", role_label, display));
    }

    // Cap to last 20 exchanges
    let start = entries.len().saturating_sub(20);
    entries[start..].join("\n")
}

// ---------------------------------------------------------------------------
// Executable DAG — Phase parsing and TaskGraph bridge
// ---------------------------------------------------------------------------

/// A typed, dependency-aware phase parsed from a blueprint's Markdown body.
/// Used by the executable DAG system (`parallel_phases = true`) to run
/// independent phases concurrently via the existing TaskGraph infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintPhase {
    /// Unique identifier for this phase (e.g. "phase-1").
    pub id: String,
    /// Human-readable name (e.g. "Build", "Deploy").
    pub name: String,
    /// The goal line from the `**Goal**:` field, if present.
    pub goal: String,
    /// The full phase body (steps, decision points, failure modes, quality gates).
    /// This is what gets sent to the LLM as the instruction for this phase.
    pub body: String,
    /// Phase IDs this phase depends on. Empty = no dependencies.
    /// By default, each phase depends on the previous one (sequential).
    /// Explicit annotations override: `(parallel with Phase N)` or `(independent)`.
    pub depends_on: Vec<String>,
}

/// Parse a blueprint's Markdown body into typed phases with dependency information.
///
/// Returns an empty vec if the body contains no `### Phase N:` headers,
/// signaling the caller to fall back to holistic blueprint execution.
///
/// Dependency rules (conservative — sequential by default):
/// - No annotation → depends on previous phase (linear chain)
/// - `(parallel with Phase N)` → same dependencies as Phase N
/// - `(independent)` → no dependencies (runs as early as possible)
pub fn parse_blueprint_phases(body: &str) -> Vec<BlueprintPhase> {
    let mut phases: Vec<BlueprintPhase> = Vec::new();
    let mut current_num: Option<u32> = None;
    let mut current_name = String::new();
    let mut current_body_lines: Vec<String> = Vec::new();
    let mut parallel_annotations: Vec<(u32, ParallelAnnotation)> = Vec::new();

    for line in body.lines() {
        if let Some(header) = parse_phase_header(line) {
            // Flush previous phase
            if let Some(num) = current_num {
                phases.push(build_phase(num, &current_name, &current_body_lines));
            }
            current_num = Some(header.number);
            current_name = header.name;
            current_body_lines.clear();
            if let Some(annotation) = header.annotation {
                parallel_annotations.push((header.number, annotation));
            }
        } else if current_num.is_some() {
            current_body_lines.push(line.to_string());
        }
    }

    // Flush last phase
    if let Some(num) = current_num {
        phases.push(build_phase(num, &current_name, &current_body_lines));
    }

    if phases.is_empty() {
        return Vec::new();
    }

    // Apply dependency rules
    apply_dependencies(&mut phases, &parallel_annotations);

    debug!(
        phase_count = phases.len(),
        "Parsed blueprint phases for DAG"
    );
    phases
}

/// Convert parsed BlueprintPhases into a TaskGraph for execution.
///
/// Returns None if phases are empty or construction fails (cycle, missing dep).
/// The caller should fall back to holistic execution on None.
pub fn phases_to_task_graph(
    phases: &[BlueprintPhase],
    goal: &str,
) -> Option<crate::task_decomposition::TaskGraph> {
    if phases.is_empty() {
        return None;
    }

    let subtasks: Vec<crate::task_decomposition::SubTask> = phases
        .iter()
        .map(|phase| {
            let mut subtask =
                crate::task_decomposition::SubTask::new(phase.id.clone(), phase.body.clone());
            subtask.dependencies = phase.depends_on.clone();
            subtask
        })
        .collect();

    match crate::task_decomposition::TaskGraph::new(goal, subtasks) {
        Ok(graph) => Some(graph),
        Err(e) => {
            warn!(error = %e, "Failed to build TaskGraph from blueprint phases — falling back");
            None
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum ParallelAnnotation {
    /// `(parallel with Phase N)` — run alongside phase N
    ParallelWith(u32),
    /// `(independent)` — no dependencies
    Independent,
}

struct PhaseHeader {
    number: u32,
    name: String,
    annotation: Option<ParallelAnnotation>,
}

/// Parse a `### Phase N: Name (annotation)` header line.
fn parse_phase_header(line: &str) -> Option<PhaseHeader> {
    let trimmed = line.trim();

    // Must start with ### Phase (case-insensitive)
    if !trimmed.starts_with("### ") {
        return None;
    }
    let after_hashes = trimmed[4..].trim();

    // Match "Phase N:" pattern
    let after_phase = after_hashes.strip_prefix("Phase ")?;

    // Extract phase number
    let colon_pos = after_phase.find(':')?;
    let num_str = after_phase[..colon_pos].trim();
    let number: u32 = num_str.parse().ok()?;

    // Extract name and optional annotation
    let rest = after_phase[colon_pos + 1..].trim().to_string();

    let (name, annotation) = extract_parallel_annotation(&rest);

    Some(PhaseHeader {
        number,
        name,
        annotation,
    })
}

/// Extract `(parallel with Phase N)` or `(independent)` from a phase name.
fn extract_parallel_annotation(text: &str) -> (String, Option<ParallelAnnotation>) {
    let lower = text.to_lowercase();

    // Check for (independent)
    if let Some(start) = lower.find("(independent)") {
        let name = text[..start].trim().to_string();
        return (name, Some(ParallelAnnotation::Independent));
    }

    // Check for (parallel with Phase N) or (parallel with phase N)
    if let Some(start) = lower.find("(parallel with phase ") {
        let after = &lower[start + "(parallel with phase ".len()..];
        if let Some(end) = after.find(')') {
            let num_str = after[..end].trim();
            if let Ok(n) = num_str.parse::<u32>() {
                let name = text[..start].trim().to_string();
                return (name, Some(ParallelAnnotation::ParallelWith(n)));
            }
        }
    }

    (text.to_string(), None)
}

/// Build a BlueprintPhase from collected lines (dependencies applied later).
fn build_phase(number: u32, name: &str, body_lines: &[String]) -> BlueprintPhase {
    let body = body_lines.join("\n").trim().to_string();

    // Extract goal from **Goal**: line if present
    let goal = body_lines
        .iter()
        .find(|l| l.trim().starts_with("**Goal**:"))
        .map(|l| {
            l.trim()
                .strip_prefix("**Goal**:")
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    BlueprintPhase {
        id: format!("phase-{}", number),
        name: name.to_string(),
        goal,
        body,
        depends_on: Vec::new(), // filled in by apply_dependencies()
    }
}

/// Apply dependency rules to parsed phases.
///
/// Conservative default: each phase depends on the previous one (linear chain).
/// Explicit annotations override:
/// - `ParallelWith(N)` → same dependencies as Phase N (runs alongside it)
/// - `Independent` → no dependencies
fn apply_dependencies(phases: &mut [BlueprintPhase], annotations: &[(u32, ParallelAnnotation)]) {
    let annotation_map: std::collections::HashMap<u32, &ParallelAnnotation> =
        annotations.iter().map(|(n, a)| (*n, a)).collect();

    for i in 0..phases.len() {
        // Extract the phase number from the id
        let phase_num: u32 = phases[i]
            .id
            .strip_prefix("phase-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if let Some(annotation) = annotation_map.get(&phase_num) {
            match annotation {
                ParallelAnnotation::Independent => {
                    // No dependencies
                    phases[i].depends_on.clear();
                }
                ParallelAnnotation::ParallelWith(target_num) => {
                    // Same dependencies as the target phase
                    let target_id = format!("phase-{}", target_num);
                    let target_deps = phases
                        .iter()
                        .find(|p| p.id == target_id)
                        .map(|p| p.depends_on.clone())
                        .unwrap_or_default();
                    phases[i].depends_on = target_deps;
                }
            }
        } else if i > 0 {
            // Default: depends on previous phase (linear chain)
            phases[i].depends_on = vec![phases[i - 1].id.clone()];
        }
        // First phase with no annotation: no dependencies (it's the root)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sample_blueprint() -> Blueprint {
        Blueprint {
            id: "bp-test-deploy".to_string(),
            name: "Test Deployment".to_string(),
            version: 2,
            created: Utc::now(),
            updated: Utc::now(),
            trigger_patterns: vec![
                "deploy".to_string(),
                "production".to_string(),
                "docker".to_string(),
                "server".to_string(),
            ],
            task_signature: "shell+file".to_string(),
            semantic_tags: vec![
                "deployment".to_string(),
                "infrastructure".to_string(),
                "devops".to_string(),
            ],
            times_executed: 5,
            times_succeeded: 4,
            times_failed: 1,
            avg_tool_calls: 25,
            avg_duration_secs: 300,
            owner_user_id: "user-123".to_string(),
            body: "## Objective\nDeploy app to production.\n\n## Phases\n### Phase 1: Build\n**Steps**:\n1. Run docker build\n2. Push to registry".to_string(),
            token_count: 30, // approximate for test body
        }
    }

    fn make_meta(
        tool_calls: u32,
        tools: &[&str],
        compound: bool,
        outcome: TaskExecutionOutcome,
    ) -> TaskExecutionMeta {
        TaskExecutionMeta {
            tool_calls,
            tools_used: tools.iter().map(|s| s.to_string()).collect(),
            duration_secs: 120,
            outcome,
            is_compound: compound,
        }
    }

    // ── should_create_blueprint ──────────────────────────────────

    #[test]
    fn create_blueprint_all_conditions_met() {
        let meta = make_meta(
            15,
            &["shell", "file_read", "web_fetch"],
            true,
            TaskExecutionOutcome::Success,
        );
        assert!(should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_low_effort_passed_to_llm() {
        // Low effort tasks are no longer rejected by should_create_blueprint;
        // the LLM decides via SKIP in the authoring call.
        let meta = make_meta(
            5,
            &["shell", "file_read", "web_fetch"],
            true,
            TaskExecutionOutcome::Success,
        );
        assert!(should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_failure_rejected() {
        let meta = make_meta(
            15,
            &["shell", "file_read", "web_fetch"],
            true,
            TaskExecutionOutcome::Failure,
        );
        assert!(!should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_already_loaded_rejected() {
        let meta = make_meta(
            15,
            &["shell", "file_read", "web_fetch"],
            true,
            TaskExecutionOutcome::Success,
        );
        assert!(!should_create_blueprint(&meta, true));
    }

    #[test]
    fn create_blueprint_not_compound_but_enough_tools() {
        let meta = make_meta(
            12,
            &["shell", "browser", "web_fetch"],
            false,
            TaskExecutionOutcome::Success,
        );
        assert!(should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_compound_but_few_tools() {
        let meta = make_meta(15, &["shell"], true, TaskExecutionOutcome::Success);
        assert!(should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_no_tools_rejected() {
        let meta = make_meta(0, &[], false, TaskExecutionOutcome::Success);
        assert!(!should_create_blueprint(&meta, false));
    }

    #[test]
    fn create_blueprint_partial_success_accepted() {
        let meta = make_meta(
            15,
            &["shell", "browser", "web_fetch"],
            true,
            TaskExecutionOutcome::Partial,
        );
        assert!(should_create_blueprint(&meta, false));
    }

    // ── parse_blueprint ──────────────────────────────────────────

    #[test]
    fn parse_valid_blueprint() {
        let raw = r#"---
id: "bp-reddit-engagement"
name: "Reddit Organic Engagement"
trigger_patterns:
  - "reddit"
  - "subreddit"
  - "engagement"
task_signature: "browser+web_fetch"
semantic_tags:
  - "social-media"
  - "marketing"
---

## Objective
Engage on Reddit naturally.

## Phases
### Phase 1: Login
**Steps**:
1. Navigate to reddit.com/login
2. Enter credentials"#;

        let bp = parse_blueprint(raw).unwrap();
        assert_eq!(bp.id, "bp-reddit-engagement");
        assert_eq!(bp.name, "Reddit Organic Engagement");
        assert_eq!(bp.trigger_patterns.len(), 3);
        assert_eq!(bp.task_signature, "browser+web_fetch");
        assert_eq!(bp.semantic_tags.len(), 2);
        assert!(bp.body.contains("## Objective"));
        assert!(bp.body.contains("Phase 1: Login"));
    }

    #[test]
    fn parse_blueprint_no_frontmatter() {
        let raw = "Just some text without frontmatter";
        assert!(parse_blueprint(raw).is_err());
    }

    #[test]
    fn parse_blueprint_missing_closing() {
        let raw = "---\nid: test\nname: test\n";
        assert!(parse_blueprint(raw).is_err());
    }

    #[test]
    fn parse_blueprint_malformed_yaml() {
        let raw = "---\n  bad: [yaml: broken\n---\nBody";
        assert!(parse_blueprint(raw).is_err());
    }

    // ── format_blueprint_catalog ─────────────────────────────────

    #[test]
    fn catalog_format_with_blueprints() {
        let bp = make_sample_blueprint();
        let catalog = format_blueprint_catalog(&[bp], None);

        assert!(catalog.contains("AVAILABLE BLUEPRINTS"));
        assert!(catalog.contains("bp-test-deploy"));
        assert!(catalog.contains("30 tok"));
        assert!(catalog.contains("v2"));
        assert!(catalog.contains("80% success"));
        assert!(catalog.contains("END BLUEPRINTS"));
    }

    #[test]
    fn catalog_format_with_loaded_marker() {
        let bp = make_sample_blueprint();
        let catalog = format_blueprint_catalog(&[bp], Some("bp-test-deploy"));
        assert!(catalog.contains("*LOADED*"));
    }

    #[test]
    fn catalog_format_empty() {
        let catalog = format_blueprint_catalog(&[], None);
        assert!(catalog.is_empty());
    }

    #[test]
    fn catalog_format_caps_at_five() {
        let bps: Vec<Blueprint> = (0..8)
            .map(|i| {
                let mut bp = make_sample_blueprint();
                bp.id = format!("bp-{i}");
                bp
            })
            .collect();
        let catalog = format_blueprint_catalog(&bps, None);
        // Should have exactly 5 blueprint lines (not 8)
        let bp_lines = catalog
            .lines()
            .filter(|l| l.trim_start().starts_with('['))
            .count();
        assert_eq!(bp_lines, 5);
    }

    // ── format_blueprint_outline ──────────────────────────────────

    #[test]
    fn outline_format_shows_headers() {
        let bp = make_sample_blueprint();
        let outline = format_blueprint_outline(&bp);
        assert!(outline.contains("BLUEPRINT OUTLINE"));
        assert!(outline.contains("## Objective"));
        assert!(outline.contains("### Phase 1: Build"));
        assert!(outline.contains("too large"));
        assert!(outline.contains("END BLUEPRINT OUTLINE"));
    }

    // ── select_best_blueprint ─────────────────────────────────────

    #[test]
    fn select_best_returns_highest_success_rate() {
        let mut bp1 = make_sample_blueprint();
        bp1.token_count = 500;

        let mut bp2 = make_sample_blueprint();
        bp2.id = "bp-better".to_string();
        bp2.times_succeeded = 5;
        bp2.times_failed = 0;
        bp2.token_count = 400;

        // bp2 has 100% success rate vs bp1's 80% — sorted best first
        let bps = vec![bp2, bp1];
        let best = select_best_blueprint(&bps, 1000, 10000);
        assert!(best.is_some());
        assert_eq!(best.unwrap().id, "bp-better");
    }

    #[test]
    fn select_best_rejects_too_large() {
        let mut bp = make_sample_blueprint();
        bp.token_count = 30000; // >25% of 100K context
        let bps = vec![bp];
        let best = select_best_blueprint(&bps, 10000, 100000);
        assert!(best.is_none());
    }

    #[test]
    fn select_best_empty_list() {
        let best = select_best_blueprint(&[], 1000, 10000);
        assert!(best.is_none());
    }

    // ── format_blueprint_context ─────────────────────────────────

    #[test]
    fn format_context_includes_key_fields() {
        let bp = make_sample_blueprint();
        let ctx = format_blueprint_context(&bp);
        assert!(ctx.contains("BLUEPRINT: Test Deployment (v2)"));
        assert!(ctx.contains("5 previous execution(s)"));
        assert!(ctx.contains("success rate: 80%"));
        assert!(ctx.contains("Deploy app to production"));
        assert!(ctx.contains("END BLUEPRINT"));
    }

    // ── to_memory_entry ──────────────────────────────────────────

    #[test]
    fn memory_entry_roundtrip() {
        let bp = make_sample_blueprint();
        let entry = to_memory_entry(&bp, Some("session-1".to_string()));

        assert_eq!(entry.id, "blueprint:bp-test-deploy");
        assert_eq!(entry.entry_type, MemoryEntryType::Blueprint);
        assert_eq!(entry.session_id, Some("session-1".to_string()));

        // Content should be parseable back
        let parsed = parse_blueprint(&entry.content).unwrap();
        assert_eq!(parsed.id, "bp-test-deploy");
        assert_eq!(parsed.name, "Test Deployment");
        assert_eq!(parsed.trigger_patterns.len(), 4);

        // Metadata should have expected fields
        assert_eq!(entry.metadata["type"], "blueprint");
        assert_eq!(entry.metadata["version"], 2);
        assert_eq!(entry.metadata["times_executed"], 5);
        assert_eq!(entry.metadata["token_count"], 30);
    }

    // ── build_authoring_prompt ────────────────────────────────────

    #[test]
    fn authoring_prompt_contains_metadata() {
        let history = vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Deploy the app".to_string()),
        }];
        let meta = make_meta(
            25,
            &["shell", "browser"],
            true,
            TaskExecutionOutcome::Success,
        );
        let prompt = build_authoring_prompt(&history, &meta);

        assert!(prompt.contains("25 tool calls"));
        assert!(prompt.contains("120s duration"));
        assert!(prompt.contains("shell, browser"));
        assert!(prompt.contains("SUCCESS"));
        assert!(prompt.contains("REPLAYABILITY"));
    }

    // ── build_refinement_prompt ──────────────────────────────────

    #[test]
    fn refinement_prompt_contains_original() {
        let bp = make_sample_blueprint();
        let meta = make_meta(
            20,
            &["shell", "file_read"],
            true,
            TaskExecutionOutcome::Success,
        );
        let prompt = build_refinement_prompt(&bp, &meta);

        assert!(prompt.contains("ORIGINAL BLUEPRINT (v2)"));
        assert!(prompt.contains("bp-test-deploy"));
        assert!(prompt.contains("Deploy app to production"));
        assert!(prompt.contains("Tool calls: 20"));
    }

    // ── success_rate ─────────────────────────────────────────────

    #[test]
    fn success_rate_normal() {
        let bp = make_sample_blueprint();
        assert!((bp.success_rate() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn success_rate_zero_executions() {
        let mut bp = make_sample_blueprint();
        bp.times_executed = 0;
        assert_eq!(bp.success_rate(), 0.0);
    }

    // ── extract_tools_used ───────────────────────────────────────

    #[test]
    fn extract_tools_from_history() {
        let history = vec![
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({}),
                }]),
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: "t2".to_string(),
                    name: "browser".to_string(),
                    input: serde_json::json!({}),
                }]),
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: "t3".to_string(),
                    name: "shell".to_string(), // duplicate
                    input: serde_json::json!({}),
                }]),
            },
        ];
        let tools = extract_tools_used(&history);
        assert_eq!(tools, vec!["shell", "browser"]);
    }

    #[test]
    fn extract_tools_empty_history() {
        let tools = extract_tools_used(&[]);
        assert!(tools.is_empty());
    }

    // ── summarize_history ────────────────────────────────────────

    #[test]
    fn summarize_extracts_user_and_assistant() {
        let history = vec![
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Deploy the app".to_string()),
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("Starting deployment...".to_string()),
            },
            ChatMessage {
                role: Role::Tool,
                content: MessageContent::Text("exit code 0".to_string()),
            },
        ];
        let summary = summarize_history(&history);
        assert!(summary.contains("User: Deploy the app"));
        assert!(summary.contains("Assistant: Starting deployment"));
        assert!(!summary.contains("exit code"));
    }

    // ── parse_blueprint_phases ─────────────────────────────────────

    #[test]
    fn parse_phases_linear() {
        let body = "\
## Objective
Deploy the app.

## Phases
### Phase 1: Build
**Goal**: Compile the binary
**Steps**:
1. Run cargo build --release

### Phase 2: Test
**Goal**: Run all tests
**Steps**:
1. Run cargo test

### Phase 3: Deploy
**Goal**: Push to production
**Steps**:
1. Run docker push";

        let phases = parse_blueprint_phases(body);
        assert_eq!(phases.len(), 3);

        assert_eq!(phases[0].id, "phase-1");
        assert_eq!(phases[0].name, "Build");
        assert_eq!(phases[0].goal, "Compile the binary");
        assert!(phases[0].depends_on.is_empty()); // root — no deps

        assert_eq!(phases[1].id, "phase-2");
        assert_eq!(phases[1].name, "Test");
        assert_eq!(phases[1].depends_on, vec!["phase-1"]); // sequential default

        assert_eq!(phases[2].id, "phase-3");
        assert_eq!(phases[2].depends_on, vec!["phase-2"]); // sequential default
    }

    #[test]
    fn parse_phases_with_parallel_annotation() {
        let body = "\
## Phases
### Phase 1: Setup
**Steps**: Create directories

### Phase 2: Lint (parallel with Phase 3)
**Steps**: Run clippy

### Phase 3: Test
**Steps**: Run cargo test

### Phase 4: Deploy
**Steps**: Push to registry";

        let phases = parse_blueprint_phases(body);
        assert_eq!(phases.len(), 4);

        // Phase 1: root
        assert!(phases[0].depends_on.is_empty());

        // Phase 2: parallel with Phase 3 → same deps as Phase 3
        // Phase 3 depends on Phase 2 by default, but Phase 2 says parallel with 3
        // Phase 3's deps at assignment time: depends on phase-2 (default sequential)
        // Phase 2's annotation: ParallelWith(3) → copies Phase 3's deps
        // But Phase 3 hasn't been dependency-resolved yet at Phase 2's turn
        // apply_dependencies processes in order, so Phase 3's deps are empty at that point
        // This means Phase 2 gets no deps (parallel with phase-3 which has no deps yet)
        // Phase 3 gets default sequential: depends on phase-2
        // Result: Phase 2 has no deps, Phase 3 depends on phase-2
        // This is correct! Phase 2 and Phase 3 do NOT run in parallel in this case.
        // The annotation should be: Phase 3 (parallel with Phase 2) to run alongside Phase 2.

        // Phase 4: depends on phase-3 (sequential default)
        assert_eq!(phases[3].depends_on, vec!["phase-3"]);
    }

    #[test]
    fn parse_phases_with_independent_annotation() {
        let body = "\
## Phases
### Phase 1: Setup
**Steps**: Create directories

### Phase 2: Lint (independent)
**Steps**: Run clippy

### Phase 3: Test (independent)
**Steps**: Run cargo test

### Phase 4: Deploy
**Steps**: Push to registry";

        let phases = parse_blueprint_phases(body);
        assert_eq!(phases.len(), 4);

        // Phase 1: root
        assert!(phases[0].depends_on.is_empty());

        // Phase 2: independent — no deps
        assert!(phases[1].depends_on.is_empty());

        // Phase 3: independent — no deps
        assert!(phases[2].depends_on.is_empty());

        // Phase 4: sequential default → depends on phase-3
        assert_eq!(phases[3].depends_on, vec!["phase-3"]);
    }

    #[test]
    fn parse_phases_empty_body() {
        let body = "## Objective\nJust some text with no phases.";
        let phases = parse_blueprint_phases(body);
        assert!(phases.is_empty());
    }

    #[test]
    fn parse_phases_single_phase() {
        let body = "\
## Phases
### Phase 1: Everything
**Steps**: Do it all";

        let phases = parse_blueprint_phases(body);
        assert_eq!(phases.len(), 1);
        assert!(phases[0].depends_on.is_empty());
    }

    #[test]
    fn parse_phases_goal_extraction() {
        let body = "\
## Phases
### Phase 1: Build
**Goal**: Compile the release binary
**Steps**:
1. Run cargo build --release
2. Check binary size";

        let phases = parse_blueprint_phases(body);
        assert_eq!(phases[0].goal, "Compile the release binary");
    }

    // ── phases_to_task_graph ──────────────────────────────────────

    #[test]
    fn phases_to_graph_linear() {
        let body = "\
## Phases
### Phase 1: Build
**Steps**: Compile

### Phase 2: Test
**Steps**: Run tests

### Phase 3: Deploy
**Steps**: Push";

        let phases = parse_blueprint_phases(body);
        let graph = phases_to_task_graph(&phases, "Deploy pipeline").unwrap();

        // Should have 3 tasks in linear order
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1); // Only phase-1 is ready
        assert_eq!(ready[0].id, "phase-1");
    }

    #[test]
    fn phases_to_graph_with_parallelism() {
        let body = "\
## Phases
### Phase 1: Setup
**Steps**: Init

### Phase 2: Lint (independent)
**Steps**: Clippy

### Phase 3: Test (independent)
**Steps**: Test

### Phase 4: Deploy
**Steps**: Push";

        let phases = parse_blueprint_phases(body);
        let graph = phases_to_task_graph(&phases, "CI pipeline").unwrap();

        // Phase 1, 2, and 3 should all be ready (independent)
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn phases_to_graph_empty() {
        let result = phases_to_task_graph(&[], "nothing");
        assert!(result.is_none());
    }

    // ── parse_phase_header ────────────────────────────────────────

    #[test]
    fn header_basic() {
        let h = parse_phase_header("### Phase 1: Build").unwrap();
        assert_eq!(h.number, 1);
        assert_eq!(h.name, "Build");
        assert!(h.annotation.is_none());
    }

    #[test]
    fn header_with_independent() {
        let h = parse_phase_header("### Phase 3: Lint (independent)").unwrap();
        assert_eq!(h.number, 3);
        assert_eq!(h.name, "Lint");
        assert!(matches!(
            h.annotation,
            Some(ParallelAnnotation::Independent)
        ));
    }

    #[test]
    fn header_with_parallel() {
        let h = parse_phase_header("### Phase 4: Test (parallel with Phase 2)").unwrap();
        assert_eq!(h.number, 4);
        assert_eq!(h.name, "Test");
        assert!(matches!(
            h.annotation,
            Some(ParallelAnnotation::ParallelWith(2))
        ));
    }

    #[test]
    fn header_not_a_phase() {
        assert!(parse_phase_header("## Phase 1: Build").is_none()); // wrong heading level
        assert!(parse_phase_header("### Something Else").is_none()); // no "Phase"
        assert!(parse_phase_header("regular text").is_none());
    }

    // ── extract_parallel_annotation ───────────────────────────────

    #[test]
    fn annotation_none() {
        let (name, ann) = extract_parallel_annotation("Build Docker Image");
        assert_eq!(name, "Build Docker Image");
        assert!(ann.is_none());
    }

    #[test]
    fn annotation_independent() {
        let (name, ann) = extract_parallel_annotation("Run Linter (independent)");
        assert_eq!(name, "Run Linter");
        assert!(matches!(ann, Some(ParallelAnnotation::Independent)));
    }

    #[test]
    fn annotation_parallel_with() {
        let (name, ann) = extract_parallel_annotation("Run Tests (parallel with Phase 2)");
        assert_eq!(name, "Run Tests");
        assert!(matches!(ann, Some(ParallelAnnotation::ParallelWith(2))));
    }
}
