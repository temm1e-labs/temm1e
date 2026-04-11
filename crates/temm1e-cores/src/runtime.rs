//! Core runtime — the simplified agent loop for TemDOS cores.
//!
//! This is the heart of TemDOS. A stripped-down agent loop that inherits
//! Tem's Mind execution cycle: ORDER → THINK → ACTION → VERIFY → DONE.
//!
//! - **ORDER**: Receives task string from main agent
//! - **THINK**: Builds context (system prompt + history + tool defs)
//! - **ACTION**: Calls LLM, executes tools
//! - **VERIFY**: Failure tracking + strategy rotation (inherited from self_correction.rs)
//! - **DONE**: LLM stops calling tools (implicit for single-task specialists)
//!
//! What it does NOT have: classification, blueprints, consciousness,
//! social intelligence, streaming, prompted tool calling, lambda memory,
//! learning extraction, interrupt handling, task decomposition.

use std::path::PathBuf;
use std::sync::Arc;

use temm1e_agent::budget::{self, BudgetTracker, ModelPricing};
use temm1e_agent::executor::execute_tool;
use temm1e_agent::self_correction::FailureTracker;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role, ToolDefinition,
};
use temm1e_core::types::session::SessionContext;
use temm1e_core::{Provider, Tool};
use tracing::{debug, info};

use crate::types::CoreResult;

/// The TemDOS core runtime — executes a specialist core's LLM tool loop.
pub struct CoreRuntime {
    /// The core's system prompt (from the .md file, with placeholders substituted).
    system_prompt: String,
    /// Shared provider (same as main agent's).
    provider: Arc<dyn Provider>,
    /// Tools available to this core (all tools MINUS invoke_core).
    tools: Vec<Arc<dyn Tool>>,
    /// Shared budget tracker (same atomic instance as main agent's).
    budget: Arc<BudgetTracker>,
    /// Pricing for cost calculation.
    model_pricing: ModelPricing,
    /// Model name.
    model: String,
    /// Maximum context tokens for this core.
    max_context_tokens: usize,
    /// Core name (for logging).
    core_name: String,
    /// LLM temperature (0.0 = deterministic, 0.7 = creative).
    temperature: f32,
}

impl CoreRuntime {
    /// Create a new core runtime.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core_name: String,
        system_prompt: String,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        budget: Arc<BudgetTracker>,
        model_pricing: ModelPricing,
        model: String,
        max_context_tokens: usize,
        temperature: f32,
    ) -> Self {
        Self {
            system_prompt,
            provider,
            tools,
            budget,
            model_pricing,
            model,
            max_context_tokens,
            core_name,
            temperature,
        }
    }

    /// Run the core's task to completion.
    ///
    /// The core runs its LLM tool loop until the LLM stops calling tools
    /// (i.e., produces a final text response) or the shared budget is exhausted.
    pub async fn run(
        &self,
        task: &str,
        workspace_path: PathBuf,
    ) -> Result<CoreResult, Temm1eError> {
        let session_id = format!("core-{}-{}", self.core_name, uuid::Uuid::new_v4());

        let mut session = SessionContext {
            session_id: session_id.clone(),
            channel: "core".to_string(),
            chat_id: session_id.clone(),
            user_id: "core".to_string(),
            role: temm1e_core::types::rbac::Role::Admin,
            history: Vec::new(),
            workspace_path,
            read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashSet::new(),
            )),
        };

        // Initial user message is the task
        session.history.push(ChatMessage {
            role: Role::User,
            content: MessageContent::Text(task.to_string()),
        });

        let tool_defs: Vec<ToolDefinition> = self
            .tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();

        let mut rounds: usize = 0;
        let mut total_input_tokens: u32 = 0;
        let mut total_output_tokens: u32 = 0;
        let mut total_cost: f64 = 0.0;

        // VERIFY: failure tracking — inherited from Tem's Mind self-correction.
        // Tracks consecutive failures per tool and injects strategy rotation
        // prompts after 2 failures, preventing blind retries.
        let mut failure_tracker = FailureTracker::default();

        info!(
            core = %self.core_name,
            tools = tool_defs.len(),
            "TemDOS core started"
        );

        loop {
            // Budget gate — check shared budget before every LLM call
            self.budget.check_budget().map_err(|e| {
                Temm1eError::Tool(format!("[{}] Budget exhausted: {}", self.core_name, e))
            })?;

            // Build request with pruned history
            let messages = self.prune_history(&session.history);
            let request = CompletionRequest {
                model: self.model.clone(),
                messages,
                tools: tool_defs.clone(),
                max_tokens: None,
                temperature: Some(self.temperature),
                system: Some(self.system_prompt.clone()),
            };

            // Call provider
            let response = self.provider.complete(request).await.map_err(|e| {
                Temm1eError::Tool(format!("[{}] Provider error: {}", self.core_name, e))
            })?;

            // Record cost in shared budget
            let cost = budget::calculate_cost(
                response.usage.input_tokens,
                response.usage.output_tokens,
                &self.model_pricing,
            );
            self.budget.record_usage(
                response.usage.input_tokens,
                response.usage.output_tokens,
                cost,
            );
            total_input_tokens = total_input_tokens.saturating_add(response.usage.input_tokens);
            total_output_tokens = total_output_tokens.saturating_add(response.usage.output_tokens);
            total_cost += cost;

            // Parse response — separate text from tool calls
            let mut text_parts = Vec::new();
            let mut tool_uses = Vec::new();
            for part in &response.content {
                match part {
                    ContentPart::Text { text } => text_parts.push(text.clone()),
                    ContentPart::ToolUse {
                        id, name, input, ..
                    } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    _ => {}
                }
            }

            // If no tool calls, the core is done
            if tool_uses.is_empty() {
                let final_text = text_parts.join("\n");
                info!(
                    core = %self.core_name,
                    rounds,
                    input_tokens = total_input_tokens,
                    output_tokens = total_output_tokens,
                    cost_usd = format!("{:.4}", total_cost),
                    "TemDOS core completed"
                );
                return Ok(CoreResult {
                    output: final_text,
                    rounds,
                    input_tokens: total_input_tokens,
                    output_tokens: total_output_tokens,
                    cost_usd: total_cost,
                });
            }

            // Record assistant message (with tool calls) in history
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(response.content.clone()),
            });

            // Execute each tool call + VERIFY (failure tracking)
            let mut tool_result_parts = Vec::new();
            for (tool_use_id, tool_name, arguments) in &tool_uses {
                debug!(
                    core = %self.core_name,
                    tool = %tool_name,
                    round = rounds,
                    "Core executing tool"
                );

                let result =
                    execute_tool(tool_name, arguments.clone(), &self.tools, &session).await;

                let (mut content, is_error) = match result {
                    Ok(out) => (out.content, out.is_error),
                    Err(e) => (format!("Error: {e}"), true),
                };

                // VERIFY: track tool success/failure for strategy rotation
                if is_error {
                    failure_tracker.record_failure(tool_name, &content);

                    // Inject strategy rotation prompt after N consecutive failures
                    if let Some(rotation_prompt) = failure_tracker.format_rotation_prompt(tool_name)
                    {
                        debug!(
                            core = %self.core_name,
                            tool = %tool_name,
                            failures = failure_tracker.failure_count(tool_name),
                            "VERIFY: strategy rotation triggered"
                        );
                        content.push_str(&rotation_prompt);
                    }
                } else {
                    failure_tracker.record_success(tool_name);
                }

                tool_result_parts.push(ContentPart::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content,
                    is_error,
                });
            }

            // Record tool results in history
            session.history.push(ChatMessage {
                role: Role::Tool,
                content: MessageContent::Parts(tool_result_parts),
            });

            rounds += 1;
        }
    }

    /// Simple history pruning — keep messages within the context token budget.
    ///
    /// Unlike the main agent's priority-based context builder, this is
    /// straightforward: if total tokens exceed the budget, drop oldest
    /// messages (keeping the initial task message).
    fn prune_history(&self, history: &[ChatMessage]) -> Vec<ChatMessage> {
        if history.is_empty() {
            return Vec::new();
        }

        let mut total_tokens = 0;
        let mut keep_from = 0;

        // Estimate tokens for each message (newest first)
        let estimates: Vec<usize> = history
            .iter()
            .map(|msg| {
                let text = match &msg.content {
                    MessageContent::Text(t) => t.len(),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => text.len(),
                            ContentPart::ToolUse { input, .. } => input.to_string().len(),
                            ContentPart::ToolResult { content, .. } => content.len(),
                            _ => 0,
                        })
                        .sum(),
                };
                // Unicode-aware token estimate: count non-ASCII bytes from content
                let non_ascii: usize = match &msg.content {
                    MessageContent::Text(t) => t.as_bytes().iter().filter(|&&b| b > 127).count(),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => {
                                text.as_bytes().iter().filter(|&&b| b > 127).count()
                            }
                            _ => 0,
                        })
                        .sum(),
                };
                if text > 0 && non_ascii as f64 / text.max(1) as f64 > 0.3 {
                    text / 2
                } else {
                    text / 4
                }
            })
            .collect();

        // System prompt overhead (~estimate)
        let sys_non_ascii = self
            .system_prompt
            .as_bytes()
            .iter()
            .filter(|&&b| b > 127)
            .count();
        let system_overhead = if sys_non_ascii as f64 / self.system_prompt.len().max(1) as f64 > 0.3
        {
            self.system_prompt.len() / 2
        } else {
            self.system_prompt.len() / 4
        };
        let tool_defs_overhead = self.tools.len() * 100; // ~100 tokens per tool definition
        let available = self
            .max_context_tokens
            .saturating_sub(system_overhead)
            .saturating_sub(tool_defs_overhead);

        // Walk from newest to oldest, keeping what fits
        for (i, est) in estimates.iter().enumerate().rev() {
            if total_tokens + est > available && i > 0 {
                keep_from = i + 1;
                break;
            }
            total_tokens += est;
        }

        // Always keep the first message (the task) if we're dropping history
        if keep_from > 0 {
            let mut result = vec![history[0].clone()];
            result.extend_from_slice(&history[keep_from..]);
            result
        } else {
            history.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_history_keeps_all_when_small() {
        let runtime = CoreRuntime {
            system_prompt: "short prompt".to_string(),
            provider: Arc::new(MockProvider),
            tools: Vec::new(),
            budget: Arc::new(BudgetTracker::new(10.0)),
            model_pricing: ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
            },
            model: "test".to_string(),
            max_context_tokens: 30_000,
            core_name: "test".to_string(),
            temperature: 0.0,
        };

        let history = vec![
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hello".to_string()),
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text("Hi there".to_string()),
            },
        ];

        let pruned = runtime.prune_history(&history);
        assert_eq!(pruned.len(), 2);
    }

    // Minimal mock provider for unit tests
    struct MockProvider;

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<temm1e_core::types::message::CompletionResponse, Temm1eError> {
            Err(Temm1eError::Provider("Mock provider".to_string()))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<
            futures::stream::BoxStream<
                '_,
                Result<temm1e_core::types::message::StreamChunk, Temm1eError>,
            >,
            Temm1eError,
        > {
            Err(Temm1eError::Provider("Mock provider".to_string()))
        }

        async fn health_check(&self) -> Result<bool, Temm1eError> {
            Ok(true)
        }

        async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
            Ok(vec!["mock".to_string()])
        }
    }

    #[test]
    fn verify_failure_tracker_integrates_with_core() {
        // VERIFY: FailureTracker from self_correction.rs works in core context
        let mut tracker = FailureTracker::default();
        assert_eq!(tracker.max_failures, 2);

        // First failure — no rotation yet
        tracker.record_failure("shell", "command not found: foo");
        assert!(!tracker.should_rotate_strategy("shell"));
        assert!(tracker.format_rotation_prompt("shell").is_none());

        // Second failure — triggers strategy rotation
        tracker.record_failure("shell", "command not found: bar");
        assert!(tracker.should_rotate_strategy("shell"));

        let prompt = tracker.format_rotation_prompt("shell").unwrap();
        assert!(prompt.contains("STRATEGY ROTATION"));
        assert!(prompt.contains("Do NOT retry the same approach"));
        assert!(prompt.contains("3 alternative approaches"));

        // Success resets the tracker
        tracker.record_success("shell");
        assert!(!tracker.should_rotate_strategy("shell"));
        assert_eq!(tracker.failure_count("shell"), 0);
    }

    #[test]
    fn verify_tracks_per_tool_independently() {
        let mut tracker = FailureTracker::default();

        tracker.record_failure("shell", "err1");
        tracker.record_failure("shell", "err2");
        tracker.record_failure("file_read", "not found");

        // Shell hit threshold, file_read didn't
        assert!(tracker.should_rotate_strategy("shell"));
        assert!(!tracker.should_rotate_strategy("file_read"));

        // Success on shell doesn't affect file_read
        tracker.record_success("shell");
        assert!(!tracker.should_rotate_strategy("shell"));
        assert_eq!(tracker.failure_count("file_read"), 1);
    }
}
