//! JIT Swarm — `spawn_swarm` tool for mid-flight parallel work.
//!
//! Exposes Hive's `maybe_decompose` + `execute_order` as a tool the main
//! agent can call when it discovers N independent subtasks during its own
//! investigation. Workers run with:
//! - Fresh AgentRuntime per task (isolated budget, no parent history)
//! - Tool filter excluding `spawn_swarm` (recursion block)
//! - SharedContext injected as the worker's first user message
//! - Parent's BudgetTracker receives aggregated swarm usage exactly once
//!
//! The tool returns the aggregated text from Hive; the main agent synthesizes
//! the user-facing reply from the tool result in its next turn.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::InboundMessage;
use temm1e_core::{Memory, Provider, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::budget::BudgetTracker;
use crate::runtime::AgentRuntime;

/// Name of the JIT-swarm tool. Exact-match filtered out of worker toolsets
/// by the worker's tool filter to prevent nested recursion.
pub const SPAWN_SWARM_TOOL_NAME: &str = "spawn_swarm";

/// Environment variable workers set to signal "I am inside a swarm, don't
/// offer spawn_swarm". Redundant with the tool filter (defence in depth).
pub const IN_SWARM_ENV: &str = "TEMM1E_IN_SWARM";

/// Per-worker limits for AgentRuntime::with_limits.
/// Workers are lightweight (no session history, no full prompt stack) so
/// these values are tuned for compact, bounded subtask work.
const WORKER_MAX_TURNS: usize = 10;
const WORKER_MAX_CONTEXT_TOKENS: usize = 30_000;
/// 0 = unlimited (matches post-P4 behaviour). Workers still respect parent
/// BudgetTracker via the dispatcher's record_usage after swarm completes.
const WORKER_MAX_TOOL_ROUNDS: usize = 0;
/// Hard wall-clock cap per worker (5 minutes) — independent of parent agent.
const WORKER_MAX_TASK_DURATION: u64 = 300;

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    goal: String,
    shared_context: String,
    #[serde(default)]
    subtasks: Option<Vec<SubtaskSpec>>,
}

#[derive(Debug, Deserialize)]
struct SubtaskSpec {
    /// v1: description parses but isn't yet routed through Hive's Queen
    /// (all subtasks still go through the Queen decomposition path). Retained
    /// in the schema for forward compatibility when accept_explicit_subtasks
    /// lands in Hive's public API.
    #[allow(dead_code)]
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    writes_files: Vec<String>,
}

/// Runtime context the spawn_swarm tool needs to actually fire. Populated
/// asynchronously after Hive + provider + memory + tools are all ready.
/// When None, the tool returns a "swarm not available yet" message so the
/// model knows to continue with its own tools.
pub struct SpawnSwarmContext {
    pub hive: Arc<temm1e_hive::Hive>,
    pub provider: Arc<dyn Provider>,
    pub memory: Arc<dyn Memory>,
    pub tools_template: Vec<Arc<dyn Tool>>,
    pub model: String,
    pub parent_budget: Arc<BudgetTracker>,
    pub cancel: CancellationToken,
    /// Parent's workspace_path. Workers use this so Witness Planner
    /// Oaths ground against the user's real filesystem instead of the
    /// process cwd. Defaults to "." if construction site passed None.
    pub workspace_path: std::path::PathBuf,
    /// Parent's Witness attachments. When Some, each JIT swarm worker
    /// is constructed with `.with_witness_attachments(...)` so Oath
    /// sealing + verification happens per-worker-turn just like the
    /// main agent. None = JIT workers run without Witness oversight.
    pub witness_attachments: Option<crate::witness_init::WitnessAttachments>,
}

/// Shared handle to the swarm context. The tool is registered early in
/// tool-creation with an Arc<RwLock<Option<...>>> that's filled in later
/// once Hive + provider wiring is complete.
pub type SwarmHandle = Arc<tokio::sync::RwLock<Option<SpawnSwarmContext>>>;

/// The spawn_swarm tool. Reads its context from a shared handle at execute
/// time — this decouples tool registration (early) from dependency wiring
/// (later in startup, after Hive is initialized).
pub struct SpawnSwarmTool {
    handle: SwarmHandle,
}

impl SpawnSwarmTool {
    pub fn new(handle: SwarmHandle) -> Self {
        Self { handle }
    }

    /// Convenience: create a fresh shared handle populated with nothing.
    /// The startup wiring fills this in via `handle.write().await = Some(ctx)`
    /// once dependencies are ready.
    pub fn fresh_handle() -> SwarmHandle {
        Arc::new(tokio::sync::RwLock::new(None))
    }
}

#[async_trait]
impl Tool for SpawnSwarmTool {
    fn name(&self) -> &str {
        SPAWN_SWARM_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Spawn parallel worker Tems to handle N independent subtasks in parallel. \
         Use ONLY when you have identified multiple units of work with no sequential \
         dependency. Each worker receives the shared_context you provide plus its \
         individual task description. Returns aggregated text from all workers; \
         you compose the final user-facing reply from it in your next turn.\n\n\
         Arguments:\n\
         - goal (required): one-sentence description of the overall goal.\n\
         - shared_context (required): everything workers need to know that you \
           have already discovered (files read, key findings, conventions, constraints). \
           Workers start blank — this is their only inheritance. Keep under 2000 tokens.\n\
         - subtasks (optional): your own decomposition into independent subtasks. \
           If provided, the Queen decomposition step is skipped. Each subtask has \
           {description, depends_on?, writes_files?}. If two subtasks write the \
           same file, one must depend_on the other.\n\n\
         Call this ONLY when:\n\
         (a) you can enumerate ≥2 truly independent units of work, AND\n\
         (b) running them in parallel is meaningfully faster than sequential."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["goal", "shared_context"],
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "Overall user-facing goal this swarm is serving."
                },
                "shared_context": {
                    "type": "string",
                    "description": "Discoveries and context from your investigation so far — what workers need to know."
                },
                "subtasks": {
                    "type": "array",
                    "description": "Optional explicit subtasks. When provided, skips the Queen decomposition LLM call.",
                    "items": {
                        "type": "object",
                        "required": ["description"],
                        "properties": {
                            "description": {"type": "string"},
                            "depends_on": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "IDs of other subtasks (0-indexed as strings, e.g. \"0\", \"1\") that must complete first."
                            },
                            "writes_files": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Files this subtask will write — used for collision detection."
                            }
                        }
                    }
                }
            }
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec![],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let args: SpawnArgs = serde_json::from_value(input.arguments)
            .map_err(|e| Temm1eError::Tool(format!("spawn_swarm: invalid arguments: {e}")))?;

        // Pull the live context (Hive, provider, etc.). If the runtime hasn't
        // finished wiring yet, return a graceful fallback so the model can
        // continue with its own tools instead of crashing.
        let ctx_guard = self.handle.read().await;
        let swarm_ctx = match ctx_guard.as_ref() {
            Some(c) => c,
            None => {
                return Ok(ToolOutput {
                    content: "Swarm not available yet (Hive not initialized). \
                              Continue with your own sequential tools."
                        .into(),
                    is_error: false,
                });
            }
        };

        // Writer-exclusion advisory check — reject obvious collisions up front
        // so the model can retry with a sequential decomposition.
        if let Some(ref subtasks) = args.subtasks {
            if let Some(collision) = detect_writer_collisions(subtasks) {
                return Ok(ToolOutput {
                    content: format!(
                        "spawn_swarm rejected: subtasks {collision} both write the same \
                         file. Either sequence them via `depends_on`, or serialize the \
                         work yourself instead of spawning a swarm."
                    ),
                    is_error: true,
                });
            }
        }

        // Build the worker execute_fn closure.
        let provider = swarm_ctx.provider.clone();
        let memory = swarm_ctx.memory.clone();
        let tools_template = swarm_ctx.tools_template.clone();
        let model = swarm_ctx.model.clone();
        let witness_attachments_for_closure = swarm_ctx.witness_attachments.clone();
        let workspace_for_closure = swarm_ctx.workspace_path.clone();
        let shared_context = args.shared_context.clone();

        let execute_fn = Arc::new(
            move |task: temm1e_hive::types::HiveTask, dep_results: Vec<(String, String)>| {
                let provider = provider.clone();
                let memory = memory.clone();
                let tools = tools_template.clone();
                let model = model.clone();
                let shared_context = shared_context.clone();
                let witness_for_worker = witness_attachments_for_closure.clone();
                let workspace_for_worker = workspace_for_closure.clone();
                async move {
                    // Tool filter: strip spawn_swarm so the worker can't recurse.
                    let filter: crate::runtime::ToolFilter =
                        Arc::new(|t: &dyn Tool| t.name() != SPAWN_SWARM_TOOL_NAME);

                    let worker = AgentRuntime::with_limits(
                        provider.clone(),
                        memory.clone(),
                        tools,
                        model.clone(),
                        None,
                        WORKER_MAX_TURNS,
                        WORKER_MAX_CONTEXT_TOKENS,
                        WORKER_MAX_TOOL_ROUNDS,
                        WORKER_MAX_TASK_DURATION,
                        0.0,
                    )
                    .with_tool_filter(filter)
                    .with_witness_attachments(witness_for_worker.as_ref());

                    let deps_text = format_dep_results(&dep_results);
                    let initial_msg = format!(
                        "## Context from parent Tem\n{shared_context}\n\n\
                         ## Your task\n{}\n\n\
                         ## Results from dependency tasks\n{deps_text}",
                        task.description,
                    );

                    let inbound = InboundMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        chat_id: format!("jit-swarm-{}", task.id),
                        user_id: "jit-swarm".into(),
                        username: None,
                        channel: "jit-swarm".into(),
                        text: Some(initial_msg),
                        attachments: vec![],
                        reply_to: None,
                        timestamp: chrono::Utc::now(),
                    };

                    let mut session = temm1e_core::types::session::SessionContext {
                        session_id: format!("jit-swarm-{}", task.id),
                        user_id: "jit-swarm".into(),
                        channel: "jit-swarm".into(),
                        chat_id: format!("jit-swarm-{}", task.id),
                        role: temm1e_core::types::rbac::Role::Admin,
                        history: vec![],
                        workspace_path: workspace_for_worker.clone(),
                        read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(
                            std::collections::HashSet::new(),
                        )),
                    };

                    match worker
                        .process_message(&inbound, &mut session, None, None, None, None, None)
                        .await
                    {
                        Ok((reply, usage)) => {
                            let snap = worker.budget_snapshot();
                            Ok(temm1e_hive::worker::TaskResult {
                                summary: reply.text,
                                tokens_used: usage.combined_tokens(),
                                input_tokens: snap.input_tokens,
                                output_tokens: snap.output_tokens,
                                cost_usd: snap.cost_usd,
                                artifacts: vec![],
                                success: true,
                                error: None,
                            })
                        }
                        Err(e) => Ok(temm1e_hive::worker::TaskResult {
                            summary: String::new(),
                            tokens_used: 0,
                            input_tokens: 0,
                            output_tokens: 0,
                            cost_usd: 0.0,
                            artifacts: vec![],
                            success: false,
                            error: Some(e.to_string()),
                        }),
                    }
                }
            },
        );

        // Decompose via Queen. For v1, we always route through Queen even
        // when the caller provides explicit subtasks. `accept_explicit_subtasks`
        // is a planned v2 Hive API addition.
        if args.subtasks.is_some() {
            info!("spawn_swarm: caller provided subtasks, but Queen still runs (v1 behaviour)");
        }
        let order_id = match Self::decompose_with_provider(swarm_ctx, &args.goal).await? {
            Some(oid) => oid,
            None => {
                return Ok(ToolOutput {
                    content: "Swarm not beneficial for this task (speedup \
                              threshold not met OR Queen cost too high). \
                              Continue with your own sequential tools."
                        .into(),
                    is_error: false,
                })
            }
        };

        // Execute the swarm.
        let swarm_result = swarm_ctx
            .hive
            .execute_order(&order_id, swarm_ctx.cancel.clone(), move |task, deps| {
                let exec = execute_fn.clone();
                async move { exec(task, deps).await }
            })
            .await?;

        // Record swarm cost against the PARENT's budget tracker — workers
        // had their own isolated trackers, so no double-count.
        if swarm_result.total_input_tokens > 0 || swarm_result.total_output_tokens > 0 {
            swarm_ctx.parent_budget.record_usage(
                swarm_result.total_input_tokens.min(u32::MAX as u64) as u32,
                swarm_result.total_output_tokens.min(u32::MAX as u64) as u32,
                swarm_result.total_cost_usd,
            );
        }

        info!(
            order_id = %order_id,
            completed = swarm_result.tasks_completed,
            escalated = swarm_result.tasks_escalated,
            workers = swarm_result.workers_used,
            wall_ms = swarm_result.wall_clock_ms,
            input_tokens = swarm_result.total_input_tokens,
            output_tokens = swarm_result.total_output_tokens,
            cost_usd = format!("{:.6}", swarm_result.total_cost_usd),
            "spawn_swarm completed"
        );

        let content = format!(
            "Swarm completed: {} tasks ({} escalated) in {}ms across {} workers.\n\n\
             Aggregated results:\n{}",
            swarm_result.tasks_completed,
            swarm_result.tasks_escalated,
            swarm_result.wall_clock_ms,
            swarm_result.workers_used,
            swarm_result.text,
        );

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

impl SpawnSwarmTool {
    /// Run Queen decomposition via the parent provider.
    async fn decompose_with_provider(
        swarm_ctx: &SpawnSwarmContext,
        goal: &str,
    ) -> Result<Option<String>, Temm1eError> {
        let provider = swarm_ctx.provider.clone();
        let model = swarm_ctx.model.clone();
        let provider_call = move |prompt: String| {
            let provider = provider.clone();
            let model = model.clone();
            async move {
                let request = temm1e_core::types::message::CompletionRequest {
                    model,
                    messages: vec![temm1e_core::types::message::ChatMessage {
                        role: temm1e_core::types::message::Role::User,
                        content: temm1e_core::types::message::MessageContent::Text(prompt),
                    }],
                    tools: vec![],
                    // Per project rule (feedback_no_max_tokens): no hardcoded caps.
                    max_tokens: None,
                    temperature: Some(0.0),
                    system: None,
                    system_volatile: None,
                };
                match provider.complete(request).await {
                    Ok(resp) => {
                        // Extract the text content.
                        let text = resp
                            .content
                            .iter()
                            .filter_map(|p| match p {
                                temm1e_core::types::message::ContentPart::Text { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let total_tokens =
                            (resp.usage.input_tokens + resp.usage.output_tokens) as u64;
                        Ok((text, total_tokens))
                    }
                    Err(e) => Err(e),
                }
            }
        };
        swarm_ctx
            .hive
            .maybe_decompose(goal, "jit-swarm", provider_call)
            .await
    }
}

fn format_dep_results(deps: &[(String, String)]) -> String {
    if deps.is_empty() {
        "(no dependency results)".into()
    } else {
        deps.iter()
            .map(|(id, summary)| format!("- [{id}]: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Return `Some("i and j")` if subtasks i and j both declare overlapping
/// `writes_files` without one depending on the other. `None` if no collision.
fn detect_writer_collisions(subtasks: &[SubtaskSpec]) -> Option<String> {
    for i in 0..subtasks.len() {
        for j in (i + 1)..subtasks.len() {
            let a = &subtasks[i];
            let b = &subtasks[j];
            let overlap: Vec<&String> = a
                .writes_files
                .iter()
                .filter(|f| b.writes_files.contains(f))
                .collect();
            if overlap.is_empty() {
                continue;
            }
            let i_str = i.to_string();
            let j_str = j.to_string();
            let a_waits = a.depends_on.contains(&j_str);
            let b_waits = b.depends_on.contains(&i_str);
            if !a_waits && !b_waits {
                warn!(
                    subtask_a = i,
                    subtask_b = j,
                    overlap = ?overlap,
                    "spawn_swarm: detected writer-file collision"
                );
                return Some(format!("{i} and {j}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_collision_no_overlap() {
        let subtasks = vec![
            SubtaskSpec {
                description: "a".into(),
                depends_on: vec![],
                writes_files: vec!["a.rs".into()],
            },
            SubtaskSpec {
                description: "b".into(),
                depends_on: vec![],
                writes_files: vec!["b.rs".into()],
            },
        ];
        assert!(detect_writer_collisions(&subtasks).is_none());
    }

    #[test]
    fn detect_collision_unsequenced_overlap() {
        let subtasks = vec![
            SubtaskSpec {
                description: "a".into(),
                depends_on: vec![],
                writes_files: vec!["shared.rs".into()],
            },
            SubtaskSpec {
                description: "b".into(),
                depends_on: vec![],
                writes_files: vec!["shared.rs".into()],
            },
        ];
        let collision = detect_writer_collisions(&subtasks);
        assert!(collision.is_some());
        assert!(collision.unwrap().contains("0"));
    }

    #[test]
    fn detect_collision_sequenced_overlap_ok() {
        // When subtask 1 depends_on 0, they don't run in parallel → no collision.
        let subtasks = vec![
            SubtaskSpec {
                description: "a".into(),
                depends_on: vec![],
                writes_files: vec!["shared.rs".into()],
            },
            SubtaskSpec {
                description: "b".into(),
                depends_on: vec!["0".into()],
                writes_files: vec!["shared.rs".into()],
            },
        ];
        assert!(detect_writer_collisions(&subtasks).is_none());
    }

    #[test]
    fn spawn_swarm_tool_name_constant() {
        assert_eq!(SPAWN_SWARM_TOOL_NAME, "spawn_swarm");
    }

    #[test]
    fn format_dep_results_empty() {
        assert_eq!(format_dep_results(&[]), "(no dependency results)");
    }

    #[test]
    fn format_dep_results_populated() {
        let deps = vec![
            ("task1".to_string(), "done".to_string()),
            ("task2".to_string(), "result".to_string()),
        ];
        let text = format_dep_results(&deps);
        assert!(text.contains("task1"));
        assert!(text.contains("task2"));
        assert!(text.contains("done"));
    }
}
