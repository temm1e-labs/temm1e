//! Planner Oath generation helpers.
//!
//! Phase 2.5: provide a system prompt + JSON schema for the Planner LLM to
//! emit a Root Oath when it classifies a user request as non-trivial. Also
//! provide a parser that turns the LLM's JSON output back into an `Oath`
//! ready for sealing.
//!
//! This module is standalone — it does not wire into the Planner yet. The
//! Planner integration is a small follow-up (call the generation prompt
//! during classification, parse, seal via the Spec Reviewer).

use crate::auto_detect::detect_active_sets;
use crate::error::WitnessError;
use crate::types::{Oath, Predicate};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Static Oath-generation system prompt to inject into the Planner.
pub const OATH_GENERATION_PROMPT: &str = r#"You are the Oath Planner for the Witness verification system.

Before the agent executes a non-trivial task, you must emit a frozen
machine-checkable commitment of what "done" means. This is the Oath.

Rules for a valid Oath (the Spec Reviewer will reject violations):
1. At least ONE postcondition must be a deterministic Tier 0 predicate
   (FileExists, GrepPresent, CommandExits, etc.) — not AspectVerifier
   or AdversarialJudge.
2. For code-producing tasks, include:
   - A wiring check: GrepCountAtLeast with n >= 2 over the touched files.
   - An anti-stub check: GrepAbsent with a pattern like
     "todo!|unimplemented!|NotImplementedError|pass\\s*#.*TODO".
3. Keep the postcondition count between 2 and 8 for the Root Oath.
4. Reference real file paths, real commands, real patterns — not placeholders.
5. Reply ONLY as JSON matching the schema below. No prose, no markdown fences.

Schema:
{
  "goal": "natural-language description of the task",
  "postconditions": [
    { "kind": "file_exists", "path": "..." },
    { "kind": "grep_present", "pattern": "...", "path_glob": "..." },
    { "kind": "grep_absent", "pattern": "...", "path_glob": "..." },
    { "kind": "grep_count_at_least", "pattern": "...", "path_glob": "...", "n": 2 },
    { "kind": "command_exits", "cmd": "...", "args": ["..."], "expected_code": 0, "cwd": null, "timeout_ms": 30000 },
    ...
  ]
}

You will be told which predicate sets are active for this project (e.g.
"rust", "python", "javascript") so you can use the appropriate command
names and paths. If no predicate set matches, fall back to Tier 0
primitives only.
"#;

/// A minimal Oath draft parsed from the Planner's JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerOathDraft {
    pub goal: String,
    pub postconditions: Vec<Predicate>,
}

/// Parse a Planner LLM response into a `PlannerOathDraft`.
///
/// Tolerates markdown code fences and surrounding prose — finds the first
/// top-level JSON object and parses it.
pub fn parse_planner_oath(text: &str) -> Result<PlannerOathDraft, WitnessError> {
    let stripped = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let start = stripped.find('{');
    let end = stripped.rfind('}');
    let json_str = match (start, end) {
        (Some(s), Some(e)) if e >= s => &stripped[s..=e],
        _ => {
            return Err(WitnessError::Internal(format!(
                "planner response has no JSON object: {}",
                stripped
            )));
        }
    };

    serde_json::from_str::<PlannerOathDraft>(json_str).map_err(WitnessError::Json)
}

/// Build a full `Oath` from a `PlannerOathDraft` by attaching the session
/// metadata and the detected active predicate sets.
///
/// The returned Oath is unsealed — the caller must still pass it through
/// `oath::seal_oath()` to write it into the Ledger. Sealing runs the Spec
/// Reviewer which will reject the Oath if it doesn't meet minimum rigor.
pub fn oath_from_draft(
    draft: PlannerOathDraft,
    subtask_id: impl Into<String>,
    root_goal_id: impl Into<String>,
    session_id: impl Into<String>,
    workspace_root: &Path,
) -> Oath {
    let active_sets = detect_active_sets(workspace_root);
    let mut oath = Oath::draft(subtask_id, root_goal_id, session_id, draft.goal);
    oath.active_predicate_sets = active_sets;
    oath.postconditions = draft.postconditions;
    oath
}

/// Build the user-side prompt fragment the Planner appends to its normal
/// planning prompt when a task is classified as non-trivial. Returns a
/// string ready to concatenate into the Planner's instruction.
pub fn build_planner_user_prompt(user_request: &str, active_sets: &[String]) -> String {
    let sets_list = if active_sets.is_empty() {
        "none (use generic Tier 0 primitives)".to_string()
    } else {
        active_sets.join(", ")
    };
    format!(
        "User request:\n{}\n\nActive predicate sets for this project: {}\n\nEmit a Root Oath as JSON matching the schema above. Reply with JSON only.",
        user_request, sets_list
    )
}

/// End-to-end Oath generation via a real LLM Provider call.
///
/// This is the Phase 4 wiring point — it makes a clean-slate LLM call with
/// the static `OATH_GENERATION_PROMPT` system prompt, parses the JSON
/// response, builds an `Oath` from the draft, and seals it via
/// `oath::seal_oath` (which runs the Spec Reviewer + writes to the Ledger).
///
/// **Single-model policy**: uses the same Provider/model the agent runs.
/// **Clean-slate context**: no conversation history, no tool schema, no
/// prior reasoning — only the static prompt + user request + active sets.
/// **Failure mode**: returns Err on any stage (LLM error, parse error,
/// Spec Reviewer rejection, sealing error). The caller is responsible for
/// deciding whether to retry, fall back to a default Oath, or skip Witness
/// for this session.
/// Bundled inputs to `seal_oath_via_planner`. Kept as a struct so the
/// function stays under clippy's `too_many_arguments` ceiling and so
/// callers can construct the request as a literal.
pub struct PlannerOathRequest<'a> {
    pub witness: &'a std::sync::Arc<crate::witness::Witness>,
    pub provider: std::sync::Arc<dyn temm1e_core::traits::Provider>,
    pub model: String,
    pub user_request: &'a str,
    pub workspace_root: &'a std::path::Path,
    pub session_id: String,
    pub root_goal_id: String,
    pub subtask_id: String,
}

pub async fn seal_oath_via_planner(
    req: PlannerOathRequest<'_>,
) -> Result<(crate::types::Oath, i64), WitnessError> {
    use temm1e_core::types::message::{ChatMessage, CompletionRequest, MessageContent, Role};

    let active_sets = crate::auto_detect::detect_active_sets(req.workspace_root);
    let user_prompt = build_planner_user_prompt(req.user_request, &active_sets);

    let llm_req = CompletionRequest {
        model: req.model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(user_prompt),
        }],
        tools: vec![],
        max_tokens: Some(1024),
        temperature: Some(0.0),
        system: Some(OATH_GENERATION_PROMPT.to_string()),
    };

    let resp = req
        .provider
        .complete(llm_req)
        .await
        .map_err(|e| WitnessError::Internal(format!("planner LLM call: {e}")))?;

    // Extract text from the response.
    let text = resp
        .content
        .iter()
        .filter_map(|p| match p {
            temm1e_core::types::message::ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let draft = parse_planner_oath(&text)?;
    let oath = oath_from_draft(
        draft,
        req.subtask_id,
        req.root_goal_id,
        req.session_id,
        req.workspace_root,
    );
    crate::oath::seal_oath(req.witness.ledger(), oath).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn parse_planner_oath_handles_plain_json() {
        let text = r#"{
          "goal": "add compute_tax",
          "postconditions": [
            {"kind": "file_exists", "path": "/tmp/compute_tax.py"},
            {"kind": "grep_count_at_least", "pattern": "compute_tax", "path_glob": "*.py", "n": 2},
            {"kind": "grep_absent", "pattern": "TODO", "path_glob": "*.py"}
          ]
        }"#;
        let draft = parse_planner_oath(text).unwrap();
        assert_eq!(draft.goal, "add compute_tax");
        assert_eq!(draft.postconditions.len(), 3);
    }

    #[test]
    fn parse_planner_oath_handles_markdown_fence() {
        let text = r#"```json
{
  "goal": "x",
  "postconditions": [{"kind": "file_exists", "path": "/tmp/x"}]
}
```"#;
        let draft = parse_planner_oath(text).unwrap();
        assert_eq!(draft.goal, "x");
    }

    #[test]
    fn parse_planner_oath_rejects_non_json() {
        let r = parse_planner_oath("I don't feel like replying with JSON today.");
        assert!(r.is_err());
    }

    #[test]
    fn oath_from_draft_attaches_active_sets() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

        let draft = PlannerOathDraft {
            goal: "add fn foo".into(),
            postconditions: vec![Predicate::FileExists {
                path: PathBuf::from("src/foo.rs"),
            }],
        };
        let oath = oath_from_draft(draft, "root", "root-goal", "sess", dir.path());
        assert_eq!(oath.goal, "add fn foo");
        assert_eq!(oath.postconditions.len(), 1);
        assert!(oath.active_predicate_sets.contains(&"rust".to_string()));
    }

    #[test]
    fn build_planner_user_prompt_mentions_active_sets() {
        let prompt = build_planner_user_prompt("add foo", &["rust".into(), "docs".into()]);
        assert!(prompt.contains("add foo"));
        assert!(prompt.contains("rust, docs"));
        assert!(prompt.contains("JSON"));
    }

    #[test]
    fn build_planner_user_prompt_handles_empty_sets() {
        let prompt = build_planner_user_prompt("do something", &[]);
        assert!(prompt.contains("none (use generic Tier 0 primitives)"));
    }

    #[test]
    fn oath_generation_prompt_mentions_law1_rules() {
        assert!(OATH_GENERATION_PROMPT.contains("wiring check"));
        assert!(OATH_GENERATION_PROMPT.contains("anti-stub check"));
        assert!(OATH_GENERATION_PROMPT.contains("Tier 0"));
    }
}
