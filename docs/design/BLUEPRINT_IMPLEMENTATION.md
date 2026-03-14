# Blueprint System — Implementation Guide

> **Status**: Implementation Plan
> **Date**: 2026-03-12
> **Prerequisite**: Read `BLUEPRINT_SYSTEM.md` for the vision, design decisions, and examples.

This document specifies **exactly** how to implement the Blueprint system in the TEMM1E codebase. Every file touched, every struct added, every function written — with risk analysis for each change.

## Risk Philosophy

The Blueprint system is **purely additive**. It:
- Adds a new module (`blueprint.rs`) — no existing module modified for core logic
- Adds a new variant to `MemoryEntryType` — existing variants untouched
- Adds a new budget category to context builder — existing budgets unchanged
- Adds a new post-DONE phase to runtime — existing learning phase untouched
- Adds a new pre-loop query in runtime — existing classification/DONE flow untouched

**Zero existing behavior changes.** A user who never triggers a complex task will never see any difference. The learning system continues to operate identically. The context builder continues to allocate the same budgets for all existing categories.

---

## Implementation Steps

### Step 1: Add `Blueprint` variant to `MemoryEntryType`

**File**: `crates/temm1e-core/src/traits/memory.rs`

**Change**: Add one variant to the enum.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemoryEntryType {
    Conversation,
    LongTerm,
    DailyLog,
    Skill,
    Knowledge,
    Blueprint,     // NEW
}
```

**Risk**: ZERO.

- `MemoryEntryType` is `Serialize + Deserialize`. Adding a variant is backwards-compatible for serialization (new variant can be serialized; old data without `Blueprint` will never deserialize to it).
- All `match` statements on `MemoryEntryType` in the codebase use either specific arms + wildcard (`_`) or don't destructure at all — they filter by `==`. A new variant falls through to the wildcard or is simply never matched. No exhaustive match will break.
- The SQLite backend stores `entry_type` as a string column via serde. `"Blueprint"` is just a new string value — no schema migration needed.
- Existing queries that filter by `entry_type_filter: Some(MemoryEntryType::Knowledge)` etc. will never accidentally include Blueprint entries.

**Verification**: `cargo check --workspace` passes. Grep all `match.*MemoryEntryType` and `entry_type` references to confirm no exhaustive matches break.

---

### Step 2: Create `blueprint.rs` module

**File**: `crates/temm1e-agent/src/blueprint.rs` (NEW FILE)

**Risk**: ZERO. New file, no existing code touched.

This module contains all Blueprint logic — types, authoring, parsing, matching, refinement. It is self-contained and only called from runtime.rs and context.rs when wired in (Steps 5-6).

#### 2.1 Types

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A parsed Blueprint — the agent's procedural memory for a complex task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blueprint {
    // ── Identity ──
    pub id: String,
    pub name: String,
    pub version: u32,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,

    // ── Matching ──
    pub trigger_patterns: Vec<String>,
    pub task_signature: String,
    pub semantic_tags: Vec<String>,

    // ── Fitness ──
    pub times_executed: u32,
    pub times_succeeded: u32,
    pub times_failed: u32,
    pub avg_tool_calls: u32,
    pub avg_duration_secs: u32,

    // ── Scope ──
    pub owner_user_id: String,

    // ── Content ──
    /// The full Markdown body (Objective, Prerequisites, Phases, etc.)
    pub body: String,
}

impl Blueprint {
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

#[derive(Debug, Clone, PartialEq)]
pub enum TaskExecutionOutcome {
    Success,
    Failure,
    Partial,
}
```

#### 2.2 Creation threshold

```rust
/// Determine whether a completed task warrants a new Blueprint.
///
/// Returns true when ALL of:
/// 1. The task was compound (multi-step, detected by done_criteria)
///    OR used 3+ distinct tools
/// 2. The task involved 10+ tool calls (significant effort)
/// 3. The task succeeded or partially succeeded
/// 4. No existing blueprint was loaded for this task
pub fn should_create_blueprint(
    meta: &TaskExecutionMeta,
    blueprint_was_loaded: bool,
) -> bool {
    if blueprint_was_loaded {
        return false; // Existing blueprint was used — refine, don't create
    }

    let sufficient_complexity =
        meta.is_compound || meta.tools_used.len() >= 3;
    let sufficient_effort = meta.tool_calls >= 10;
    let succeeded = meta.outcome != TaskExecutionOutcome::Failure;

    sufficient_complexity && sufficient_effort && succeeded
}
```

#### 2.3 Authoring prompt

```rust
use temm1e_core::types::message::ChatMessage;

/// Build the LLM prompt for authoring a new Blueprint from conversation history.
///
/// Returns the system instruction + a summary of the conversation to send
/// as a single LLM call. The LLM response is the Blueprint markdown body
/// with YAML frontmatter.
pub fn build_authoring_prompt(
    history: &[ChatMessage],
    meta: &TaskExecutionMeta,
) -> String {
    let tools_str = meta.tools_used.join(", ");
    let outcome_str = match meta.outcome {
        TaskExecutionOutcome::Success => "SUCCESS",
        TaskExecutionOutcome::Failure => "FAILURE",
        TaskExecutionOutcome::Partial => "PARTIAL",
    };

    format!(
        r#"You have just completed a complex task. Write a Blueprint — a structured, \
replayable procedure document — so that a future agent can execute the same \
type of task by following your blueprint.

Task stats: {tool_calls} tool calls, {duration}s duration, tools: {tools}, outcome: {outcome}

Write the blueprint in Markdown with YAML frontmatter. Follow this EXACT structure:

```yaml
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
[Break the procedure into sequential phases. Each phase has:]
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
        tools_sig = meta.tools_used.join("+"),
        date = Utc::now().format("%Y-%m-%d"),
    )
}
```

#### 2.4 Parsing

```rust
/// Parse a Blueprint from its stored form (YAML frontmatter + Markdown body).
///
/// The `raw` string is the full content as stored in MemoryEntry.content.
/// The YAML frontmatter provides matching metadata; the body is the procedure.
pub fn parse_blueprint(raw: &str) -> Result<Blueprint, String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with("---") {
        return Err("Blueprint content does not start with YAML frontmatter".into());
    }

    let after_opening = trimmed[3..].trim_start_matches(['\r', '\n']);
    let closing_pos = after_opening.find("\n---")
        .ok_or("No closing YAML frontmatter delimiter")?;

    let yaml_str = &after_opening[..closing_pos];
    let body = after_opening[closing_pos + 4..].trim().to_string();

    // Parse YAML frontmatter into the Blueprint struct's matching fields
    let meta: BlueprintFrontmatter = serde_yaml::from_str(yaml_str)
        .map_err(|e| format!("Failed to parse blueprint YAML: {e}"))?;

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
        owner_user_id: String::new(), // Set by caller
        body,
    })
}

#[derive(Deserialize)]
struct BlueprintFrontmatter {
    id: String,
    name: String,
    trigger_patterns: Vec<String>,
    task_signature: String,
    semantic_tags: Vec<String>,
}
```

#### 2.5 Matching

```rust
/// Score how well a blueprint matches an incoming task.
///
/// Returns a score in [0.0, 1.0]. Higher = better match.
/// Uses keyword overlap between trigger_patterns and the task text.
pub fn score_blueprint_match(blueprint: &Blueprint, task_text: &str) -> f64 {
    let task_lower = task_text.to_lowercase();
    let task_words: Vec<&str> = task_lower.split_whitespace().collect();

    // Trigger pattern matching (keyword overlap)
    let trigger_hits = blueprint.trigger_patterns.iter()
        .filter(|pattern| {
            let p = pattern.to_lowercase();
            // Check if any word in the task contains this pattern
            task_words.iter().any(|w| w.contains(&p))
                || task_lower.contains(&p)
        })
        .count();

    let trigger_score = if blueprint.trigger_patterns.is_empty() {
        0.0
    } else {
        trigger_hits as f64 / blueprint.trigger_patterns.len() as f64
    };

    // Semantic tag matching
    let tag_hits = blueprint.semantic_tags.iter()
        .filter(|tag| task_lower.contains(&tag.to_lowercase()))
        .count();

    let tag_score = if blueprint.semantic_tags.is_empty() {
        0.0
    } else {
        tag_hits as f64 / blueprint.semantic_tags.len() as f64
    };

    // Success rate bonus
    let fitness_score = blueprint.success_rate();

    // Weighted combination
    trigger_score * 0.5 + tag_score * 0.3 + fitness_score * 0.2
}

/// Minimum score for a blueprint to be loaded into context.
pub const BLUEPRINT_MATCH_THRESHOLD: f64 = 0.4;

/// Search memory for blueprints and return the best match above threshold.
pub async fn find_matching_blueprint(
    memory: &dyn temm1e_core::Memory,
    task_text: &str,
) -> Option<Blueprint> {
    use temm1e_core::{SearchOpts, MemoryEntryType};

    let opts = SearchOpts {
        limit: 20,
        entry_type_filter: Some(MemoryEntryType::Blueprint),
        ..Default::default()
    };

    let entries = match memory.search(task_text, opts).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "Blueprint search failed");
            return None;
        }
    };

    let mut best: Option<(f64, Blueprint)> = None;

    for entry in &entries {
        // Blueprint content is stored as YAML frontmatter + markdown body
        match parse_blueprint(&entry.content) {
            Ok(bp) => {
                let score = score_blueprint_match(&bp, task_text);
                if score >= BLUEPRINT_MATCH_THRESHOLD {
                    if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
                        best = Some((score, bp));
                    }
                }
            }
            Err(e) => {
                tracing::debug!(id = %entry.id, error = %e, "Skipping unparseable blueprint");
            }
        }
    }

    if let Some((score, ref bp)) = best {
        tracing::info!(
            id = %bp.id,
            name = %bp.name,
            score = score,
            version = bp.version,
            "Blueprint matched for task"
        );
    }

    best.map(|(_, bp)| bp)
}
```

#### 2.6 Refinement prompt

```rust
/// Build the LLM prompt for refining an existing Blueprint after re-execution.
pub fn build_refinement_prompt(
    original_blueprint: &Blueprint,
    meta: &TaskExecutionMeta,
) -> String {
    let outcome_str = match meta.outcome {
        TaskExecutionOutcome::Success => "SUCCESS",
        TaskExecutionOutcome::Failure => "FAILURE",
        TaskExecutionOutcome::Partial => "PARTIAL",
    };

    format!(
        r#"You just executed a task using an existing Blueprint. Review the execution \
and produce an UPDATED version of the blueprint.

ORIGINAL BLUEPRINT (v{version}):
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
        version = original_blueprint.version,
        body = original_blueprint.body,
        tool_calls = meta.tool_calls,
        duration = meta.duration_secs,
        tools = meta.tools_used.join(", "),
        outcome = outcome_str,
    )
}
```

#### 2.7 Context formatting

```rust
/// Format a Blueprint for injection into the agent's context.
///
/// Returns a system message string with the blueprint body wrapped in
/// clear delimiters and an instruction preamble.
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
```

#### 2.8 Storage helpers

```rust
/// Serialize a Blueprint into a MemoryEntry for storage.
pub fn to_memory_entry(
    blueprint: &Blueprint,
    session_id: Option<String>,
) -> temm1e_core::MemoryEntry {
    // Reconstruct the full content: YAML frontmatter + body
    let trigger_yaml: String = blueprint.trigger_patterns
        .iter()
        .map(|p| format!("  - \"{}\"", p))
        .collect::<Vec<_>>()
        .join("\n");
    let tags_yaml: String = blueprint.semantic_tags
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
        }),
        timestamp: blueprint.updated,
        session_id,
        entry_type: temm1e_core::MemoryEntryType::Blueprint,
    }
}
```

#### 2.9 Tests

Full unit test suite covering:
- `should_create_blueprint()` — all threshold combinations
- `parse_blueprint()` — valid frontmatter, missing fields, malformed YAML
- `score_blueprint_match()` — keyword overlap, semantic tags, edge cases
- `format_blueprint_context()` — output contains name, version, body
- `to_memory_entry()` — roundtrip: serialize → parse → compare
- `build_authoring_prompt()` — output contains task metadata

---

### Step 3: Register the module

**File**: `crates/temm1e-agent/src/lib.rs`

**Change**: Add one `pub mod` line and one `pub use` line.

```rust
pub mod blueprint;    // NEW — add after `pub mod budget;`

pub use blueprint::Blueprint;  // NEW — add after `pub use budget::BudgetTracker;`
```

**Risk**: ZERO. Adding a public module and re-export. No existing exports change.

---

### Step 4: Add `serde_yaml` dependency

**File**: `crates/temm1e-agent/Cargo.toml`

**Change**: Add `serde_yaml` to dependencies (it's already a transitive dependency via `temm1e-skills`, so this adds no new code to the binary — just makes it a direct dependency for blueprint parsing).

```toml
serde_yaml = "0.9"
```

**Risk**: ZERO. Already in the dependency tree. No version conflicts. No new binary size.

**Verification**: Check that `temm1e-skills` already uses `serde_yaml` — if so, Cargo deduplicates automatically.

---

### Step 5: Wire into runtime — Post-DONE Blueprint Authoring

**File**: `crates/temm1e-agent/src/runtime.rs`

**Change**: After the existing Learning phase (line ~895), add Blueprint authoring. This is **appended after** the learning code, not modifying it.

```rust
// ── Blueprint Authoring (async, non-blocking) ──────────
// After learnings are persisted, check if this task warrants a Blueprint.
// Blueprint authoring makes a separate LLM call, so we spawn it as a
// background task to avoid blocking the user response.
{
    let exec_meta = crate::blueprint::TaskExecutionMeta {
        tool_calls: rounds as u32,
        tools_used: /* collect unique tool names from session.history */,
        duration_secs: task_start.elapsed().as_secs(),
        outcome: if interrupted {
            crate::blueprint::TaskExecutionOutcome::Partial
        } else if learnings.first().is_some_and(|l|
            l.outcome == learning::TaskOutcome::Failure
        ) {
            crate::blueprint::TaskExecutionOutcome::Failure
        } else if learnings.first().is_some_and(|l|
            l.outcome == learning::TaskOutcome::Partial
        ) {
            crate::blueprint::TaskExecutionOutcome::Partial
        } else {
            crate::blueprint::TaskExecutionOutcome::Success
        },
        is_compound,
    };

    // Was a blueprint loaded for this task? (set in the pre-loop phase)
    let blueprint_was_loaded = active_blueprint.is_some();

    if crate::blueprint::should_create_blueprint(&exec_meta, blueprint_was_loaded) {
        // Create new blueprint
        let prompt = crate::blueprint::build_authoring_prompt(
            &session.history, &exec_meta,
        );
        let memory = Arc::clone(&self.memory);
        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let user_id = msg.user_id.clone();
        let session_id = session.session_id.clone();

        tokio::spawn(async move {
            match author_blueprint(&provider, &model, &prompt, &user_id).await {
                Ok(bp) => {
                    let entry = crate::blueprint::to_memory_entry(&bp, Some(session_id));
                    if let Err(e) = memory.store(entry).await {
                        tracing::warn!(error = %e, "Failed to store blueprint");
                    } else {
                        tracing::info!(
                            id = %bp.id,
                            name = %bp.name,
                            "Blueprint authored and stored"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Blueprint authoring failed — skipping");
                }
            }
        });
    } else if blueprint_was_loaded {
        // Refine existing blueprint
        if let Some(ref loaded_bp) = active_blueprint {
            let prompt = crate::blueprint::build_refinement_prompt(loaded_bp, &exec_meta);
            let memory = Arc::clone(&self.memory);
            let provider = Arc::clone(&self.provider);
            let model = self.model.clone();
            let bp_id = loaded_bp.id.clone();
            let session_id = session.session_id.clone();
            let mut updated_bp = loaded_bp.clone();
            updated_bp.version += 1;
            updated_bp.times_executed += 1;
            match exec_meta.outcome {
                crate::blueprint::TaskExecutionOutcome::Success => {
                    updated_bp.times_succeeded += 1;
                }
                crate::blueprint::TaskExecutionOutcome::Failure => {
                    updated_bp.times_failed += 1;
                }
                _ => {}
            }
            updated_bp.updated = chrono::Utc::now();

            tokio::spawn(async move {
                match refine_blueprint(&provider, &model, &prompt, &mut updated_bp).await {
                    Ok(()) => {
                        let entry = crate::blueprint::to_memory_entry(
                            &updated_bp, Some(session_id),
                        );
                        // Store with same ID — this is an Update, not Create
                        if let Err(e) = memory.store(entry).await {
                            tracing::warn!(error = %e, "Failed to store refined blueprint");
                        } else {
                            tracing::info!(
                                id = %bp_id,
                                version = updated_bp.version,
                                "Blueprint refined and stored"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Blueprint refinement failed — keeping original");
                    }
                }
            });
        }
    }
}
```

**Helper functions** (private, in runtime.rs or blueprint.rs):

```rust
/// Make a single LLM call to author a Blueprint. Parses the response into a Blueprint struct.
async fn author_blueprint(
    provider: &dyn Provider,
    model: &str,
    prompt: &str,
    user_id: &str,
) -> Result<Blueprint, Temm1eError> {
    // Build a minimal CompletionRequest — no tools, no history, just the prompt
    let request = CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.to_string()),
        }],
        tools: vec![],
        max_tokens: Some(4096),
        temperature: Some(0.3), // Low temperature for precise, structured output
        system: Some("You are a technical writer. Output only the requested Blueprint document, nothing else.".to_string()),
    };

    let response = provider.complete(request).await?;
    let text = response.text.ok_or_else(||
        Temm1eError::Provider("Blueprint authoring returned no text".into())
    )?;

    let mut bp = crate::blueprint::parse_blueprint(&text)
        .map_err(|e| Temm1eError::Provider(format!("Failed to parse authored blueprint: {e}")))?;
    bp.owner_user_id = user_id.to_string();
    Ok(bp)
}

/// Make a single LLM call to refine a Blueprint. Updates the body in-place.
async fn refine_blueprint(
    provider: &dyn Provider,
    model: &str,
    prompt: &str,
    blueprint: &mut Blueprint,
) -> Result<(), Temm1eError> {
    let request = CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.to_string()),
        }],
        tools: vec![],
        max_tokens: Some(4096),
        temperature: Some(0.3),
        system: Some("You are a technical writer. Output only the updated Blueprint document, nothing else.".to_string()),
    };

    let response = provider.complete(request).await?;
    let text = response.text.ok_or_else(||
        Temm1eError::Provider("Blueprint refinement returned no text".into())
    )?;

    let refined = crate::blueprint::parse_blueprint(&text)
        .map_err(|e| Temm1eError::Provider(format!("Failed to parse refined blueprint: {e}")))?;
    blueprint.body = refined.body;
    Ok(())
}
```

**Risk analysis**:

- The authoring LLM call runs in `tokio::spawn` — it's fire-and-forget. If it fails, a warning is logged and the user is unaffected. The user's response has already been sent.
- The authoring call uses its own token budget (max_tokens: 4096). It does NOT consume the user's conversation budget or affect the BudgetTracker.
- If the Blueprint authoring LLM call panics (shouldn't, but defense-in-depth), it's caught by `tokio::spawn`'s panic handling — the spawned task dies, the main runtime continues.
- The `memory.store()` call uses an ID prefixed with `blueprint:` — it cannot collide with existing learning (`learning:`) or conversation entries.
- **Existing learning code is NOT modified.** Blueprint authoring is an additional block that runs AFTER learnings.

**Cost note**: The authoring/refinement LLM call is a real cost — one extra API call per qualifying task. At ~4K output tokens with low-temp, this is approximately $0.01-0.05 per blueprint (varies by provider). This is acceptable because:
1. Only complex tasks (10+ tool calls) qualify — these already cost $0.50-5.00
2. The blueprint saves tokens on future executions (negative net cost over time)
3. It's async and non-blocking — the user never waits for it

---

### Step 6: Wire into runtime — Pre-Loop Blueprint Matching

**File**: `crates/temm1e-agent/src/runtime.rs`

**Change**: After the DONE criteria injection (line ~510) and before the tool-use loop (line ~547), add blueprint matching.

```rust
// ── Blueprint Matching ──────────────────────────────────
// Search for a relevant blueprint before entering the tool loop.
// If found, it will be injected into context by the context builder.
let active_blueprint: Option<crate::blueprint::Blueprint> = if is_compound
    || execution_profile.as_ref().is_some_and(|p| {
        matches!(p.prompt_tier, PromptTier::Standard | PromptTier::Full)
    })
{
    crate::blueprint::find_matching_blueprint(self.memory.as_ref(), &user_text).await
} else {
    None
};
```

**Risk**: ZERO.

- This is a single `memory.search()` call with `entry_type_filter: Some(Blueprint)`. On a fresh system with no blueprints, this returns an empty vec immediately.
- The matching only runs for compound/standard/complex tasks — trivial and simple tasks skip it entirely.
- If the search fails, `find_matching_blueprint` logs a warning and returns `None`. The agent proceeds normally without a blueprint.
- The `active_blueprint` variable is passed to the context builder (Step 7) and the post-DONE phase (Step 5). If `None`, both phases skip their blueprint logic.

---

### Step 7: Wire into context builder — Blueprint Injection

**File**: `crates/temm1e-agent/src/context.rs`

**Change**: Add a new parameter to `build_context()` and a new budget category between system prompt and memory.

```rust
// Signature change — add one parameter:
pub async fn build_context(
    session: &SessionContext,
    memory: &dyn Memory,
    tools: &[Arc<dyn Tool>],
    model: &str,
    system_prompt: Option<&str>,
    max_turns: usize,
    max_context_tokens: usize,
    prompt_tier: Option<PromptTier>,
    active_blueprint: Option<&crate::blueprint::Blueprint>,  // NEW
) -> CompletionRequest {
```

Add between the fixed overhead calculation and the memory search:

```rust
// ── Category 3b: Active Blueprint (up to 10% of budget) ───
const BLUEPRINT_BUDGET_FRACTION: f32 = 0.10;
let mut blueprint_messages: Vec<ChatMessage> = Vec::new();
let mut blueprint_tokens_used = 0;

if let Some(bp) = active_blueprint {
    let bp_text = crate::blueprint::format_blueprint_context(bp);
    let tokens = estimate_tokens(&bp_text);
    let bp_budget = ((budget as f32) * BLUEPRINT_BUDGET_FRACTION) as usize;

    if tokens <= bp_budget {
        blueprint_messages.push(ChatMessage {
            role: Role::System,
            content: MessageContent::Text(bp_text),
        });
        blueprint_tokens_used = tokens;
        debug!(
            name = %bp.name,
            version = bp.version,
            tokens = tokens,
            "Blueprint injected into context"
        );
    } else {
        warn!(
            name = %bp.name,
            tokens = tokens,
            budget = bp_budget,
            "Blueprint too large for budget — skipping"
        );
    }
}
```

Update the assembly section to include blueprint messages:

```rust
// Order: summary → chat digest → blueprint → knowledge → memory → learnings → older → recent
let mut messages: Vec<ChatMessage> = Vec::new();
messages.extend(summary_messages);
if let Some(digest_msg) = chat_digest {
    messages.push(digest_msg);
}
messages.extend(blueprint_messages);  // NEW — positioned before knowledge/memory
messages.extend(knowledge_messages);
messages.extend(memory_messages);
messages.extend(learning_messages);
messages.extend(kept_older);
messages.extend(recent_messages);
```

Update the debug log to include blueprint:

```rust
debug!(
    system = system_tokens,
    tools = tool_def_tokens,
    blueprint = blueprint_tokens_used,  // NEW
    recent = recent_tokens,
    memory = memory_tokens_used,
    knowledge = knowledge_tokens_used,
    learnings = learning_tokens_used,
    history = older_tokens_used,
    total = total_tokens,
    budget = budget,
    dropped = dropped_count,
    "Context budget allocation"
);
```

**Risk analysis**:

- Adding a parameter to `build_context()` requires updating all call sites. There are exactly two:
  1. `runtime.rs` — the main tool loop call (pass `active_blueprint.as_ref()`)
  2. Tests in `context.rs` — pass `None` to maintain existing behavior

- All existing tests pass `None` for the new parameter → exact same behavior as before. No test changes needed beyond adding the parameter.
- The blueprint budget (10%) comes from `available_after_fixed_and_recent` — the same pool that memory and learnings draw from. If blueprint consumes 10%, there's slightly less room for older history. This is the intended tradeoff.
- If `active_blueprint` is `None` (which it always is when no blueprints exist), the entire blueprint block is skipped — zero overhead.

**Verification**: All existing `context.rs` tests pass unchanged (with `None` as the new parameter).

---

### Step 8: Update call sites

**File**: `crates/temm1e-agent/src/runtime.rs` (the `build_context` call in the tool loop)

```rust
// Before:
let mut request = build_context(
    session, self.memory.as_ref(), &self.tools, &self.model,
    self.system_prompt.as_deref(), self.max_turns,
    self.max_context_tokens, prompt_tier,
).await;

// After:
let mut request = build_context(
    session, self.memory.as_ref(), &self.tools, &self.model,
    self.system_prompt.as_deref(), self.max_turns,
    self.max_context_tokens, prompt_tier,
    active_blueprint.as_ref(),  // NEW
).await;
```

**File**: `crates/temm1e-agent/src/context.rs` (tests)

Every test that calls `build_context` gets `None` appended:

```rust
// Before:
build_context(&session, &memory, &tools, "model", None, 6, 30_000, None).await;

// After:
build_context(&session, &memory, &tools, "model", None, 6, 30_000, None, None).await;
```

**Risk**: ZERO. Adding `None` to existing test calls produces identical behavior.

---

## File Change Summary

| File | Change Type | Risk |
|---|---|---|
| `crates/temm1e-core/src/traits/memory.rs` | Add `Blueprint` variant to enum | ZERO — additive, backwards-compatible |
| `crates/temm1e-agent/src/blueprint.rs` | **NEW FILE** — all blueprint logic | ZERO — new code, nothing touched |
| `crates/temm1e-agent/src/lib.rs` | Add `pub mod blueprint` + `pub use` | ZERO — additive |
| `crates/temm1e-agent/Cargo.toml` | Add `serde_yaml` dep | ZERO — already transitive |
| `crates/temm1e-agent/src/runtime.rs` | Add blueprint matching (pre-loop) + authoring (post-DONE) | ZERO — appended code, existing code untouched |
| `crates/temm1e-agent/src/context.rs` | Add `active_blueprint` param + injection block | LOW — signature change requires call-site updates, but all pass `None` |
| `crates/temm1e-agent/src/context.rs` (tests) | Add `None` to `build_context` calls | ZERO — identical behavior |

**Total existing lines modified**: ~15 (call-site parameter additions + debug log field)
**Total new lines**: ~500 (blueprint.rs) + ~50 (runtime additions) + ~30 (context additions)

---

## Compilation Gate

After implementation, ALL of these must pass:

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

The new `blueprint.rs` module should have **at minimum 15 tests** covering:
- `should_create_blueprint()` — 4 cases (all true, low effort, failure, blueprint loaded)
- `parse_blueprint()` — 3 cases (valid, missing frontmatter, malformed YAML)
- `score_blueprint_match()` — 4 cases (full match, partial match, no match, empty patterns)
- `format_blueprint_context()` — 1 case (output structure)
- `to_memory_entry()` — 1 case (roundtrip serialization)
- `build_authoring_prompt()` — 1 case (contains metadata)
- `build_refinement_prompt()` — 1 case (contains original blueprint)

---

## What This Does NOT Touch

Explicitly listing what remains unchanged, for confidence:

- `learning.rs` — completely untouched. Learnings continue to fire for all tasks.
- `done_criteria.rs` — completely untouched. DONE criteria continue to work.
- `task_decomposition.rs` — completely untouched. (Phase 2 optimization: use blueprint phases as task graph — future work.)
- `self_correction.rs` — completely untouched.
- `llm_classifier.rs` — completely untouched.
- All channel code — completely untouched.
- All provider code — completely untouched.
- All tool code — completely untouched.
- `main.rs` (gateway) — completely untouched.
- Config schema — completely untouched. No new config fields needed for MVP.
- SQLite schema — completely untouched. No migration needed.

---

## Rollback Plan

If any issue is discovered after implementation:

1. **Instant rollback**: Remove the `active_blueprint` parameter from `build_context()`, revert runtime additions, delete `blueprint.rs`. The `Blueprint` variant in `MemoryEntryType` can stay (it's harmless — nothing references it).

2. **Feature flag rollback** (preferred): Add a `blueprints_enabled: bool` field to `AgentRuntime`. Set to `false` by default. All blueprint code checks this flag before executing. Enable via config: `[agent] blueprints = true`. This allows rolling out gradually without code changes.

---

## Sequence Diagram

```
User sends complex task
  │
  ├─ [Existing] Classify message → Order
  ├─ [Existing] Detect compound task → inject DONE criteria
  ├─ [NEW] Search memory for Blueprint match
  │    └─ Found? → set active_blueprint
  │
  ├─ [Existing] Tool loop begins
  │    ├─ [Modified] build_context() includes blueprint in System messages
  │    ├─ [Existing] Provider.complete()
  │    ├─ [Existing] Tool execution
  │    └─ ... loop until DONE ...
  │
  ├─ [Existing] Extract learnings → store
  ├─ [NEW] Check should_create_blueprint()
  │    ├─ Yes + no blueprint loaded → spawn author_blueprint() task
  │    └─ Blueprint was loaded → spawn refine_blueprint() task
  │
  └─ Return response to user (immediate — authoring is background)
```

---

## Future Steps (Not In This Implementation)

These are documented for completeness but are NOT part of this implementation:

1. **Config field**: `[agent] blueprints = true` — feature flag (add in follow-up)
2. **Blueprint CLI commands**: `temm1e blueprints list`, `temm1e blueprints show <id>`, `temm1e blueprints delete <id>` — user management (add in follow-up)
3. **Filesystem cache**: `~/.temm1e/blueprints/*.md` — human-readable copies (add in follow-up)
4. **Phase-aware loading**: Load only the current phase to reduce context cost (add if blueprints prove too large)
5. **Blueprint → TaskGraph integration**: Use blueprint phases as task decomposition input (add after validation)
6. **TemHub distribution**: Share blueprints across users (distant future)
