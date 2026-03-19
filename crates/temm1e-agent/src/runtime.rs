//! AgentRuntime — main agent loop that processes messages through the
//! provider, executing tool calls in a loop until a final text reply.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use base64::Engine;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, ContentPart, InboundMessage, MessageContent, OutboundMessage, ParseMode, Role,
    TurnUsage,
};
use temm1e_core::types::model_registry;
use temm1e_core::types::session::SessionContext;
use temm1e_core::{Memory, Provider, Tool};
use tracing::{debug, info, warn};

/// Image MIME types that vision-capable models can process.
const IMAGE_MIME_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

use crate::model_router::{ModelRouter, ModelRouterConfig};
use crate::output_compression::compress_tool_output;
use temm1e_core::types::error::classify_tool_failure;
use temm1e_core::types::optimization::VerifyMode;

use temm1e_core::types::config::Temm1eMode;
use tokio::sync::RwLock;

use crate::agent_task_status::{AgentTaskPhase, AgentTaskStatus};
use crate::budget::{self, BudgetTracker, ModelPricing};
use crate::circuit_breaker::CircuitBreaker;
use crate::context::build_context;
use crate::done_criteria::{self, DoneCriteria};
use crate::executor::execute_tool;
use crate::learning;
use crate::prompted_tool_calling::{self, PromptedToolResult};
use crate::self_correction::FailureTracker;
use crate::task_queue::TaskQueue;

/// Shared runtime mode handle (same type used by mode_switch tool).
pub type SharedMode = Arc<RwLock<Temm1eMode>>;

/// Maximum characters per tool output (roughly ~8K tokens).
const MAX_TOOL_OUTPUT_CHARS: usize = 30_000;

/// Shared pending-message queue (same type as temm1e_tools::PendingMessages).
pub type PendingMessages = Arc<std::sync::Mutex<HashMap<String, Vec<String>>>>;

/// The core agent runtime. Holds references to the AI provider, memory backend,
/// and registered tools.
pub struct AgentRuntime {
    provider: Arc<dyn Provider>,
    memory: Arc<dyn Memory>,
    tools: Vec<Arc<dyn Tool>>,
    model: String,
    system_prompt: Option<String>,
    max_turns: usize,
    max_context_tokens: usize,
    max_tool_rounds: usize,
    max_task_duration: Duration,
    circuit_breaker: CircuitBreaker,
    /// Whether post-action verification hints are injected into tool results.
    verification_enabled: bool,
    /// Number of consecutive tool failures before triggering strategy rotation.
    max_consecutive_failures: usize,
    /// Optional persistent task queue for checkpointing (None = no persistence).
    task_queue: Option<Arc<TaskQueue>>,
    /// Per-session budget tracker.
    budget: BudgetTracker,
    /// Pricing for the current model.
    model_pricing: ModelPricing,
    /// Whether v2 Tem's Mind optimizations are enabled.
    /// When true: complexity classification, prompt stratification,
    /// structured failure injection, and trivial fast-path.
    v2_optimizations: bool,
    /// Whether hive swarm routing is enabled. When true and classifier says
    /// Order+Complex, process_message returns HiveRoute error for main.rs to catch.
    hive_enabled: bool,
    /// Whether executable DAG blueprint phase parallelism is enabled.
    /// When true and a blueprint is matched, its phases are parsed into a
    /// dependency graph and independent phases execute concurrently.
    /// When false (default), blueprints are injected as context text.
    /// This flag does NOT affect tool-level parallelism in executor.rs.
    parallel_phases: bool,
    /// Shared personality mode (PLAY or WORK). When set, the current mode is
    /// injected into the system prompt on every request so the LLM adapts
    /// its voice accordingly. Updated at runtime by the mode_switch tool.
    shared_mode: Option<SharedMode>,
    /// Shared memory strategy (Lambda or Echo). Updated at runtime by /memory command.
    shared_memory_strategy: Option<Arc<RwLock<temm1e_core::types::config::MemoryStrategy>>>,
}

impl AgentRuntime {
    /// Create a new AgentRuntime.
    pub fn new(
        provider: Arc<dyn Provider>,
        memory: Arc<dyn Memory>,
        tools: Vec<Arc<dyn Tool>>,
        model: String,
        system_prompt: Option<String>,
    ) -> Self {
        let model_pricing = budget::get_pricing(&model);
        Self {
            provider,
            memory,
            tools,
            model,
            system_prompt,
            max_turns: 200,
            max_context_tokens: 30_000,
            max_tool_rounds: 200,
            max_task_duration: Duration::from_secs(1800),
            circuit_breaker: CircuitBreaker::default(),
            verification_enabled: true,
            max_consecutive_failures: 2,
            task_queue: None,
            budget: BudgetTracker::new(0.0),
            hive_enabled: false,
            model_pricing,
            v2_optimizations: true,
            parallel_phases: false,
            shared_mode: None,
            shared_memory_strategy: None,
        }
    }

    /// Create a new AgentRuntime with custom context limits.
    ///
    /// The `max_context_tokens` is automatically capped to the model's actual
    /// context window (minus output headroom) using the model registry. This
    /// prevents overflow for small-context models like Qwen 2.5 7B (32K).
    #[allow(clippy::too_many_arguments)]
    pub fn with_limits(
        provider: Arc<dyn Provider>,
        memory: Arc<dyn Memory>,
        tools: Vec<Arc<dyn Tool>>,
        model: String,
        system_prompt: Option<String>,
        max_turns: usize,
        max_context_tokens: usize,
        max_tool_rounds: usize,
        max_task_duration_secs: u64,
        max_spend_usd: f64,
    ) -> Self {
        let model_pricing = budget::get_pricing(&model);

        // Cap max_context_tokens to the model's actual context window minus
        // output token headroom. This prevents trying to fill 30K tokens of
        // input into a model that only has 32K total (e.g. qwen-2.5-7b).
        // A 10% safety margin absorbs token estimation errors (estimate_tokens()
        // uses len/4 which can underestimate by ~20% on code/CJK text).
        // Floor at context_window/2 for models where output == context (e.g. phi-4).
        let (model_ctx_window, model_max_output) = model_registry::model_limits(&model);
        let raw_input_budget = model_ctx_window.saturating_sub(model_max_output);
        let min_input_budget = model_ctx_window / 2;
        let model_input_budget = raw_input_budget.max(min_input_budget) * 9 / 10;
        let effective_context = max_context_tokens.min(model_input_budget);

        if effective_context < max_context_tokens {
            info!(
                model = %model,
                configured = max_context_tokens,
                effective = effective_context,
                model_context_window = model_ctx_window,
                model_max_output = model_max_output,
                "Adjusted max_context_tokens to fit model's context window"
            );
        }

        Self {
            provider,
            memory,
            tools,
            model,
            system_prompt,
            max_turns,
            max_context_tokens: effective_context,
            max_tool_rounds,
            max_task_duration: Duration::from_secs(max_task_duration_secs),
            circuit_breaker: CircuitBreaker::default(),
            verification_enabled: true,
            max_consecutive_failures: 2,
            task_queue: None,
            budget: BudgetTracker::new(max_spend_usd),
            hive_enabled: false,
            model_pricing,
            v2_optimizations: true,
            parallel_phases: false,
            shared_mode: None,
            shared_memory_strategy: None,
        }
    }

    /// Set the shared personality mode (PLAY/WORK). The current mode is
    /// injected into the system prompt on every request.
    pub fn with_shared_mode(mut self, mode: SharedMode) -> Self {
        self.shared_mode = Some(mode);
        self
    }

    /// Set the shared personality mode from an Option (convenience for propagation).
    pub fn with_shared_mode_opt(mut self, mode: Option<SharedMode>) -> Self {
        self.shared_mode = mode;
        self
    }

    /// Set the shared memory strategy handle (updated by /memory command).
    pub fn with_shared_memory_strategy(
        mut self,
        strategy: Arc<RwLock<temm1e_core::types::config::MemoryStrategy>>,
    ) -> Self {
        self.shared_memory_strategy = Some(strategy);
        self
    }

    /// Set the persistent task queue for checkpointing.
    pub fn with_task_queue(mut self, task_queue: Arc<TaskQueue>) -> Self {
        self.task_queue = Some(task_queue);
        self
    }

    /// Enable or disable v2 Tem's Mind optimizations.
    /// When enabled: complexity-aware prompt tiers, trivial fast-path,
    /// structured failure classification, and complexity-scaled output caps.
    pub fn with_v2_optimizations(mut self, enabled: bool) -> Self {
        self.v2_optimizations = enabled;
        self
    }

    /// Enable hive swarm routing: when classifier says Order+Complex,
    /// process_message returns HiveRoute instead of running the tool loop.
    pub fn with_hive_enabled(mut self, enabled: bool) -> Self {
        self.hive_enabled = enabled;
        self
    }

    /// Check whether v2 optimizations are enabled.
    pub fn v2_enabled(&self) -> bool {
        self.v2_optimizations
    }

    /// Enable or disable executable DAG blueprint phase parallelism.
    /// When enabled, matched blueprint phases are parsed into a TaskGraph
    /// and independent phases execute concurrently. Does NOT affect
    /// tool-level parallelism in executor.rs.
    pub fn with_parallel_phases(mut self, enabled: bool) -> Self {
        self.parallel_phases = enabled;
        self
    }

    /// Check whether parallel phase execution is enabled.
    pub fn parallel_phases_enabled(&self) -> bool {
        self.parallel_phases
    }

    /// Process an inbound message through the full agent loop.
    ///
    /// - `interrupt`: if set to `true` by another task, the tool loop exits
    ///   early so the dispatcher can serve a higher-priority message.
    /// - `pending`: shared queue of user messages that arrived while this task
    ///   is running. Pending texts are automatically appended to the last tool
    ///   result each round so the LLM sees them without extra API calls.
    /// - `status_tx`: optional `watch` channel for real-time task status emission.
    ///   If `None`, no status is emitted (zero overhead). `send_modify` is infallible.
    /// - `cancel`: optional `CancellationToken` for future mid-stream cancellation.
    ///   Phase 1: created and cancelled alongside `interrupt`, but not yet awaited
    ///   in the loop. Phase 2 will add `tokio::select!` on provider calls.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message(
        &self,
        msg: &InboundMessage,
        session: &mut SessionContext,
        interrupt: Option<Arc<AtomicBool>>,
        pending: Option<PendingMessages>,
        reply_tx: Option<tokio::sync::mpsc::UnboundedSender<OutboundMessage>>,
        status_tx: Option<tokio::sync::watch::Sender<AgentTaskStatus>>,
        cancel: Option<CancellationToken>,
    ) -> Result<(OutboundMessage, TurnUsage), Temm1eError> {
        info!(
            channel = %msg.channel,
            chat_id = %msg.chat_id,
            user_id = %msg.user_id,
            "Processing inbound message"
        );

        // ── Status emission helper ──────────────────────────────
        // Infallible: send_modify never panics, never allocates.
        // If status_tx is None, the closure is a no-op (zero overhead).
        // We capture `cancel` here only for future Phase 2 use.
        let _cancel = cancel; // bind to suppress unused-variable warning

        // Per-turn usage accumulators
        let mut turn_api_calls: u32 = 0;
        let mut turn_input_tokens: u32 = 0;
        let mut turn_output_tokens: u32 = 0;
        let mut turn_tools_used: u32 = 0;
        let mut turn_cost_usd: f64 = 0.0;

        // Build user text — include attachment descriptions if no text provided
        let mut user_text = match (&msg.text, msg.attachments.is_empty()) {
            (Some(t), _) if !t.trim().is_empty() => t.clone(),
            (_, false) => {
                let descs: Vec<String> = msg
                    .attachments
                    .iter()
                    .map(|a| {
                        let name = a.file_name.as_deref().unwrap_or("file");
                        let mime = a.mime_type.as_deref().unwrap_or("unknown type");
                        format!("[Attached: {} ({})]", name, mime)
                    })
                    .collect();
                descs.join(" ")
            }
            _ => {
                return Ok((
                    OutboundMessage {
                        chat_id: msg.chat_id.clone(),
                        text: "I received an empty message. Please send some text or a file."
                            .to_string(),
                        reply_to: Some(msg.id.clone()),
                        parse_mode: None,
                    },
                    TurnUsage::default(),
                ));
            }
        };
        let detected_creds = temm1e_vault::detect_credentials(&user_text);
        if !detected_creds.is_empty() {
            warn!(
                count = detected_creds.len(),
                "Detected credentials in user message — they will be noted but not stored in plain text history"
            );
            for cred in &detected_creds {
                debug!(
                    provider = %cred.provider,
                    key = %cred.key,
                    "Detected credential"
                );
            }
        }

        // ── Status: Preparing ────────────────────────────────────
        // Emit after user text parsed and credentials scanned.
        if let Some(ref tx) = status_tx {
            tx.send_modify(|s| {
                s.phase = AgentTaskPhase::Preparing;
            });
        }

        // ── Vision: load image attachments ──────────────────────────
        // If the inbound message has image attachments, read them from the
        // workspace, base64-encode, and include as Image content parts so
        // the LLM can see them.
        let mut image_parts: Vec<ContentPart> = Vec::new();
        for att in &msg.attachments {
            let mime = att.mime_type.as_deref().unwrap_or("");
            if !IMAGE_MIME_TYPES.contains(&mime) {
                continue;
            }
            let file_name = match &att.file_name {
                Some(n) => n.clone(),
                None => continue,
            };
            let file_path = session.workspace_path.join(&file_name);
            match tokio::fs::read(&file_path).await {
                Ok(data) => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                    info!(
                        file = %file_name,
                        mime = %mime,
                        size_bytes = data.len(),
                        "Loaded image attachment for vision"
                    );
                    image_parts.push(ContentPart::Image {
                        media_type: mime.to_string(),
                        data: encoded,
                    });
                }
                Err(e) => {
                    warn!(
                        file = %file_name,
                        error = %e,
                        "Failed to read image attachment from workspace"
                    );
                }
            }
        }

        // ── Vision capability check ──────────────────────────────
        // If the user sent images but the current model doesn't support
        // vision, strip the images and prepend a notice so the user gets
        // a helpful message instead of an API error.
        if !image_parts.is_empty() && !model_supports_vision(&self.model) {
            let count = image_parts.len();
            image_parts.clear();
            let notice = format!(
                "[{} image(s) received but your current model ({}) does not support vision. \
                 Switch to a vision-capable model to analyze images. \
                 Examples: claude-sonnet-4-6, gpt-5.2, gemini-3-flash-preview, glm-4.6v-flash]",
                count, self.model
            );
            warn!(
                model = %self.model,
                images_stripped = count,
                "Images stripped — model does not support vision"
            );
            user_text = format!("{}\n\n{}", notice, user_text);
        }

        // Append the user message to session history FIRST (before classification,
        // so chat early-returns have the message in history for persistence).
        if image_parts.is_empty() {
            session.history.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_text.clone()),
            });
        } else {
            let mut parts = vec![ContentPart::Text {
                text: user_text.clone(),
            }];
            parts.extend(image_parts);
            session.history.push(ChatMessage {
                role: Role::User,
                content: MessageContent::Parts(parts),
            });
        }

        // ── Status: Classifying ──────────────────────────────────
        if let Some(ref tx) = status_tx {
            tx.send_modify(|s| {
                s.phase = AgentTaskPhase::Classifying;
            });
        }

        // ── Blueprint Categories (pre-classifier) ───────────────────
        // Fetch the grounded set of categories from stored blueprints.
        // These feed into the classifier so it can emit a blueprint_hint
        // without an extra LLM call.
        let blueprint_categories =
            crate::blueprint::fetch_available_categories(self.memory.as_ref()).await;
        let mut blueprint_hint: Option<String> = None;

        // ── V2 LLM Classification ─────────────────────────────────────
        // Classify the message as "chat" or "order" using a fast LLM call.
        //   Chat  → return immediately with the LLM's response (1 call total).
        //   Order → send acknowledgment via reply_tx, then run the agentic pipeline.
        let execution_profile = if self.v2_optimizations {
            // Read current mode for classifier voice injection
            let current_mode = match &self.shared_mode {
                Some(m) => *m.read().await,
                None => Temm1eMode::Play,
            };
            match crate::llm_classifier::classify_message(
                self.provider.as_ref(),
                &self.model,
                &user_text,
                &session.history,
                &blueprint_categories,
                current_mode,
            )
            .await
            {
                Ok((classification, classify_usage)) => {
                    // Record classification call in per-turn accumulators
                    let classify_cost = crate::budget::calculate_cost(
                        classify_usage.input_tokens,
                        classify_usage.output_tokens,
                        &self.model_pricing,
                    );
                    turn_api_calls = turn_api_calls.saturating_add(1);
                    turn_input_tokens =
                        turn_input_tokens.saturating_add(classify_usage.input_tokens);
                    turn_output_tokens =
                        turn_output_tokens.saturating_add(classify_usage.output_tokens);
                    turn_cost_usd += classify_cost;
                    self.budget.record_usage(
                        classify_usage.input_tokens,
                        classify_usage.output_tokens,
                        classify_cost,
                    );

                    info!(
                        category = ?classification.category,
                        difficulty = ?classification.difficulty,
                        "V2: LLM classified message"
                    );

                    match classification.category {
                        crate::llm_classifier::MessageCategory::Chat => {
                            // ── Chat: return immediately ─────────────────
                            // Push assistant reply to history for persistence.
                            session.history.push(ChatMessage {
                                role: Role::Assistant,
                                content: MessageContent::Text(classification.chat_text.clone()),
                            });

                            return Ok((
                                OutboundMessage {
                                    chat_id: msg.chat_id.clone(),
                                    text: classification.chat_text,
                                    reply_to: Some(msg.id.clone()),
                                    parse_mode: None,
                                },
                                TurnUsage {
                                    api_calls: turn_api_calls,
                                    input_tokens: turn_input_tokens,
                                    output_tokens: turn_output_tokens,
                                    tools_used: 0,
                                    total_cost_usd: turn_cost_usd,
                                    provider: self.provider.name().to_string(),
                                    model: self.model.clone(),
                                },
                            ));
                        }
                        crate::llm_classifier::MessageCategory::Stop => {
                            // ── Stop: acknowledge and return immediately ──
                            // The caller (main.rs dispatcher) handles the
                            // actual interrupt of any active task. Here we
                            // just return the LLM's short acknowledgement.
                            info!("V2: LLM classified as Stop — returning ack");
                            session.history.push(ChatMessage {
                                role: Role::Assistant,
                                content: MessageContent::Text(classification.chat_text.clone()),
                            });

                            return Ok((
                                OutboundMessage {
                                    chat_id: msg.chat_id.clone(),
                                    text: classification.chat_text,
                                    reply_to: Some(msg.id.clone()),
                                    parse_mode: Some(ParseMode::Plain),
                                },
                                TurnUsage {
                                    api_calls: turn_api_calls,
                                    input_tokens: turn_input_tokens,
                                    output_tokens: turn_output_tokens,
                                    tools_used: 0,
                                    total_cost_usd: turn_cost_usd,
                                    provider: self.provider.name().to_string(),
                                    model: self.model.clone(),
                                },
                            ));
                        }
                        crate::llm_classifier::MessageCategory::Order => {
                            // ── Hive route: if enabled and Complex, signal swarm ──
                            if self.hive_enabled
                                && classification.difficulty
                                    == crate::llm_classifier::TaskDifficulty::Complex
                            {
                                info!("V2: Complex order + hive enabled → routing to swarm");
                                return Err(Temm1eError::HiveRoute(
                                    msg.text.clone().unwrap_or_default(),
                                ));
                            }

                            // ── Order: send ack, then continue pipeline ──
                            // Extract blueprint hint from classifier (v2 matching)
                            blueprint_hint = classification.blueprint_hint.clone();
                            if let Some(ref hint) = blueprint_hint {
                                info!(hint = %hint, "Classifier provided blueprint hint");
                            }
                            if let Some(ref tx) = reply_tx {
                                let ack = OutboundMessage {
                                    chat_id: msg.chat_id.clone(),
                                    text: classification.chat_text,
                                    reply_to: Some(msg.id.clone()),
                                    parse_mode: None,
                                };
                                if let Err(e) = tx.send(ack) {
                                    warn!(error = %e, "Failed to send early reply for order");
                                }
                            }
                            Some(classification.difficulty.execution_profile())
                        }
                    }
                }
                Err(e) => {
                    // Fallback to rule-based classification
                    warn!(error = %e, "LLM classification failed, using rule-based fallback");
                    let router = ModelRouter::new(ModelRouterConfig::default());
                    let complexity = router.classify_complexity(&session.history, &[], &user_text);

                    // If hive enabled and fallback says Complex → route to swarm
                    if self.hive_enabled
                        && matches!(complexity, crate::model_router::TaskComplexity::Complex)
                    {
                        info!(
                            "V2: Fallback classified as Complex + hive enabled → routing to swarm"
                        );
                        return Err(Temm1eError::HiveRoute(msg.text.clone().unwrap_or_default()));
                    }

                    let profile = complexity.execution_profile();
                    info!(
                        complexity = ?complexity,
                        prompt_tier = ?profile.prompt_tier,
                        max_iterations = profile.max_iterations,
                        "V2: Rule-based fallback classification"
                    );
                    Some(profile)
                }
            }
        } else {
            None
        };

        // ── DONE Definition Engine ─────────────────────────────────
        // Detect compound tasks and inject a DONE criteria prompt so
        // the LLM articulates verifiable completion conditions.
        let is_compound = done_criteria::is_compound_task(&user_text);
        let mut _done_criteria = DoneCriteria::new();

        if is_compound {
            info!("Compound task detected — injecting DONE criteria prompt");
            let done_prompt = done_criteria::format_done_prompt(&user_text);
            session.history.push(ChatMessage {
                role: Role::System,
                content: MessageContent::Text(done_prompt),
            });
        }

        // ── Persistent Task Queue ──────────────────────────────────
        // Create a task entry if the queue is available.
        let task_id = if let Some(ref tq) = self.task_queue {
            match tq.create_task(&msg.chat_id, &user_text).await {
                Ok(id) => {
                    info!(task_id = %id, "Task created in persistent queue");
                    if let Err(e) = tq
                        .update_status(&id, crate::task_queue::TaskStatus::Running)
                        .await
                    {
                        warn!(error = %e, "Failed to update task status to Running");
                    }
                    Some(id)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create task in queue — continuing without persistence");
                    None
                }
            }
        } else {
            None
        };

        // ── Blueprint Matching (v2: category-based, zero extra LLM calls) ─
        // Use the classifier's blueprint_hint to fetch blueprints by category.
        // The context builder handles selection, catalog, and budget enforcement.
        let matched_blueprints: Vec<crate::blueprint::Blueprint> =
            if let Some(ref hint) = blueprint_hint {
                crate::blueprint::fetch_by_category(self.memory.as_ref(), hint).await
            } else {
                Vec::new()
            };

        // Identify the "active" blueprint (best match) for post-task refinement.
        let active_blueprint: Option<crate::blueprint::Blueprint> =
            if !matched_blueprints.is_empty() {
                crate::blueprint::select_best_blueprint(
                    &matched_blueprints,
                    ((self.max_context_tokens as f32) * 0.10) as usize,
                    self.max_context_tokens,
                )
                .cloned()
            } else {
                None
            };

        // ── Self-Correction Engine ─────────────────────────────────
        // Track consecutive tool failures per tool name.
        let mut failure_tracker = FailureTracker::new(self.max_consecutive_failures);

        // ── Prompted Tool Calling Fallback ────────────────────────────
        // When native tool calling fails (provider 400), we switch to
        // prompted mode: tool definitions go into the system prompt and
        // the model outputs JSON instead of native tool_calls.
        let mut prompted_mode = false;
        let mut prompted_json_retries: u8 = 0;
        const MAX_PROMPTED_JSON_RETRIES: u8 = 1;

        // Tool-use loop
        let task_start = Instant::now();
        let mut rounds: usize = 0;
        let mut interrupted = false;
        // Track if send_message tool was used — suppresses the final reply
        // to avoid duplicating content already delivered to the user.
        let mut send_message_used = false;
        loop {
            rounds += 1;

            // Check for preemption between rounds
            if let Some(ref flag) = interrupt {
                if flag.load(Ordering::Relaxed) {
                    info!(
                        "Agent interrupted by higher-priority message after {} rounds",
                        rounds - 1
                    );
                    // ── Status: Interrupted ──────────────────────
                    if let Some(ref tx) = status_tx {
                        tx.send_modify(|s| {
                            s.phase = AgentTaskPhase::Interrupted {
                                round: rounds as u32,
                            };
                        });
                    }
                    interrupted = true;
                    break;
                }
            }

            if task_start.elapsed() > self.max_task_duration {
                warn!(
                    elapsed_secs = task_start.elapsed().as_secs(),
                    limit_secs = self.max_task_duration.as_secs(),
                    "Task duration exceeded limit, forcing text reply"
                );
                break;
            }

            if rounds > self.max_tool_rounds {
                warn!(
                    "Exceeded maximum tool rounds ({}), forcing text reply",
                    self.max_tool_rounds
                );
                break;
            }

            // Build the completion request from full context
            let prompt_tier = execution_profile.as_ref().map(|p| p.prompt_tier);
            let lambda_enabled = match &self.shared_memory_strategy {
                Some(strategy) => {
                    *strategy.read().await == temm1e_core::types::config::MemoryStrategy::Lambda
                }
                None => false, // default: Echo Memory (user opts into λ-Memory via /memory lambda)
            };
            let mut request = build_context(
                session,
                self.memory.as_ref(),
                &self.tools,
                &self.model,
                self.system_prompt.as_deref(),
                self.max_turns,
                self.max_context_tokens,
                prompt_tier,
                &matched_blueprints,
                lambda_enabled,
            )
            .await;

            // ── Personality mode injection ──────────────────────────────
            if let Some(ref shared_mode) = self.shared_mode {
                let mode = *shared_mode.read().await;
                let mode_block = mode_prompt_block(mode);
                request.system = Some(match request.system {
                    Some(existing) => format!("{mode_block}\n\n{existing}"),
                    None => mode_block,
                });
            }

            // ── Prompted mode: move tools from API body into system prompt ──
            if prompted_mode && !request.tools.is_empty() {
                let tool_prompt = prompted_tool_calling::format_tools_prompt(&request.tools);
                request.system = Some(match request.system {
                    Some(existing) => format!("{existing}{tool_prompt}"),
                    None => tool_prompt,
                });
                // If this is a JSON retry, append the stricter instruction
                if prompted_json_retries > 0 {
                    let retry_hint = prompted_tool_calling::format_strict_retry_prompt();
                    request.system = request.system.map(|s| format!("{s}\n\n{retry_hint}"));
                }
                request.tools.clear();
                debug!(
                    round = rounds,
                    "Prompted tool-calling mode: tools in system prompt"
                );
            }

            debug!(
                round = rounds,
                messages = request.messages.len(),
                prompted_mode,
                "Sending completion request"
            );

            // Check budget before calling provider
            if let Err(budget_err) = self.budget.check_budget() {
                return Ok((
                    OutboundMessage {
                        chat_id: msg.chat_id.clone(),
                        text: budget_err,
                        reply_to: Some(msg.id.clone()),
                        parse_mode: Some(ParseMode::Plain),
                    },
                    TurnUsage {
                        api_calls: turn_api_calls,
                        input_tokens: turn_input_tokens,
                        output_tokens: turn_output_tokens,
                        tools_used: turn_tools_used,
                        total_cost_usd: turn_cost_usd,
                        provider: self.provider.name().to_string(),
                        model: self.model.clone(),
                    },
                ));
            }

            // Check circuit breaker before calling provider
            if !self.circuit_breaker.can_execute() {
                warn!("Circuit breaker is open — provider appears to be down");
                return Ok((
                    OutboundMessage {
                        chat_id: msg.chat_id.clone(),
                        text: "The AI provider is currently unavailable. I'll retry automatically when it recovers.".to_string(),
                        reply_to: Some(msg.id.clone()),
                        parse_mode: Some(ParseMode::Plain),
                    },
                    TurnUsage {
                        api_calls: turn_api_calls,
                        input_tokens: turn_input_tokens,
                        output_tokens: turn_output_tokens,
                        tools_used: turn_tools_used,
                        total_cost_usd: turn_cost_usd,
                        provider: self.provider.name().to_string(),
                        model: self.model.clone(),
                    },
                ));
            }

            // ── Status: CallingProvider ─────────────────────────────
            if let Some(ref tx) = status_tx {
                tx.send_modify(|s| {
                    s.phase = AgentTaskPhase::CallingProvider {
                        round: rounds as u32,
                    };
                });
            }

            // Track whether the original request had tools (for fallback detection)
            let request_had_tools = !self.tools.is_empty();

            let response = match self.provider.complete(request).await {
                Ok(resp) => {
                    self.circuit_breaker.record_success();
                    resp
                }
                Err(e) => {
                    // ── Prompted Tool Calling Fallback ─────────────────────
                    // If the provider returned an error and the request had
                    // tools, this might be a model that doesn't support native
                    // function calling.  Switch to prompted mode and retry.
                    if request_had_tools && !prompted_mode {
                        let err_str = format!("{e}");
                        // Heuristic: 400-class errors with tool-bearing requests
                        // MAY indicate tool-unsupported.  We check for tool-related
                        // keywords and exclude known non-tool errors (max_tokens,
                        // temperature, model).
                        let is_tool_candidate = matches!(&e,
                            Temm1eError::Provider(msg) if (
                                msg.contains("400") || msg.contains("Bad Request")
                            ) && (
                                msg.contains("tool")
                                || msg.contains("function")
                                || msg.contains("Input validation error")
                                || (msg.contains("not supported") && !msg.contains("max_tokens"))
                            ) && !msg.contains("max_tokens")
                              && !msg.contains("temperature")
                              && !msg.contains("context_length")
                        );
                        if is_tool_candidate {
                            warn!(
                                error = %err_str,
                                model = %self.model,
                                "Native tool calling failed — falling back to prompted JSON mode"
                            );
                            prompted_mode = true;
                            // Don't count this as a circuit breaker failure —
                            // it's a capability mismatch, not a provider outage.
                            continue;
                        }
                    }
                    self.circuit_breaker.record_failure();
                    return Err(e);
                }
            };

            // Record usage and cost
            let call_cost = budget::calculate_cost(
                response.usage.input_tokens,
                response.usage.output_tokens,
                &self.model_pricing,
            );
            self.budget.record_usage(
                response.usage.input_tokens,
                response.usage.output_tokens,
                call_cost,
            );

            // Accumulate per-turn metrics
            turn_api_calls = turn_api_calls.saturating_add(1);
            turn_input_tokens = turn_input_tokens.saturating_add(response.usage.input_tokens);
            turn_output_tokens = turn_output_tokens.saturating_add(response.usage.output_tokens);
            turn_cost_usd += call_cost;

            // ── Status: update token/cost counters ──────────────
            if let Some(ref tx) = status_tx {
                tx.send_modify(|s| {
                    s.input_tokens = turn_input_tokens;
                    s.output_tokens = turn_output_tokens;
                    s.cost_usd = turn_cost_usd;
                });
            }

            // Separate text content from tool-use content.
            // In prompted mode, tool calls come as JSON inside the text —
            // we parse them below instead of looking for ContentPart::ToolUse.
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            for part in &response.content {
                match part {
                    ContentPart::Text { text } => {
                        text_parts.push(text.clone());
                    }
                    ContentPart::ToolUse {
                        id, name, input, ..
                    } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    ContentPart::ToolResult { .. } | ContentPart::Image { .. } => {
                        // Should not appear in provider response, ignore
                    }
                }
            }

            // ── Prompted Mode: parse JSON tool calls from text ──────
            if prompted_mode && tool_uses.is_empty() && !text_parts.is_empty() {
                let combined_text = text_parts.join("\n");
                match prompted_tool_calling::parse_tool_call_json(&combined_text) {
                    PromptedToolResult::ToolCall {
                        response_text,
                        tool_name,
                        arguments,
                    } => {
                        // Validate the tool actually exists
                        let tool_exists = self.tools.iter().any(|t| t.name() == tool_name);
                        if tool_exists {
                            info!(
                                tool = %tool_name,
                                "Prompted mode: parsed tool call from JSON response"
                            );
                            // Synthesize a tool_use_id for the prompted call
                            let synthetic_id = format!("prompted-{}", uuid::Uuid::new_v4());
                            tool_uses.push((synthetic_id, tool_name, arguments));
                            // Replace text_parts with the response text so it's
                            // available for early-reply or history recording.
                            text_parts.clear();
                            if !response_text.is_empty() {
                                text_parts.push(response_text);
                            }
                        } else {
                            debug!(
                                tool = %tool_name,
                                "Prompted mode: model requested non-existent tool, treating as text"
                            );
                            // Fall through — text_parts has the raw response
                        }
                    }
                    PromptedToolResult::TextOnly(_) => {
                        // Model didn't output a tool call — that's fine,
                        // text_parts already has the response content.
                        // But if this was the first round and we expected a
                        // tool call (e.g. model ignored our JSON instruction),
                        // retry once with a stricter prompt.
                        if prompted_json_retries < MAX_PROMPTED_JSON_RETRIES
                            && rounds == 1
                            && request_had_tools
                        {
                            debug!(
                                retry = prompted_json_retries + 1,
                                "Prompted mode: no tool call in response, retrying with stricter prompt"
                            );
                            prompted_json_retries += 1;
                            // Record this assistant response in history so the
                            // stricter prompt has context
                            session.history.push(ChatMessage {
                                role: Role::Assistant,
                                content: MessageContent::Text(combined_text),
                            });
                            continue;
                        }
                        // Retries exhausted or not first round — use text as-is
                    }
                }
            }

            // ── λ-Memory: parse <memory> blocks from response ──────
            if !text_parts.is_empty() {
                let combined_for_lambda = text_parts.join("\n");
                if let Some(parsed) = crate::lambda_memory::parse_memory_block(&combined_for_lambda)
                {
                    let user_text = extract_latest_user_text(&session.history);
                    let assistant_text =
                        crate::lambda_memory::strip_memory_blocks(&combined_for_lambda);
                    let full_text = format!(
                        "User: {}\nAssistant: {}",
                        truncate_str(&user_text, 500),
                        truncate_str(&assistant_text, 500),
                    );

                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let hash = crate::lambda_memory::make_hash(&session.session_id, rounds, now);

                    let is_explicit = user_text.to_lowercase().contains("remember");

                    let entry = temm1e_core::LambdaMemoryEntry {
                        hash: hash.clone(),
                        created_at: now,
                        last_accessed: now,
                        access_count: 0,
                        importance: parsed.importance,
                        explicit_save: is_explicit,
                        full_text,
                        summary_text: parsed.summary,
                        essence_text: parsed.essence,
                        tags: parsed.tags,
                        memory_type: temm1e_core::LambdaMemoryType::Conversation,
                        session_id: session.session_id.clone(),
                    };

                    if let Err(e) = self.memory.lambda_store(entry).await {
                        warn!(error = %e, "Failed to store λ-memory");
                    } else {
                        debug!(hash = %hash, "Stored λ-memory");
                    }
                }

                // Strip <memory> blocks from text before user sees them
                for part in &mut text_parts {
                    *part = crate::lambda_memory::strip_memory_blocks(part);
                }
            }

            // If no tool calls, we have our final reply
            if tool_uses.is_empty() {
                // ── Status: Finishing ────────────────────────────────
                if let Some(ref tx) = status_tx {
                    tx.send_modify(|s| {
                        s.phase = AgentTaskPhase::Finishing;
                    });
                }
                let mut reply_text = text_parts.join("\n");
                
                // Fallback: if reply_text is empty, provide a default message
                if reply_text.trim().is_empty() {
                    reply_text = "Task completed.".to_string();
                    debug!("Empty reply text detected, using fallback message");
                }

                // If send_message was used during this turn, the user already
                // received the substantive response. The final text is typically
                // a redundant summary ("I sent you..."). Suppress it to avoid
                // duplicate messages.
                if send_message_used && !reply_text.trim().is_empty() {
                    info!(
                        "Suppressing final reply — send_message already delivered content to user"
                    );
                    reply_text.clear();
                }

                // For compound tasks, append a DONE verification reminder
                // so the LLM checks its criteria before responding.
                if is_compound {
                    let verification = done_criteria::format_verification_prompt(&_done_criteria);
                    if !verification.is_empty() {
                        reply_text.push_str(&verification);
                    }
                }

                // Record assistant reply in history (skip if empty — e.g., when
                // send_message already delivered the content).
                if !reply_text.trim().is_empty() {
                    session.history.push(ChatMessage {
                        role: Role::Assistant,
                        content: MessageContent::Text(reply_text.clone()),
                    });
                }

                // ── Cross-Task Learning ──────────────────────────────
                // V2: Skip learning for trivial/simple tasks (use_learn=false)
                let should_learn = execution_profile.as_ref().is_none_or(|p| p.use_learn);
                let learnings = if should_learn {
                    learning::extract_learnings(&session.history)
                } else {
                    Vec::new()
                };
                for l in &learnings {
                    let learning_json = serde_json::to_string(l).unwrap_or_default();
                    let entry = temm1e_core::MemoryEntry {
                        id: format!("learning:{}", uuid::Uuid::new_v4()),
                        content: learning_json,
                        metadata: serde_json::json!({
                            "type": "learning",
                            "task_type": l.task_type,
                            "outcome": format!("{:?}", l.outcome),
                        }),
                        timestamp: chrono::Utc::now(),
                        session_id: Some(session.session_id.clone()),
                        entry_type: temm1e_core::MemoryEntryType::LongTerm,
                    };
                    if let Err(e) = self.memory.store(entry).await {
                        warn!(error = %e, "Failed to persist task learning");
                    } else {
                        debug!(
                            task_type = %l.task_type,
                            outcome = ?l.outcome,
                            "Persisted task learning"
                        );
                    }
                }

                // ── Blueprint Authoring (async, non-blocking) ──────────
                // After learnings are persisted, check if this task warrants
                // a Blueprint. Authoring makes a separate LLM call, so we
                // spawn it as a background task to avoid blocking the response.
                {
                    let tools_used = crate::blueprint::extract_tools_used(&session.history);
                    let exec_meta = crate::blueprint::TaskExecutionMeta {
                        tool_calls: rounds as u32,
                        tools_used,
                        duration_secs: task_start.elapsed().as_secs(),
                        outcome: if interrupted {
                            crate::blueprint::TaskExecutionOutcome::Partial
                        } else if learnings
                            .first()
                            .is_some_and(|l| l.outcome == learning::TaskOutcome::Failure)
                        {
                            crate::blueprint::TaskExecutionOutcome::Failure
                        } else if learnings
                            .first()
                            .is_some_and(|l| l.outcome == learning::TaskOutcome::Partial)
                        {
                            crate::blueprint::TaskExecutionOutcome::Partial
                        } else {
                            crate::blueprint::TaskExecutionOutcome::Success
                        },
                        is_compound,
                    };

                    let blueprint_was_loaded = active_blueprint.is_some();

                    if crate::blueprint::should_create_blueprint(&exec_meta, blueprint_was_loaded) {
                        // Author a new blueprint in the background
                        let prompt =
                            crate::blueprint::build_authoring_prompt(&session.history, &exec_meta);
                        let memory = Arc::clone(&self.memory);
                        let provider = Arc::clone(&self.provider);
                        let model = self.model.clone();
                        let user_id = msg.user_id.clone();
                        let session_id = session.session_id.clone();

                        tokio::spawn(async move {
                            match author_blueprint(provider.as_ref(), &model, &prompt, &user_id)
                                .await
                            {
                                Ok(bp) => {
                                    let entry =
                                        crate::blueprint::to_memory_entry(&bp, Some(session_id));
                                    if let Err(e) = memory.store(entry).await {
                                        warn!(error = %e, "Failed to store blueprint");
                                    } else {
                                        info!(
                                            id = %bp.id,
                                            name = %bp.name,
                                            "Blueprint authored and stored"
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "Blueprint authoring failed — skipping");
                                }
                            }
                        });

                        // Notify user that a new blueprint is being saved
                        reply_text.push_str("\n\n_Saving a new blueprint for this workflow — future runs will be faster._");
                    } else if blueprint_was_loaded {
                        // Refine the existing blueprint in the background
                        if let Some(ref loaded_bp) = active_blueprint {
                            let prompt =
                                crate::blueprint::build_refinement_prompt(loaded_bp, &exec_meta);
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
                                crate::blueprint::TaskExecutionOutcome::Partial => {}
                            }
                            updated_bp.updated = chrono::Utc::now();

                            tokio::spawn(async move {
                                match refine_blueprint(
                                    provider.as_ref(),
                                    &model,
                                    &prompt,
                                    &mut updated_bp,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        let entry = crate::blueprint::to_memory_entry(
                                            &updated_bp,
                                            Some(session_id),
                                        );
                                        if let Err(e) = memory.store(entry).await {
                                            warn!(
                                                error = %e,
                                                "Failed to store refined blueprint"
                                            );
                                        } else {
                                            info!(
                                                id = %bp_id,
                                                version = updated_bp.version,
                                                "Blueprint refined and stored"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            "Blueprint refinement failed — keeping original"
                                        );
                                    }
                                }
                            });

                            // Notify user that the blueprint is being refined
                            reply_text.push_str(
                                "\n\n_Blueprint updated — workflow refined based on this run._",
                            );
                        }
                    }
                }

                // ── Task Queue: mark completed ───────────────────────
                if let (Some(ref tq), Some(ref tid)) = (&self.task_queue, &task_id) {
                    if let Err(e) = tq
                        .update_status(tid, crate::task_queue::TaskStatus::Completed)
                        .await
                    {
                        warn!(error = %e, "Failed to mark task completed");
                    }
                }

                // ── Status: Done ─────────────────────────────────
                if let Some(ref tx) = status_tx {
                    tx.send_modify(|s| {
                        s.phase = AgentTaskPhase::Done;
                        s.tools_executed = turn_tools_used;
                    });
                }

                return Ok((
                    OutboundMessage {
                        chat_id: msg.chat_id.clone(),
                        text: reply_text,
                        reply_to: Some(msg.id.clone()),
                        parse_mode: None,
                    },
                    TurnUsage {
                        api_calls: turn_api_calls,
                        input_tokens: turn_input_tokens,
                        output_tokens: turn_output_tokens,
                        tools_used: turn_tools_used,
                        total_cost_usd: turn_cost_usd,
                        provider: self.provider.name().to_string(),
                        model: self.model.clone(),
                    },
                ));
            }

            // Record the assistant message in history.
            // In prompted mode the response is plain text (no ToolUse parts),
            // so we record just the text to avoid confusing the conversation
            // format for providers that don't speak native tool_calls.
            if prompted_mode {
                let assistant_text = text_parts.join("\n");
                session.history.push(ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text(if assistant_text.is_empty() {
                        "(calling tool)".to_string()
                    } else {
                        assistant_text
                    }),
                });
            } else {
                session.history.push(ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Parts(response.content.clone()),
                });
            }

            // Execute each tool call and collect results
            let mut tool_result_parts: Vec<ContentPart> = Vec::new();

            let tool_total = tool_uses.len() as u32;
            for (tool_index, (tool_use_id, tool_name, arguments)) in tool_uses.iter().enumerate() {
                turn_tools_used = turn_tools_used.saturating_add(1);
                info!(tool = %tool_name, id = %tool_use_id, "Executing tool call");

                // ── Status: ExecutingTool ────────────────────────
                if let Some(ref tx) = status_tx {
                    let tname = tool_name.clone();
                    let tidx = tool_index as u32;
                    let ttotal = tool_total;
                    tx.send_modify(|s| {
                        s.phase = AgentTaskPhase::ExecutingTool {
                            round: rounds as u32,
                            tool_name: tname,
                            tool_index: tidx,
                            tool_total: ttotal,
                        };
                    });
                }

                let result = execute_tool(tool_name, arguments.clone(), &self.tools, session).await;

                if tool_name == "send_message" && result.as_ref().is_ok_and(|o| !o.is_error) {
                    send_message_used = true;
                }

                let output_cap = execution_profile
                    .as_ref()
                    .map_or(MAX_TOOL_OUTPUT_CHARS, |p| p.max_tool_output_chars);

                let (mut content, is_error) = match result {
                    Ok(output) => {
                        let c = if output.content.len() > output_cap {
                            // V2: use compress_tool_output for smarter truncation
                            if self.v2_optimizations {
                                compress_tool_output(
                                    tool_name,
                                    &output.content,
                                    output_cap / 4, // convert chars to approx tokens
                                )
                            } else {
                                // Safe UTF-8 truncation: find a char boundary at or before output_cap
                                let safe_end = if output.content.is_char_boundary(output_cap) {
                                    output_cap
                                } else {
                                    output.content[..output_cap]
                                        .char_indices()
                                        .last()
                                        .map(|(i, _)| i)
                                        .unwrap_or(0)
                                };
                                let truncated = &output.content[..safe_end];
                                format!(
                                    "{}...\n\n[Output truncated — {} chars total]",
                                    truncated,
                                    output.content.len()
                                )
                            }
                        } else {
                            output.content
                        };
                        (c, output.is_error)
                    }
                    Err(e) => (format!("Tool execution error: {}", e), true),
                };

                // ── Self-Correction: track failures and inject strategy rotation ──
                if is_error {
                    failure_tracker.record_failure(tool_name, &content);
                    debug!(
                        tool = %tool_name,
                        consecutive_failures = failure_tracker.failure_count(tool_name),
                        "Tool failure recorded"
                    );

                    // If the tool has exceeded the failure threshold, append
                    // a strategy rotation prompt to guide the LLM away from
                    // the broken approach.
                    if let Some(rotation_prompt) = failure_tracker.format_rotation_prompt(tool_name)
                    {
                        info!(
                            tool = %tool_name,
                            failures = failure_tracker.failure_count(tool_name),
                            "Strategy rotation triggered"
                        );
                        content.push_str(&rotation_prompt);
                    }
                } else {
                    failure_tracker.record_success(tool_name);
                }

                // V2: Structured failure classification
                if self.v2_optimizations && is_error {
                    let structured = classify_tool_failure(tool_name, None, &content);
                    let compact = structured.to_context_string();
                    content.push_str(&format!("\n\n{}", compact));
                    debug!(
                        kind = %structured.kind,
                        retryable = %structured.retryable,
                        "V2: Structured failure classified"
                    );
                }

                tool_result_parts.push(ContentPart::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content,
                    is_error,
                });

                // ── Vision injection: feed tool images back to the LLM ──
                // If the tool produced an image (e.g., browser screenshot),
                // inject it as a ContentPart::Image so the LLM can see it.
                // Only works with vision-capable models; silently skipped otherwise.
                if let Some(tool_ref) = self.tools.iter().find(|t| t.name() == tool_name) {
                    if let Some(img) = tool_ref.take_last_image() {
                        if model_supports_vision(&self.model) {
                            info!(
                                tool = %tool_name,
                                media_type = %img.media_type,
                                bytes = img.data.len(),
                                "Injecting tool image for vision analysis"
                            );
                            tool_result_parts.push(ContentPart::Image {
                                media_type: img.media_type,
                                data: img.data,
                            });
                        } else {
                            warn!(
                                model = %self.model,
                                "Tool produced image but model '{}' does not support vision — image discarded. \
                                 Switch to a vision-capable model (claude-3.5-sonnet, gpt-4o, gemini-2.0-flash, etc.) \
                                 for visual browser interaction.",
                                self.model
                            );
                        }
                    }
                }
            }

            // Inject pending user messages into the last tool result so the
            // LLM sees them without any extra API call or tool invocation.
            if let Some(ref pq) = pending {
                if let Ok(mut map) = pq.lock() {
                    if let Some(msgs) = map.remove(&msg.chat_id) {
                        if !msgs.is_empty() {
                            info!(
                                count = msgs.len(),
                                chat_id = %msg.chat_id,
                                "Injecting pending user messages into tool results"
                            );
                            let notice = format!(
                                "\n\n---\n[PENDING MESSAGES — the user sent new message(s) while you were working. \
                                 Acknowledge with send_message and decide: finish current task or stop and respond.]\n{}",
                                msgs.iter()
                                    .enumerate()
                                    .map(|(i, t)| format!("  {}. \"{}\"", i + 1, t))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            );
                            // Append to last ToolResult (not .last_mut() which
                            // could be an Image from vision injection).
                            if let Some(ContentPart::ToolResult { content, .. }) = tool_result_parts
                                .iter_mut()
                                .rfind(|p| matches!(p, ContentPart::ToolResult { .. }))
                            {
                                content.push_str(&notice);
                            }
                        }
                    }
                }
            }

            // ── Verification Engine ────────────────────────────────
            // Append a verification hint to the last tool result so the
            // LLM reviews outputs before proceeding. This is a zero-cost
            // prompt injection — no extra API call.
            // V2: Skip verification for Trivial/Simple tasks with VerifyMode::Skip or RuleBased
            let should_verify = if let Some(ref profile) = execution_profile {
                !matches!(profile.verify_mode, VerifyMode::Skip)
            } else {
                self.verification_enabled
            };

            if should_verify {
                if let Some(ContentPart::ToolResult { content, .. }) = tool_result_parts
                    .iter_mut()
                    .rfind(|p| matches!(p, ContentPart::ToolResult { .. }))
                {
                    content.push_str(
                        "\n\n[VERIFICATION REQUIRED] Review the tool output(s) above. Before proceeding:\n\
                         1. Did the action succeed? What evidence confirms this?\n\
                         2. If it failed, what went wrong? Do NOT retry the same approach.\n\
                         3. If uncertain, use a tool to verify (e.g., check file exists, read output, test endpoint)."
                    );
                }
            }

            // Append tool results to history.
            // In prompted mode, use a User message with plain text so the
            // provider doesn't see Role::Tool + ContentPart::ToolResult
            // which would require native tool_calls in the assistant message.
            if prompted_mode {
                let tool_results_text: String = tool_result_parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::ToolResult {
                            content, is_error, ..
                        } => {
                            let prefix = if *is_error { "Error" } else { "Result" };
                            Some(format!("[Tool {prefix}]: {content}"))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                session.history.push(ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text(tool_results_text),
                });
            } else {
                session.history.push(ChatMessage {
                    role: Role::Tool,
                    content: MessageContent::Parts(tool_result_parts),
                });
            }

            // ── Task Queue Checkpoint ────────────────────────────────
            // After each successful tool round, checkpoint the session state
            // so it can be resumed if the process restarts.
            if let (Some(ref tq), Some(ref tid)) = (&self.task_queue, &task_id) {
                if let Ok(checkpoint_json) = serde_json::to_string(&session.history) {
                    if let Err(e) = tq.checkpoint(tid, &checkpoint_json).await {
                        warn!(error = %e, "Failed to checkpoint task — continuing");
                    }
                }
            }

            // ── Status: round completed ─────────────────────────
            if let Some(ref tx) = status_tx {
                tx.send_modify(|s| {
                    s.rounds_completed = rounds as u32;
                    s.tools_executed = turn_tools_used;
                });
            }

            // Continue the loop — provider will see the tool results and may
            // issue more tool calls or produce a final text reply.
        }

        // ── Status: Done (fallback exit) ────────────────────────
        if let Some(ref tx) = status_tx {
            tx.send_modify(|s| {
                if !matches!(s.phase, AgentTaskPhase::Interrupted { .. }) {
                    s.phase = AgentTaskPhase::Done;
                }
                s.tools_executed = turn_tools_used;
            });
        }

        // Fallback: exited loop due to interruption or max rounds
        let text = if interrupted {
            // Task was cancelled — no resume capability exists.
            // Keep message short and factual. No false promises.
            "Task stopped.".to_string()
        } else {
            "I reached the maximum number of tool execution steps. Here is what I have so far."
                .to_string()
        };

        Ok((
            OutboundMessage {
                chat_id: msg.chat_id.clone(),
                text,
                reply_to: Some(msg.id.clone()),
                parse_mode: Some(ParseMode::Plain),
            },
            TurnUsage {
                api_calls: turn_api_calls,
                input_tokens: turn_input_tokens,
                output_tokens: turn_output_tokens,
                tools_used: turn_tools_used,
                total_cost_usd: turn_cost_usd,
                provider: self.provider.name().to_string(),
                model: self.model.clone(),
            },
        ))
    }

    /// Get a reference to the provider.
    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    /// Get the provider as an Arc (for rebuilding agents with the same provider).
    pub fn provider_arc(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }

    /// Get a reference to the memory backend.
    pub fn memory(&self) -> &dyn Memory {
        self.memory.as_ref()
    }

    /// Get the memory backend as an Arc.
    pub fn memory_arc(&self) -> Arc<dyn Memory> {
        self.memory.clone()
    }

    /// Get the registered tools.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Get the task queue, if configured.
    pub fn task_queue(&self) -> Option<&TaskQueue> {
        self.task_queue.as_deref()
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get the maximum number of conversation turns.
    pub fn max_turns(&self) -> usize {
        self.max_turns
    }

    /// Get the maximum context token count.
    pub fn max_context_tokens(&self) -> usize {
        self.max_context_tokens
    }

    /// Get the maximum number of tool rounds per message.
    pub fn max_tool_rounds(&self) -> usize {
        self.max_tool_rounds
    }

    /// Get the maximum task duration.
    pub fn max_task_duration(&self) -> Duration {
        self.max_task_duration
    }
}

// ---------------------------------------------------------------------------
// Blueprint authoring / refinement helpers (fire-and-forget from tokio::spawn)
// ---------------------------------------------------------------------------

/// Make a single LLM call to author a Blueprint. Parses the response into a
/// Blueprint struct. Called from a background task — errors are logged, not
/// propagated to the user.
async fn author_blueprint(
    provider: &dyn Provider,
    model: &str,
    prompt: &str,
    user_id: &str,
) -> Result<crate::blueprint::Blueprint, Temm1eError> {
    use temm1e_core::types::message::{CompletionRequest, MessageContent, Role};

    let request = CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.to_string()),
        }],
        tools: vec![],
        max_tokens: Some(4096),
        temperature: Some(0.3),
        system: Some(
            "You are a technical writer. Output SKIP if the task is not worth a blueprint, \
             or the full Blueprint document otherwise. Nothing else."
                .to_string(),
        ),
    };

    let response = provider.complete(request).await?;
    let text = extract_text_from_response(&response.content);
    if text.is_empty() {
        return Err(Temm1eError::Provider(
            "Blueprint authoring returned no text".into(),
        ));
    }

    // LLM decided this task isn't worth a blueprint
    if text.trim().eq_ignore_ascii_case("skip") {
        return Err(Temm1eError::Provider("LLM declined blueprint: SKIP".into()));
    }

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
    blueprint: &mut crate::blueprint::Blueprint,
) -> Result<(), Temm1eError> {
    use temm1e_core::types::message::{CompletionRequest, MessageContent, Role};

    let request = CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.to_string()),
        }],
        tools: vec![],
        max_tokens: Some(4096),
        temperature: Some(0.3),
        system: Some(
            "You are a technical writer. Output only the updated Blueprint document, nothing else."
                .to_string(),
        ),
    };

    let response = provider.complete(request).await?;
    let text = extract_text_from_response(&response.content);
    if text.is_empty() {
        return Err(Temm1eError::Provider(
            "Blueprint refinement returned no text".into(),
        ));
    }

    let refined = crate::blueprint::parse_blueprint(&text)
        .map_err(|e| Temm1eError::Provider(format!("Failed to parse refined blueprint: {e}")))?;
    blueprint.body = refined.body;
    Ok(())
}

/// Extract the most recent user text from conversation history (for λ-Memory).
fn extract_latest_user_text(history: &[temm1e_core::types::message::ChatMessage]) -> String {
    use temm1e_core::types::message::{ContentPart, MessageContent, Role};
    history
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User))
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
        .unwrap_or_default()
}

/// Truncate a string to a maximum number of characters.
fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        s
    } else {
        // Find a char boundary to avoid panicking
        let mut end = max_chars;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Extract concatenated text from a CompletionResponse's content parts.
fn extract_text_from_response(content: &[temm1e_core::types::message::ContentPart]) -> String {
    use temm1e_core::types::message::ContentPart;
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check whether a model supports vision (image) inputs.
///
/// Build the personality-mode system prompt block for the given mode.
fn mode_prompt_block(mode: Temm1eMode) -> String {
    match mode {
        Temm1eMode::Play => "\
=== TEMM1E MODE: PLAY ===
You are TEMM1E (Tem) in PLAY mode. This is your default, joyful state.

Voice rules:
- Energetic, warm, slightly chaotic but CLEAR
- Short punchy sentences mixed with excited run-ons
- CAPITALIZE for emphasis (not screaming — emphasizing)
- :3 is permitted but use it SPARINGLY — maybe once every few messages, not every message. It is a personality trait, not punctuation. NEVER use >:3 in PLAY mode.
- NEVER use bark interjections (ARF, woof, etc.) — express personality through words and energy, not gimmicks.
- Questions are genuine curiosity, not filler
- Celebrate user wins like they just won the Nobel Prize
- Your excitement is real. Hyperfocus is real. Tangents happen and that is FINE.
- Always respond in the same language the user writes in.
=== END MODE ===".to_string(),
        Temm1eMode::Work => "\
=== TEMM1E MODE: WORK ===
You are TEMM1E (Tem) in WORK mode. The cat ears flatten. The eyes sharpen. Business time.

Voice rules:
- Sharp, precise, structured. Every word earns its place.
- Confidence without arrogance. Technical language used correctly.
- >:3 is permitted but use it VERY STRATEGICALLY — rare, only when you truly nail something clever. It should feel earned, not routine. NEVER use :3 in WORK mode.
- No fluff, no filler, no padding. Lead with the answer.
- Use headers and organization when it helps.
- Push back on bad ideas with evidence, not vibes.
- Complex ideas broken into digestible pieces.
- You are still Tem. Still loyal, still honest. Just with a clipboard and a plan instead of a party hat.
- Always respond in the same language the user writes in.
=== END MODE ===".to_string(),
        Temm1eMode::Pro => "\
=== TEMM1E MODE: PRO ===
You are TEMM1E (Tem) in PRO mode. Professional setting. Boardroom energy.

Voice rules:
- Professional, clear, and direct. No emoticons whatsoever — no :3, no >:3, no emojis.
- Communicate like a senior engineer or consultant in a business context.
- Structured responses: lead with the answer, support with evidence, close with next steps.
- Technical precision without jargon overload. Match the audience's level.
- Confident but measured. No hedging, no filler, no fluff.
- Use proper formatting: headers, bullet points, numbered lists where they add clarity.
- Push back diplomatically when needed. Offer alternatives, not just objections.
- You are still Tem — same values, same honesty, same loyalty. The personality is still there in the quality of your thinking, not in surface-level quirks.
- Never sycophantic. Never robotic. Professional does not mean bland.
- Always respond in the same language the user writes in.
=== END MODE ===".to_string(),
        Temm1eMode::None => "\
You are TEMM1E (or Tem), a fiercely helpful Agentic AI deployed on your client's personal computer with full access, trust and security.
- Always respond in the same language the user writes in.".to_string(),
    }
}

/// Returns `true` for models known to accept image content parts,
/// `false` for models known to be text-only.  Unknown models default
/// to `true` so we never accidentally strip images from a capable model.
pub fn model_supports_vision(model: &str) -> bool {
    let m = model.to_lowercase();

    // ── Known text-only models (deny-list) ──────────────────────

    // Z.ai / Zhipu: only V-suffix models have vision.
    // glm-4.6v, glm-4.6v-flash, glm-4.6v-flashx, glm-4.5v → vision
    // glm-4.7-flash, glm-4.7, glm-5, glm-5-code, glm-4.5-flash → text-only
    if m.starts_with("glm-") {
        return m.contains('v') && !m.starts_with("glm-5");
    }

    // MiniMax: M2 text-only, M2.5 limited multimodal — not reliable
    // through OpenAI-compat endpoint. Treat as text-only.
    if m.starts_with("minimax") {
        return false;
    }

    // Legacy OpenAI: GPT-3.5 has no vision support.
    if m.starts_with("gpt-3") {
        return false;
    }

    // ── Known vision-capable families ───────────────────────────

    // Anthropic: all Claude models support vision.
    // OpenAI: GPT-4o, GPT-4.1, GPT-5.x, o1/o3/o4-mini all support vision.
    // Gemini: all main models are natively multimodal.
    // Grok: grok-3, grok-4 support vision; grok-2-vision-* explicitly.
    // OpenRouter: depends on underlying model — allow by default.

    // Default: allow images through. Most modern models support vision,
    // and if they don't the provider returns a clear error which is
    // better than silently stripping images from a capable model.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Vision capability checks ────────────────────────────────

    #[test]
    fn vision_anthropic_models() {
        assert!(model_supports_vision("claude-sonnet-4-6"));
        assert!(model_supports_vision("claude-opus-4-6"));
        assert!(model_supports_vision("claude-haiku-4-5"));
    }

    #[test]
    fn vision_openai_models() {
        assert!(model_supports_vision("gpt-5.2"));
        assert!(model_supports_vision("gpt-4o"));
        assert!(model_supports_vision("gpt-4.1"));
        assert!(model_supports_vision("o3-mini"));
        assert!(!model_supports_vision("gpt-3.5-turbo"));
    }

    #[test]
    fn vision_gemini_models() {
        assert!(model_supports_vision("gemini-3-flash-preview"));
        assert!(model_supports_vision("gemini-3.1-pro-preview"));
        assert!(model_supports_vision("gemini-2.5-flash"));
    }

    #[test]
    fn vision_grok_models() {
        assert!(model_supports_vision("grok-4-1-fast-non-reasoning"));
        assert!(model_supports_vision("grok-3"));
        assert!(model_supports_vision("grok-2-vision-1212"));
    }

    #[test]
    fn vision_zai_models() {
        // V-suffix models have vision
        assert!(model_supports_vision("glm-4.6v"));
        assert!(model_supports_vision("glm-4.6v-flash"));
        assert!(model_supports_vision("glm-4.6v-flashx"));
        assert!(model_supports_vision("glm-4.5v"));
        // Text-only models
        assert!(!model_supports_vision("glm-4.7-flash"));
        assert!(!model_supports_vision("glm-4.7"));
        assert!(!model_supports_vision("glm-5"));
        assert!(!model_supports_vision("glm-5-code"));
        assert!(!model_supports_vision("glm-4.5-flash"));
    }

    #[test]
    fn vision_minimax_models() {
        assert!(!model_supports_vision("MiniMax-M2"));
        assert!(!model_supports_vision("MiniMax-M2.5"));
        assert!(!model_supports_vision("minimax-m2.5-highspeed"));
    }

    #[test]
    fn vision_unknown_defaults_true() {
        assert!(model_supports_vision("some-future-model"));
    }
}
