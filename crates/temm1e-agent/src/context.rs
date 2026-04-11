//! Context builder — assembles a CompletionRequest from session history,
//! memory search results, system prompt, and tool definitions.
//!
//! Uses priority-based token budgeting to allocate the context window
//! surgically across categories:
//!   1. System prompt (always included)
//!   2. Tool definitions (always included)
//!   3. Current task state / DONE criteria (always included if present)
//!   4. Most recent 2–4 messages (always kept)
//!   5. Memory search results (up to 15% of budget)
//!   6. Cross-task learnings (up to 5% of budget)
//!   7. Older conversation history (fill remaining budget, newest first)
//!
//! When older messages are dropped, a brief summary is injected so the
//! LLM retains awareness of earlier context.

use std::sync::Arc;

use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role, ToolDefinition,
};
use temm1e_core::types::model_registry;
use temm1e_core::types::optimization::PromptTier;
use temm1e_core::types::session::SessionContext;
use temm1e_core::MemoryEntryType;
use temm1e_core::{Memory, SearchOpts, Tool};
use tracing::{debug, warn};

use crate::history_pruning::{group_into_turns, remove_orphaned_tool_results};
use crate::learning;
use crate::prompt_optimizer::build_tiered_system_prompt;
use crate::runtime::model_supports_vision;

/// Fraction of total context budget allocated to recent conversation history.
/// Skull-aligned: scales with model context window automatically.
/// 200K model → 50K tokens, 128K → 32K, 2M → 500K.
const RECENT_BUDGET_FRACTION: f32 = 0.25;

/// Absolute minimum: always keep last 2 messages (current query + response).
/// Ensures the current user query is never dropped regardless of budget.
const MIN_RECENT_MESSAGES: usize = 2;

/// Fraction of total budget reserved for memory search results.
const MEMORY_BUDGET_FRACTION: f32 = 0.15;

/// Fraction of total budget reserved for cross-task learnings.
const LEARNING_BUDGET_FRACTION: f32 = 0.05;

/// Estimate token count from a string.
///
/// For ASCII-heavy text (English, code): uses `len / 4` (~4 chars per token).
/// For non-ASCII-heavy text (CJK, Arabic): uses `len / 2` to avoid
/// underestimation that causes context overflow on multi-byte scripts.
pub(crate) fn estimate_tokens(s: &str) -> usize {
    let non_ascii = s.as_bytes().iter().filter(|&&b| b > 127).count();
    let ratio = non_ascii as f64 / s.len().max(1) as f64;
    if ratio > 0.3 {
        s.len() / 2
    } else {
        s.len() / 4
    }
}

/// Approximate token cost per image for vision models.
const IMAGE_TOKEN_ESTIMATE: usize = 1000;

/// Estimate token count for a ChatMessage.
fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    match &msg.content {
        MessageContent::Text(t) => estimate_tokens(t),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => estimate_tokens(text),
                ContentPart::ToolUse { input, .. } => estimate_tokens(&input.to_string()),
                ContentPart::ToolResult { content, .. } => estimate_tokens(content),
                ContentPart::Image { .. } => IMAGE_TOKEN_ESTIMATE,
            })
            .sum(),
    }
}

/// Build a CompletionRequest from all available context using priority-based
/// token budgeting.
///
/// `matched_blueprints` is the set of blueprints matching the classifier's
/// `blueprint_hint` (sorted by success rate, best first). The context builder
/// automatically selects the best one, injects its body/outline, and adds a
/// compact catalog section. Pass an empty slice when no blueprints matched.
#[allow(clippy::too_many_arguments)]
pub async fn build_context(
    session: &SessionContext,
    memory: &dyn Memory,
    tools: &[Arc<dyn Tool>],
    model: &str,
    system_prompt: Option<&str>,
    max_turns: usize,
    max_context_tokens: usize,
    prompt_tier: Option<PromptTier>,
    matched_blueprints: &[crate::blueprint::Blueprint],
    lambda_enabled: bool,
    personality: Option<&temm1e_anima::personality::PersonalityConfig>,
) -> CompletionRequest {
    let budget = max_context_tokens;

    // ── Category 1: System prompt ──────────────────────────────────
    let system = match prompt_tier {
        Some(tier) if system_prompt.is_none() => {
            // V2: Use tiered prompt from prompt_optimizer
            let config = temm1e_core::types::config::AgentConfig::default();
            let tool_refs: Vec<&dyn Tool> = tools.iter().map(|t| t.as_ref()).collect();
            Some(build_tiered_system_prompt(
                &config,
                &tool_refs,
                &session.workspace_path,
                false, // done_criteria handled separately
                tier,
                personality,
            ))
        }
        _ => build_system_prompt(system_prompt, tools, session, personality),
    };
    let system_tokens = system.as_ref().map_or(0, |s| estimate_tokens(s));

    // ── Category 2: Tool definitions ───────────────────────────────
    let tool_defs: Vec<ToolDefinition> = tools
        .iter()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();
    let tool_def_tokens: usize = tool_defs
        .iter()
        .map(|t| {
            estimate_tokens(&t.name)
                + estimate_tokens(&t.description)
                + estimate_tokens(&t.parameters.to_string())
        })
        .sum();

    // Fixed overhead (message framing, etc.)
    let overhead = 500;
    let fixed_tokens = system_tokens + tool_def_tokens + overhead;

    // ── Category 3b: Blueprints (up to 10% of budget) ──────────
    // Blueprint injection uses graceful degradation:
    //   - Best blueprint fits in 10% budget → inject full body
    //   - Best blueprint > 10% but < 25% of context → inject outline
    //   - Best blueprint > 25% of context → catalog only (no body)
    //   - Always inject catalog if any blueprints matched
    const BLUEPRINT_BUDGET_FRACTION: f32 = 0.10;
    let mut blueprint_messages: Vec<ChatMessage> = Vec::new();
    let mut blueprint_tokens_used = 0;
    let mut loaded_blueprint_id: Option<String> = None;

    if !matched_blueprints.is_empty() {
        let bp_budget = ((budget as f32) * BLUEPRINT_BUDGET_FRACTION) as usize;

        // Select best blueprint for body injection
        if let Some(best) =
            crate::blueprint::select_best_blueprint(matched_blueprints, bp_budget, budget)
        {
            if best.token_count <= bp_budget {
                // Full body fits in budget
                let bp_text = crate::blueprint::format_blueprint_context(best);
                let tokens = estimate_tokens(&bp_text);
                blueprint_messages.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(bp_text),
                });
                blueprint_tokens_used = tokens;
                loaded_blueprint_id = Some(best.id.clone());
                debug!(
                    name = %best.name,
                    version = best.version,
                    tokens = tokens,
                    "Blueprint full body injected into context"
                );
            } else {
                // Body too large for budget — inject outline instead
                let outline = crate::blueprint::format_blueprint_outline(best);
                let tokens = estimate_tokens(&outline);
                blueprint_messages.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(outline),
                });
                blueprint_tokens_used = tokens;
                loaded_blueprint_id = Some(best.id.clone());
                debug!(
                    name = %best.name,
                    token_count = best.token_count,
                    budget = bp_budget,
                    "Blueprint outline injected (full body too large)"
                );
            }
        }

        // Always inject the compact catalog so the LLM knows what's available
        let catalog = crate::blueprint::format_blueprint_catalog(
            matched_blueprints,
            loaded_blueprint_id.as_deref(),
        );
        if !catalog.is_empty() {
            let catalog_tokens = estimate_tokens(&catalog);
            blueprint_messages.push(ChatMessage {
                role: Role::System,
                content: MessageContent::Text(catalog),
            });
            blueprint_tokens_used += catalog_tokens;
        }
    }

    // ── Category 3: Task state / DONE criteria ─────────────────────
    // These are already in session.history as System messages injected by
    // the DONE Definition Engine. They will be included via the recent
    // messages or history pass, so we don't double-count them here.

    // ── Category 4: Recent messages (token-budgeted, skull-aligned) ──
    // v5.0: Dynamic budget fraction replaces hardcoded message counts.
    // Scales with model context window: 200K→50K, 128K→32K, 2M→500K.
    let history = &session.history;
    let recent_budget = ((budget as f32) * RECENT_BUDGET_FRACTION) as usize;

    // Walk backward from newest, keeping atomic turns (tool_use + tool_result) together.
    let all_turns = group_into_turns(history);
    let mut recent_indices: Vec<usize> = Vec::new();
    let mut recent_tokens: usize = 0;

    for turn in all_turns.iter().rev() {
        let turn_tokens: usize = turn
            .indices
            .iter()
            .map(|&i| estimate_message_tokens(&history[i]))
            .sum();
        if recent_tokens + turn_tokens > recent_budget
            && recent_indices.len() >= MIN_RECENT_MESSAGES
        {
            break;
        }
        recent_tokens += turn_tokens;
        recent_indices.extend_from_slice(&turn.indices);
    }
    recent_indices.sort_unstable();

    let recent_messages: Vec<ChatMessage> =
        recent_indices.iter().map(|&i| history[i].clone()).collect();
    let recent_start = recent_indices.first().copied().unwrap_or(history.len());

    let available_after_fixed_and_recent =
        budget.saturating_sub(fixed_tokens + recent_tokens + blueprint_tokens_used);

    // ── Category 5: λ-Memory (dynamic budget, replaces old Cat 5/5b/6) ──
    let query = extract_latest_query(history);
    let mut lambda_messages: Vec<ChatMessage> = Vec::new();
    let mut lambda_tokens_used = 0;

    // Also run legacy memory search + knowledge + learnings as fallback
    // (λ-Memory unifies these, but legacy entries still exist in memory_entries)
    let mut memory_tokens_used = 0;
    let mut knowledge_tokens_used = 0;
    let mut learning_tokens_used = 0;

    {
        // Get model skull size for dynamic budgeting
        let (skull, max_output) = model_registry::model_limits(model);
        let bone = fixed_tokens + blueprint_tokens_used;
        let lambda_max =
            crate::lambda_memory::lambda_budget(skull, max_output, bone, recent_tokens);
        let lambda_current = lambda_max.min(available_after_fixed_and_recent);

        let lambda_config = temm1e_core::types::config::LambdaMemoryConfig {
            enabled: lambda_enabled,
            ..Default::default()
        };

        let (lambda_text, tokens) = crate::lambda_memory::assemble_lambda_context(
            memory,
            lambda_current,
            lambda_max,
            &lambda_config,
            &query,
        )
        .await;

        if !lambda_text.is_empty() {
            lambda_messages.push(ChatMessage {
                role: Role::System,
                content: MessageContent::Text(lambda_text),
            });
            lambda_tokens_used = tokens;
        }
    }

    // Legacy fallback: if λ-Memory produced nothing, try old Category 5 search
    if lambda_tokens_used == 0 {
        let memory_budget = ((budget as f32) * MEMORY_BUDGET_FRACTION) as usize;
        let memory_budget = memory_budget.min(available_after_fixed_and_recent);

        if !query.is_empty() {
            let opts = SearchOpts {
                limit: 5,
                session_filter: Some(session.session_id.clone()),
                ..Default::default()
            };

            if let Ok(entries) = memory.search(&query, opts).await {
                if !entries.is_empty() {
                    let memory_text: String = entries
                        .iter()
                        .map(|e| {
                            format!("[{}] {}", e.timestamp.format("%Y-%m-%d %H:%M"), e.content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let tokens = estimate_tokens(&memory_text) + 10;
                    if tokens <= memory_budget {
                        lambda_messages.push(ChatMessage {
                            role: Role::System,
                            content: MessageContent::Text(format!(
                                "Relevant context from memory:\n{memory_text}",
                            )),
                        });
                        memory_tokens_used = tokens;
                    }
                }
            }
        }

        // Legacy Category 5b: knowledge entries
        {
            let knowledge_budget = memory_budget.saturating_sub(memory_tokens_used).min(2000);
            let knowledge_opts = SearchOpts {
                limit: 10,
                entry_type_filter: Some(MemoryEntryType::Knowledge),
                ..Default::default()
            };

            if let Ok(entries) = memory.search("", knowledge_opts).await {
                let knowledge_entries: Vec<_> = entries
                    .iter()
                    .filter(|e| matches!(e.entry_type, MemoryEntryType::Knowledge))
                    .collect();
                if !knowledge_entries.is_empty() {
                    let knowledge_text: String = knowledge_entries
                        .iter()
                        .map(|e| {
                            let key = e
                                .metadata
                                .get("user_key")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            format!("- {key}: {}", e.content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let tokens = estimate_tokens(&knowledge_text) + 10;
                    if tokens <= knowledge_budget && !knowledge_text.is_empty() {
                        lambda_messages.push(ChatMessage {
                            role: Role::System,
                            content: MessageContent::Text(format!(
                                "=== YOUR PERSISTENT KNOWLEDGE ===\n\
                                 These are facts you previously saved with memory_manage:\n\
                                 {knowledge_text}\n\
                                 === END KNOWLEDGE ===",
                            )),
                        });
                        knowledge_tokens_used = tokens;
                    }
                }
            }
        }

        // Category 6: learnings — scored by V(a,t) = Q × R × U
        if !query.is_empty() {
            let learning_budget = ((budget as f32) * LEARNING_BUDGET_FRACTION) as usize;
            let remaining_for_learnings = available_after_fixed_and_recent
                .saturating_sub(memory_tokens_used + knowledge_tokens_used);
            let learning_budget = learning_budget.min(remaining_for_learnings);

            // Fetch more candidates than needed, then score and take top 5
            let learning_opts = SearchOpts {
                limit: 50,
                session_filter: None,
                ..Default::default()
            };

            if let Ok(entries) = memory.search("learning:", learning_opts).await {
                let now = chrono::Utc::now();
                let mut scored: Vec<(f64, learning::TaskLearning)> = entries
                    .iter()
                    .filter_map(|e| serde_json::from_str(&e.content).ok())
                    .map(|l: learning::TaskLearning| {
                        let v = learning::learning_value(&l, now);
                        (v, l)
                    })
                    .filter(|(v, _)| *v >= 0.05) // GONE_THRESHOLD
                    .collect();

                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(5);

                let top_learnings: Vec<learning::TaskLearning> =
                    scored.into_iter().map(|(_, l)| l).collect();

                if !top_learnings.is_empty() {
                    let formatted = learning::format_learnings_context(&top_learnings);
                    let tokens = estimate_tokens(&formatted);
                    if tokens <= learning_budget && !formatted.is_empty() {
                        lambda_messages.push(ChatMessage {
                            role: Role::System,
                            content: MessageContent::Text(formatted),
                        });
                        learning_tokens_used = tokens;
                    }
                }
            }
        }
    }

    // ── Tool reliability injection (v4.6.0 self-learning) ──────────
    if let Ok(records) = memory.get_tool_reliability().await {
        let useful: Vec<_> = records
            .iter()
            .filter(|r| r.successes + r.failures >= 3) // skip low-sample
            .collect();
        if !useful.is_empty() {
            let mut lines = vec!["Tool reliability (last 30 days):".to_string()];
            for r in useful.iter().take(10) {
                let total = r.successes + r.failures;
                lines.push(format!(
                    "  {}: {} {:.0}% (N={})",
                    r.tool_name,
                    r.task_type,
                    r.success_rate() * 100.0,
                    total
                ));
            }
            let reliability_text = lines.join("\n");
            let tokens = estimate_tokens(&reliability_text);
            if tokens <= 100 {
                lambda_messages.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(reliability_text),
                });
            }
        }
    }

    // ── Category 7: Older conversation history ─────────────────────
    let used_tokens = fixed_tokens
        + recent_tokens
        + blueprint_tokens_used
        + lambda_tokens_used
        + memory_tokens_used
        + knowledge_tokens_used
        + learning_tokens_used;
    let history_budget = budget.saturating_sub(used_tokens);

    // Trim to max_turns first
    let older_end = recent_start;
    let older_history: Vec<ChatMessage> = if max_turns > 0 && older_end > max_turns * 2 {
        history[older_end - max_turns * 2..older_end].to_vec()
    } else {
        history[..older_end].to_vec()
    };

    // Group older messages into atomic turns (tool_use + tool_result are
    // indivisible) to prevent orphaned tool_result messages that cause
    // provider API errors.
    let turns = group_into_turns(&older_history);

    // Walk from newest to oldest turn, accumulate until budget exceeded.
    let mut kept_indices: Vec<usize> = Vec::new();
    let mut older_tokens_used = 0;

    for turn in turns.iter().rev() {
        let turn_tokens: usize = turn
            .indices
            .iter()
            .map(|&i| estimate_message_tokens(&older_history[i]))
            .sum();
        if older_tokens_used + turn_tokens > history_budget {
            break;
        }
        older_tokens_used += turn_tokens;
        kept_indices.extend_from_slice(&turn.indices);
    }

    // Sort indices to maintain original message order.
    kept_indices.sort_unstable();
    let kept_older: Vec<ChatMessage> = kept_indices
        .iter()
        .map(|&i| older_history[i].clone())
        .collect();
    let dropped_count = older_history.len() - kept_older.len();

    // If we dropped messages, inject a summary marker
    let mut summary_messages: Vec<ChatMessage> = Vec::new();
    if dropped_count > 0 {
        let summary = generate_dropped_summary(
            &history[..older_end.saturating_sub(kept_older.len())],
            dropped_count,
        );
        summary_messages.push(ChatMessage {
            role: Role::System,
            content: MessageContent::Text(summary),
        });
    }

    // ── Strip stale tool messages from older history ───────────────
    // Tool call/result pairs from previous sessions are ephemeral
    // execution artifacts that cause cross-provider format errors (e.g.,
    // Gemini requires strict ordering and a `name` field on tool results).
    // Strip them from older history — recent messages (current session)
    // are untouched.  Text parts in mixed messages are preserved.
    // See docs/CROSS_PROVIDER_HISTORY_SANITIZATION.md for full design.
    let mut kept_older = kept_older;
    strip_tool_messages_from_older(&mut kept_older);

    // ── Chat History Digest ────────────────────────────────────────
    // Extract a clean User ↔ Assistant conversation thread from the
    // full history (which is dominated by tool outputs). This is injected
    // as a System message so the LLM never loses track of what the human
    // actually said, even when tool outputs consume most of the context.
    let all_messages_for_digest: Vec<&ChatMessage> =
        kept_older.iter().chain(recent_messages.iter()).collect();
    let chat_digest = build_chat_digest(&all_messages_for_digest);

    // ── Assemble final message list ────────────────────────────────
    // Order: summary → chat digest → blueprint → λ-memory → older history → recent messages
    let mut messages: Vec<ChatMessage> = Vec::new();
    messages.extend(summary_messages);
    if let Some(digest_msg) = chat_digest {
        messages.push(digest_msg);
    }
    messages.extend(blueprint_messages);
    messages.extend(lambda_messages);
    messages.extend(kept_older);
    messages.extend(recent_messages);

    let total_tokens = fixed_tokens + messages.iter().map(estimate_message_tokens).sum::<usize>();

    let combined_memory_tokens =
        lambda_tokens_used + memory_tokens_used + knowledge_tokens_used + learning_tokens_used;
    debug!(
        system = system_tokens,
        tools = tool_def_tokens,
        blueprint = blueprint_tokens_used,
        recent = recent_tokens,
        lambda_memory = lambda_tokens_used,
        legacy_memory = memory_tokens_used + knowledge_tokens_used + learning_tokens_used,
        history = older_tokens_used,
        total = total_tokens,
        budget = budget,
        dropped = dropped_count,
        "Context budget allocation"
    );

    // ── Resource Budget Dashboard ───────────────────────────────
    // Inject into system prompt so the LLM sees its own resource
    // consumption and limits — the brain must know its skull size.
    let bp_budget_total = ((budget as f32) * BLUEPRINT_BUDGET_FRACTION) as usize;
    let bp_budget_remaining = bp_budget_total.saturating_sub(blueprint_tokens_used);
    let dashboard = format_budget_dashboard(
        model,
        budget,
        system_tokens,
        tool_def_tokens,
        blueprint_tokens_used,
        combined_memory_tokens,
        0, // learnings now part of λ-memory
        older_tokens_used + recent_tokens,
        bp_budget_remaining,
        bp_budget_total,
    );
    // Append dashboard to the system prompt
    let system = system.map(|s| format!("{s}\n\n{dashboard}"));

    // ── Vision safety: strip image parts for non-vision models ─────
    // If the model doesn't support vision, remove all ImageUrl parts
    // from every message (including old history) so the provider never
    // receives unsupported content types.
    if !model_supports_vision(model) {
        let mut stripped = 0usize;
        for msg in &mut messages {
            if let MessageContent::Parts(parts) = &mut msg.content {
                let before = parts.len();
                parts.retain(|p| !matches!(p, ContentPart::Image { .. }));
                stripped += before - parts.len();
                // If only text parts remain, flatten to Text for cleanliness
                if parts.len() == 1 {
                    if let Some(ContentPart::Text { text }) = parts.first().cloned() {
                        msg.content = MessageContent::Text(text);
                    }
                }
            }
        }
        if stripped > 0 {
            warn!(
                model = model,
                images_stripped = stripped,
                "Stripped image parts from conversation history — model has no vision"
            );
        }
    }

    // ── Safety net: remove orphaned tool_results ────────────────
    // After all pruning and stripping, ensure no tool_result references
    // a tool_use_id that isn't present. This prevents Anthropic 400
    // errors: "unexpected tool_use_id found in tool_result blocks".
    remove_orphaned_tool_results(&mut messages);

    // Use the model's actual max output token limit instead of hardcoding 4096
    let (_, model_max_output) = model_registry::model_limits(model);

    CompletionRequest {
        model: model.to_string(),
        messages,
        tools: tool_defs,
        max_tokens: Some(model_max_output as u32),
        temperature: Some(0.7),
        system,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Strip tool execution artifacts from older history messages.
///
/// Removes:
/// - `Role::Tool` messages entirely
/// - `ContentPart::ToolUse` and `ContentPart::ToolResult` parts from other messages
/// - Messages that become empty after part removal
///
/// Preserves:
/// - `ContentPart::Text` parts (the assistant's natural language)
/// - `ContentPart::Image` parts (handled separately by vision stripping)
/// - All `Role::User` messages unchanged
///
/// Follows the same retain-and-flatten pattern as image stripping (lines 477-503).
fn strip_tool_messages_from_older(messages: &mut Vec<ChatMessage>) {
    let before = messages.len();
    let mut tool_parts_stripped = 0usize;

    // Step 1: Remove Role::Tool messages entirely
    messages.retain(|msg| !matches!(msg.role, Role::Tool));

    // Step 2: Strip ToolUse/ToolResult parts from remaining messages
    for msg in messages.iter_mut() {
        if let MessageContent::Parts(parts) = &mut msg.content {
            let part_count_before = parts.len();
            parts.retain(|p| {
                !matches!(
                    p,
                    ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. }
                )
            });
            tool_parts_stripped += part_count_before - parts.len();

            // Flatten Parts([Text{...}]) → Text(...) when only text remains
            if parts.len() == 1 {
                if let Some(ContentPart::Text { text }) = parts.first().cloned() {
                    msg.content = MessageContent::Text(text);
                }
            }
        }
    }

    // Step 3: Remove messages that became empty after stripping
    messages.retain(|msg| match &msg.content {
        MessageContent::Text(t) => !t.is_empty(),
        MessageContent::Parts(parts) => !parts.is_empty(),
    });

    let removed = before - messages.len();
    if removed > 0 || tool_parts_stripped > 0 {
        debug!(
            messages_removed = removed,
            tool_parts_stripped = tool_parts_stripped,
            "Stripped stale tool messages from older history"
        );
    }
}

/// Build a chat history digest that separates human conversation from tool
/// execution logs. Returns `None` if there are fewer than 2 user messages
/// (no point summarizing a single exchange).
///
/// The digest extracts User and Assistant TEXT messages only, ignoring tool
/// calls, tool results, system injections, and images. This gives the LLM
/// a clean "what did the human say and what did I reply" view that doesn't
/// get buried under shell outputs, browser HTML, and file contents.
fn build_chat_digest(messages: &[&ChatMessage]) -> Option<ChatMessage> {
    let mut entries: Vec<String> = Vec::new();
    let mut user_count = 0;

    for msg in messages {
        let role_label = match msg.role {
            Role::User => {
                user_count += 1;
                "User"
            }
            Role::Assistant => "Assistant",
            _ => continue, // Skip System, Tool
        };

        // Extract text content only (skip tool_use, tool_result, images)
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
                    continue; // Skip messages that are pure tool_use / tool_result
                }
                texts.join(" ")
            }
        };

        // Skip empty or trivial messages
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Truncate long assistant replies to keep the digest compact
        let display = if role_label == "Assistant" && trimmed.len() > 200 {
            // Find a char boundary at or before byte 200
            let end = trimmed
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 200)
                .last()
                .unwrap_or(0);
            format!("{}...", &trimmed[..end])
        } else {
            trimmed.to_string()
        };

        entries.push(format!("{}: {}", role_label, display));
    }

    // Not worth injecting if the conversation is trivial
    if user_count < 2 {
        return None;
    }

    // Cap digest to last 30 exchanges to keep token cost bounded
    let max_entries = 30;
    let start = entries.len().saturating_sub(max_entries);
    let digest_text = entries[start..].join("\n");

    Some(ChatMessage {
        role: Role::System,
        content: MessageContent::Text(format!(
            "=== CHAT HISTORY (human conversation thread) ===\n\
             Below is the User ↔ Assistant conversation WITHOUT tool outputs.\n\
             Use this to stay grounded in what the user asked and what you replied.\n\
             The full tool execution logs follow in the message history below.\n\
             \n\
             {}\n\
             \n\
             === END CHAT HISTORY ===",
            digest_text
        )),
    })
}

/// Format the Resource Budget Dashboard for system prompt injection.
///
/// Shows the LLM its own resource consumption and limits so it can
/// make informed decisions about what to load (blueprints, memory, etc.).
#[allow(clippy::too_many_arguments)]
fn format_budget_dashboard(
    model: &str,
    total_limit: usize,
    system_tokens: usize,
    tool_tokens: usize,
    blueprint_tokens: usize,
    memory_tokens: usize,
    learning_tokens: usize,
    history_tokens: usize,
    blueprint_budget_remaining: usize,
    blueprint_budget_total: usize,
) -> String {
    let used = system_tokens
        + tool_tokens
        + blueprint_tokens
        + memory_tokens
        + learning_tokens
        + history_tokens;
    let available = total_limit.saturating_sub(used);

    format!(
        "=== CONTEXT BUDGET ===\n\
         Model: {model} | Limit: {total_limit} tokens\n\
         Used: {used} tokens\n\
         \x20 System:     {system_tokens}\n\
         \x20 Tools:      {tool_tokens}\n\
         \x20 Blueprint:  {blueprint_tokens}\n\
         \x20 Memory:     {memory_tokens}\n\
         \x20 Learnings:  {learning_tokens}\n\
         \x20 History:    {history_tokens}\n\
         Available: {available} tokens\n\
         Blueprint budget: {blueprint_budget_remaining} / {blueprint_budget_total} remaining\n\
         === END BUDGET ==="
    )
}

/// Extract the latest user query text from history.
fn extract_latest_query(history: &[ChatMessage]) -> String {
    history
        .iter()
        .rev()
        .find_map(|m| match &m.content {
            MessageContent::Text(t) => Some(t.clone()),
            MessageContent::Parts(parts) => parts.iter().find_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            }),
        })
        .unwrap_or_default()
}

/// Generate a brief summary of dropped messages for context continuity.
fn generate_dropped_summary(dropped_msgs: &[ChatMessage], count: usize) -> String {
    // Extract tool names used in dropped context
    let mut tools_used: Vec<String> = Vec::new();
    let mut topics: Vec<String> = Vec::new();

    for msg in dropped_msgs {
        match &msg.content {
            MessageContent::Text(t) => {
                if matches!(msg.role, Role::User) && t.len() > 5 {
                    // Take first ~50 bytes as a topic hint (safe on char boundary)
                    let topic = if t.len() > 50 {
                        let end = t
                            .char_indices()
                            .map(|(i, _)| i)
                            .take_while(|&i| i <= 50)
                            .last()
                            .unwrap_or(0);
                        &t[..end]
                    } else {
                        t
                    };
                    topics.push(topic.to_string());
                }
            }
            MessageContent::Parts(parts) => {
                for part in parts {
                    if let ContentPart::ToolUse { name, .. } = part {
                        if !tools_used.contains(name) {
                            tools_used.push(name.clone());
                        }
                    }
                }
            }
        }
    }

    let mut summary_parts = Vec::new();
    summary_parts.push(format!("[Earlier context: {} messages dropped", count));

    if !topics.is_empty() {
        let topic_str = topics
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        summary_parts.push(format!("discussed: {}", topic_str));
    }

    if !tools_used.is_empty() {
        summary_parts.push(format!("tools used: {}", tools_used.join(", ")));
    }

    format!("{}]", summary_parts.join(", "))
}

/// Build the system prompt, using a custom one or generating the default.
fn build_system_prompt(
    custom: Option<&str>,
    tools: &[Arc<dyn Tool>],
    session: &SessionContext,
    personality: Option<&temm1e_anima::personality::PersonalityConfig>,
) -> Option<String> {
    custom.map(|s| s.to_string()).or_else(|| {
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        let identity = if let Some(p) = personality {
            format!("{}\nYou are a cloud-native AI agent runtime. You control a computer through messaging apps.", p.generate_identity_section())
        } else {
            "You are TEMM1E, a cloud-native AI agent runtime. You control a computer through messaging apps.".to_string()
        };
        Some(format!(
            "{identity}\n\
             \n\
             You have access to these tools: {}\n\
             \n\
             Workspace: All file operations use the workspace directory at {}.\n\
             Files sent by the user are automatically saved here.\n\
             \n\
             File protocol:\n\
             - Received files are saved to the workspace automatically — use file_read to read them\n\
             - To send a file to the user, use send_file with just the path (chat_id is automatic)\n\
             - Use file_write to create files in the workspace, then send_file to deliver them\n\
             - Paths are relative to the workspace directory\n\
             \n\
             Guidelines:\n\
             - Use the shell tool to run commands, install packages, manage services, check system status\n\
             - Use file tools to read, write, and list files in the workspace\n\
             - Use web_fetch to look up documentation, check APIs, or research information\n\
             - Be concise in responses — the user is on a messaging app\n\
             - When a task requires multiple steps, execute them sequentially using tools\n\
             - If a command fails, read the error and try to fix it\n\
             - Never expose secrets, API keys, or sensitive data in responses\n\
             \n\
             Verification:\n\
             After every tool execution, you MUST verify the result before proceeding:\n\
             - Check that commands succeeded (exit code 0, expected output)\n\
             - Verify file operations by reading back what was written\n\
             - Test endpoints after deployment\n\
             - Never assume success — verify with evidence\n\
             \n\
             DONE criteria:\n\
             For compound tasks (multiple steps), define what DONE looks like before executing:\n\
             - List specific, verifiable conditions that must ALL be true when complete\n\
             - After completing all steps, verify each condition before declaring done\n\
             - Report completion with evidence for each condition\n\
             \n\
             Self-correction:\n\
             If an approach fails repeatedly, do NOT retry the same way:\n\
             - Analyze why the approach fails\n\
             - Generate alternative approaches\n\
             - Execute the most promising alternative\n\
             - If no alternatives exist, ask the user for guidance",
            tool_names.join(", "),
            session.workspace_path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_test_utils::{make_session, MockMemory, MockTool};

    #[tokio::test]
    async fn context_includes_system_prompt() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let session = make_session();

        let req = build_context(
            &session,
            &memory,
            &tools,
            "test-model",
            Some("Custom prompt"),
            6,
            30_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;
        // System prompt now includes the budget dashboard appended
        assert!(req.system.as_ref().unwrap().starts_with("Custom prompt"));
        assert_eq!(req.model, "test-model");
    }

    #[tokio::test]
    async fn context_default_system_prompt() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let session = make_session();

        let req = build_context(
            &session,
            &memory,
            &tools,
            "test-model",
            None,
            6,
            30_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;
        assert!(req.system.is_some());
        assert!(req.system.unwrap().contains("TEMM1E"));
    }

    #[tokio::test]
    async fn context_includes_tool_definitions() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(MockTool::new("shell")),
            Arc::new(MockTool::new("browser")),
        ];
        let session = make_session();

        let req = build_context(
            &session,
            &memory,
            &tools,
            "model",
            None,
            6,
            30_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;
        assert_eq!(req.tools.len(), 2);
        assert_eq!(req.tools[0].name, "shell");
        assert_eq!(req.tools[1].name, "browser");
    }

    #[tokio::test]
    async fn context_includes_conversation_history() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let mut session = make_session();
        session.history.push(ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Hello".to_string()),
        });
        session.history.push(ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("Hi there".to_string()),
        });

        let req = build_context(
            &session,
            &memory,
            &tools,
            "model",
            None,
            6,
            30_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;
        // Messages should include the history
        assert!(req.messages.len() >= 2);
    }

    #[tokio::test]
    async fn recent_messages_always_kept() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let mut session = make_session();

        // Add many messages
        for i in 0..20 {
            session.history.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(format!("Message {i}")),
            });
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(format!("Reply {i}")),
            });
        }

        // Use a very small budget to force dropping older messages
        let req = build_context(
            &session,
            &memory,
            &tools,
            "model",
            None,
            200,
            2_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;

        // The most recent messages should always be present
        let last_msg = req.messages.last().expect("messages should not be empty");
        match &last_msg.content {
            MessageContent::Text(t) => assert!(t.contains("Reply 19")),
            _ => panic!("Expected text message"),
        }
    }

    #[tokio::test]
    async fn dropped_messages_generate_summary() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let mut session = make_session();

        // Add many messages with enough content to exceed a small budget.
        // Each message is ~200 chars = ~50 tokens. 50 pairs = 100 messages = ~5000 tokens.
        let padding = "x".repeat(180);
        for i in 0..50 {
            session.history.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(format!("User message {i}: {padding}")),
            });
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(format!("Reply {i}: {padding}")),
            });
        }

        // Budget of 2000 tokens can't fit all 5000 tokens of messages + system prompt
        let req = build_context(
            &session,
            &memory,
            &tools,
            "model",
            None,
            200,
            2_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;

        // Check that a summary message was injected
        let has_summary = req.messages.iter().any(|m| {
            if let MessageContent::Text(t) = &m.content {
                t.contains("[Earlier context:")
            } else {
                false
            }
        });
        assert!(has_summary);
    }

    #[test]
    fn generate_dropped_summary_with_tools() {
        let msgs = vec![
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Deploy the application to production".to_string()),
            },
            ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                }]),
            },
        ];
        let summary = generate_dropped_summary(&msgs, 5);
        assert!(summary.contains("5 messages dropped"));
        assert!(summary.contains("Deploy"));
        assert!(summary.contains("shell"));
    }

    #[test]
    fn generate_dropped_summary_empty() {
        let summary = generate_dropped_summary(&[], 0);
        assert!(summary.contains("0 messages dropped"));
    }

    #[test]
    fn chat_digest_extracts_user_assistant_only() {
        let m1 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Deploy the app".to_string()),
        };
        let m2 = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: "t1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "docker build ."}),
                thought_signature: None,
            }]),
        };
        let m3 = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: "t1".to_string(),
                content: "Successfully built image abc123\nStep 1/10 : FROM node:20\n...lots of output...".to_string(),
                is_error: false,
            }]),
        };
        let m4 = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text("Done! The app is deployed.".to_string()),
        };
        let m5 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Great, now check the logs".to_string()),
        };

        let refs: Vec<&ChatMessage> = vec![&m1, &m2, &m3, &m4, &m5];
        let digest = build_chat_digest(&refs);
        assert!(digest.is_some());

        let text = match &digest
            .expect("digest should be Some for multi-message input")
            .content
        {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("Expected text"),
        };

        // Should contain user and assistant text
        assert!(text.contains("User: Deploy the app"));
        assert!(text.contains("Assistant: Done! The app is deployed."));
        assert!(text.contains("User: Great, now check the logs"));

        // Should NOT contain tool output
        assert!(!text.contains("docker build"));
        assert!(!text.contains("Successfully built"));
        assert!(!text.contains("abc123"));

        // Should have the section headers
        assert!(text.contains("CHAT HISTORY"));
    }

    #[test]
    fn chat_digest_skips_pure_tool_use_messages() {
        // An assistant message that is ONLY tool_use (no text) should be skipped
        let m1 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Run ls".to_string()),
        };
        let m2 = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: "t1".to_string(),
                name: "shell".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }]),
        };
        let m3 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Now run pwd".to_string()),
        };

        let refs: Vec<&ChatMessage> = vec![&m1, &m2, &m3];
        let digest = build_chat_digest(&refs);
        assert!(digest.is_some());

        let text = match &digest
            .expect("digest should be Some for multi-message input")
            .content
        {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("Expected text"),
        };

        // Should have user messages but no tool_use content
        assert!(text.contains("User: Run ls"));
        assert!(text.contains("User: Now run pwd"));
        assert!(!text.contains("shell"));
    }

    #[test]
    fn chat_digest_none_for_single_user_message() {
        let m1 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Hello".to_string()),
        };

        let refs: Vec<&ChatMessage> = vec![&m1];
        assert!(build_chat_digest(&refs).is_none());
    }

    #[test]
    fn chat_digest_truncates_long_assistant_replies() {
        let long_reply = "A".repeat(500);
        let m1 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Question 1".to_string()),
        };
        let m2 = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text(long_reply),
        };
        let m3 = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Question 2".to_string()),
        };

        let refs: Vec<&ChatMessage> = vec![&m1, &m2, &m3];
        let digest =
            build_chat_digest(&refs).expect("digest should be Some for multi-message input");

        let text = match &digest.content {
            MessageContent::Text(t) => t.clone(),
            _ => panic!("Expected text"),
        };

        // Assistant reply should be truncated to ~200 chars + "..."
        assert!(text.contains("..."));
        // But should NOT contain the full 500-char reply
        assert!(!text.contains(&"A".repeat(500)));
    }

    #[tokio::test]
    async fn context_includes_chat_digest_when_enough_messages() {
        let memory = MockMemory::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let mut session = make_session();

        // Simulate a realistic conversation with tool calls interleaved
        for i in 0..5 {
            session.history.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(format!("User request {i}")),
            });
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(vec![ContentPart::ToolUse {
                    id: format!("t{i}"),
                    name: "shell".to_string(),
                    input: serde_json::json!({"command": format!("cmd {i}")}),
                    thought_signature: None,
                }]),
            });
            session.history.push(ChatMessage {
                role: Role::Tool,
                content: MessageContent::Parts(vec![ContentPart::ToolResult {
                    tool_use_id: format!("t{i}"),
                    content: format!("output line {i}\nmore output\neven more output"),
                    is_error: false,
                }]),
            });
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Text(format!("Done with task {i}")),
            });
        }

        let req = build_context(
            &session,
            &memory,
            &tools,
            "model",
            None,
            200,
            100_000,
            None,
            &[],
            true, // lambda_enabled
            None, // personality
        )
        .await;

        // Should have a chat digest in the messages
        let has_digest = req.messages.iter().any(|m| {
            if let MessageContent::Text(t) = &m.content {
                t.contains("CHAT HISTORY")
            } else {
                false
            }
        });
        assert!(has_digest, "Expected chat digest in context messages");
    }

    // ── strip_tool_messages_from_older tests ──────────────────────

    fn tool_use_msg(name: &str, id: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![ContentPart::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({"cmd": "ls"}),
                thought_signature: None,
            }]),
        }
    }

    fn tool_result_msg(id: &str, output: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: id.to_string(),
                content: output.to_string(),
                is_error: false,
            }]),
        }
    }

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

    fn mixed_assistant_msg(text: &str, tool_name: &str, tool_id: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: text.to_string(),
                },
                ContentPart::ToolUse {
                    id: tool_id.to_string(),
                    name: tool_name.to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                },
            ]),
        }
    }

    #[test]
    fn strip_removes_tool_role_messages() {
        let mut messages = vec![
            user_msg("Hello"),
            assistant_msg("Let me check"),
            tool_use_msg("shell", "t1"),
            tool_result_msg("t1", "file.txt"),
            assistant_msg("Found file.txt"),
        ];

        strip_tool_messages_from_older(&mut messages);

        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0].role, Role::User));
        assert!(matches!(messages[2].role, Role::Assistant));
        // The pure tool_use assistant message should be removed (empty after strip)
        for msg in &messages {
            assert!(!matches!(msg.role, Role::Tool));
        }
    }

    #[test]
    fn strip_preserves_text_in_mixed_assistant() {
        let mut messages = vec![
            user_msg("Deploy"),
            mixed_assistant_msg("Let me check that for you", "shell", "t1"),
            tool_result_msg("t1", "success"),
            assistant_msg("Done"),
        ];

        strip_tool_messages_from_older(&mut messages);

        // 3 messages: user, assistant (text only), assistant
        assert_eq!(messages.len(), 3);
        // The mixed message should be flattened to Text
        match &messages[1].content {
            MessageContent::Text(t) => assert_eq!(t, "Let me check that for you"),
            MessageContent::Parts(_) => panic!("Expected Text, got Parts"),
        }
    }

    #[test]
    fn strip_removes_pure_tool_use_assistant() {
        let mut messages = vec![
            user_msg("Run ls"),
            tool_use_msg("shell", "t1"),
            tool_result_msg("t1", "files"),
            assistant_msg("Here are your files"),
        ];

        strip_tool_messages_from_older(&mut messages);

        // tool_use_msg becomes empty Parts after stripping → removed
        // tool_result_msg is Role::Tool → removed
        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0].role, Role::User));
        assert!(matches!(messages[1].role, Role::Assistant));
    }

    #[test]
    fn strip_preserves_user_messages() {
        let mut messages = vec![
            user_msg("Hello"),
            user_msg("How are you?"),
            assistant_msg("Good!"),
        ];

        strip_tool_messages_from_older(&mut messages);

        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn strip_empty_input() {
        let mut messages: Vec<ChatMessage> = vec![];
        strip_tool_messages_from_older(&mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn strip_no_tool_messages() {
        let mut messages = vec![
            user_msg("Hi"),
            assistant_msg("Hey!"),
            user_msg("What's up?"),
            assistant_msg("Not much"),
        ];

        strip_tool_messages_from_older(&mut messages);

        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn strip_preserves_image_parts() {
        let mut messages = vec![ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Here's what I found".to_string(),
                },
                ContentPart::ToolUse {
                    id: "t1".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                },
                ContentPart::Image {
                    media_type: "image/png".to_string(),
                    data: "abc123".to_string(),
                },
            ]),
        }];

        strip_tool_messages_from_older(&mut messages);

        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2); // Text + Image, ToolUse stripped
                assert!(matches!(&parts[0], ContentPart::Text { .. }));
                assert!(matches!(&parts[1], ContentPart::Image { .. }));
            }
            _ => panic!("Expected Parts"),
        }
    }

    #[test]
    fn strip_all_tool_messages_leaves_empty() {
        let mut messages = vec![
            tool_use_msg("shell", "t1"),
            tool_result_msg("t1", "output"),
            tool_use_msg("browser", "t2"),
            tool_result_msg("t2", "html"),
        ];

        strip_tool_messages_from_older(&mut messages);

        assert!(messages.is_empty());
    }
}
