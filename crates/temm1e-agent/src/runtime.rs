//! AgentRuntime — main agent loop that processes messages through the
//! provider, executing tool calls in a loop until a final text reply.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
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

/// Serialize a JSON value to a short preview string (max `max_chars`
/// characters, UTF-8 safe). Used by v4.8.0 observability enrichment.
fn truncate_json_preview(value: &serde_json::Value, max_chars: usize) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    if s.chars().count() <= max_chars {
        return s;
    }
    let cap = max_chars.saturating_sub(1).max(1);
    let mut out: String = s.chars().take(cap).collect();
    out.push('…');
    out
}

/// Concatenate all `Text` parts of a `CompletionResponse`'s content into
/// a single string. Used by the Eigen-Tune routing wrapper to compare
/// local and cloud responses for shadow/monitor mode.
fn response_to_text(resp: &temm1e_core::types::message::CompletionResponse) -> String {
    let mut out = String::new();
    for part in &resp.content {
        if let temm1e_core::types::message::ContentPart::Text { text } = part {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// Extract the first non-empty line of `text`, trimmed and truncated
/// to `max_chars` (UTF-8 safe). Used by v4.8.0 observability enrichment.
fn first_nonempty_line_preview(text: &str, max_chars: usize) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first.chars().count() <= max_chars {
        return first.to_string();
    }
    let cap = max_chars.saturating_sub(1).max(1);
    let mut out: String = first.chars().take(cap).collect();
    out.push('…');
    out
}

use crate::model_router::{ModelRouter, ModelRouterConfig};
use crate::output_compression::compress_tool_output;
use temm1e_core::types::error::classify_tool_failure;
use temm1e_core::types::optimization::VerifyMode;

use temm1e_core::types::config::Temm1eMode;

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

// Social intelligence
use temm1e_anima::personality::PersonalityConfig;
use temm1e_anima::SocialStorage;

// Eigen-Tune self-tuning distillation engine
use temm1e_distill::EigenTuneEngine;

/// Returns true when the user's turn looks like code / file / tool work
/// that benefits from layer-2 observers (Witness Planner Oath, Tem
/// Conscious pre/post observation). Chat, Q&A, and channel-style turns
/// return false — those observers have nothing postcondition-shaped to
/// ground against on conversational prompts, and their LLM round-trips
/// would just add latency (5-10s per observer) for zero user-visible
/// benefit.
///
/// The rule is intentionally simple and fully local (no LLM call):
///   1) complexity bucket must be above Trivial / Simple
///   2) the prompt must carry at least one code-signal substring
///      (file ext, `fn `, `pub fn`, `workspace`, code fence, etc.)
///
/// Both conditions together: catches "write a Rust file ..." and
/// "use file_write to create ..." while correctly skipping "write a
/// haiku" and "hey".
///
/// When this returns false, observers still stay attached to the
/// runtime — only the *proactive* LLM call is suppressed. Gate hooks
/// (e.g., Witness verifier) run as no-ops when no observation has
/// been seeded for the session.
pub(crate) fn turn_is_code_shaped(
    history_len: usize,
    user_text: &str,
) -> (bool, crate::model_router::TaskComplexity) {
    // We don't need full history for this classifier — it's a per-turn
    // rule. Empty tool slice is fine; the classifier only uses tools for
    // "multi_tool_types > 2" which isn't load-bearing here.
    let _ = history_len; // kept as parameter for future tuning
    let router = ModelRouter::new(ModelRouterConfig::default());
    let complexity = router.classify_complexity(&[], &[], user_text);
    let trivial_or_simple = matches!(
        complexity,
        crate::model_router::TaskComplexity::Trivial | crate::model_router::TaskComplexity::Simple
    );
    let t = user_text.to_ascii_lowercase();
    let has_code_signal = t.contains("file_")
        || t.contains("workspace")
        || t.contains(".rs")
        || t.contains(".py")
        || t.contains(".ts")
        || t.contains(".js")
        || t.contains(".json")
        || t.contains(".toml")
        || t.contains(".md")
        || t.contains("pub fn")
        || t.contains("fn ")
        || t.contains("class ")
        || t.contains("struct ")
        || t.contains("```");
    let is_code_shaped = !trivial_or_simple && has_code_signal;
    (is_code_shaped, complexity)
}

/// Shared runtime mode handle (same type used by mode_switch tool).
pub type SharedMode = Arc<RwLock<Temm1eMode>>;

/// P6: per-runtime tool-filter closure. Returns `true` for tools that should
/// remain visible to the provider, `false` to hide them.
pub type ToolFilter = Arc<dyn Fn(&dyn Tool) -> bool + Send + Sync>;

/// Maximum characters per tool output (roughly ~8K tokens).
const MAX_TOOL_OUTPUT_CHARS: usize = 30_000;

/// Soft per-turn cost advisory threshold (USD). When a single turn exceeds
/// this, we emit a `tracing::warn!` once so operators can notice runaway
/// tool loops on ambiguous prompts. Log-only — no behavior change; users
/// who need a hard ceiling set `[agent] max_spend_usd` in config.
const SOFT_TURN_COST_WARNING_USD: f64 = 0.10;

/// Record a provider failure into the circuit breaker UNLESS the error is a
/// rate-limit. A 429 is a throttle signal, not a provider-health signal, and
/// must not count toward the CB's failure threshold. Other errors (Auth,
/// Provider, network) still trip the breaker on repeated failures.
#[inline]
fn record_cb_failure_unless_rate_limit(cb: &CircuitBreaker, err: &Temm1eError) {
    if matches!(err, Temm1eError::RateLimited(_)) {
        tracing::debug!("circuit breaker: ignoring RateLimited (not a health signal)");
    } else {
        cb.record_failure();
    }
}

/// P5: derive a difficulty label from *actual* turn behaviour (tool-round
/// count), not from the classifier's intent. Strictly more informative for
/// downstream consumers (memory priors, eigen-tune routing) since it
/// reflects what happened rather than what was predicted.
///
/// Thresholds chosen to match the existing TaskDifficulty::{Simple, Standard,
/// Complex} tiers so the legacy string values remain usable for persistence.
#[inline]
pub fn derive_outcome_difficulty(tool_rounds: usize) -> &'static str {
    match tool_rounds {
        0..=2 => "simple",
        3..=10 => "standard",
        _ => "complex",
    }
}

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
    /// Per-session budget tracker (Arc-wrapped for sharing with TemDOS cores).
    budget: Arc<BudgetTracker>,
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
    /// Tem Conscious — consciousness observer that watches internal state and
    /// selectively injects context to improve outcomes. None = disabled.
    consciousness: Option<crate::consciousness_engine::ConsciousnessEngine>,
    /// Perpetuum temporal context injection string. Updated externally before each call.
    /// When set, prepended to the system prompt for time awareness.
    perpetuum_temporal: Option<Arc<RwLock<String>>>,
    /// Social intelligence: personality configuration loaded from personality.toml.
    /// When Some, replaces hardcoded personality text in system prompts and classifier.
    personality: Option<Arc<PersonalityConfig>>,
    /// Social intelligence: persistent user profile storage.
    /// When Some, enables user profiling, fact collection, and background evaluation.
    social_storage: Option<Arc<SocialStorage>>,
    /// Social intelligence: configuration from [social] TOML section.
    social_config: Option<temm1e_core::types::config::SocialConfig>,
    /// Social intelligence: per-session turn counter for evaluation scheduling.
    social_turn_count: Arc<AtomicU32>,
    /// Social intelligence: concurrent evaluation guard to prevent overlapping evals.
    social_evaluating: Arc<AtomicBool>,
    /// Eigen-Tune self-tuning distillation engine. None = disabled (default).
    /// All hooks are fire-and-forget — never blocks the user reply path.
    /// Default-config users (engine=None) see zero new code paths exercised.
    eigen_tune: Option<Arc<EigenTuneEngine>>,
    /// Whether local routing of distilled models is enabled. The double opt-in
    /// gate: even if `eigen_tune.is_some()`, local routing only fires when this
    /// is also true. Mirrors `EigenTuneConfig::enable_local_routing` so the
    /// runtime doesn't need to read config on every request.
    eigen_tune_local_routing: bool,
    /// Witness verification layer. When set, the runtime looks up an active
    /// `Oath` for the current session after the agent decides it is done, runs
    /// `verify_oath`, and rewrites the final reply per the configured
    /// strictness. `None` = Phase 1 default (no gating).
    witness: Option<Arc<temm1e_witness::Witness>>,
    /// Strictness for the Witness gate. Only consulted when `witness` is Some.
    witness_strictness: temm1e_witness::config::WitnessStrictness,
    /// Whether to append the per-task Witness readout to the final reply.
    witness_show_readout: bool,
    /// Cambium TrustEngine — when set, the gate hook calls
    /// `record_verdict(passed, AutonomousBasic)` after every Witness verdict,
    /// turning autonomy levels into evidence-bound state. Wraps in
    /// tokio::sync::Mutex because TrustEngine is &mut self for write paths.
    cambium_trust: Option<Arc<tokio::sync::Mutex<temm1e_cambium::trust::TrustEngine>>>,
    /// When true AND `witness` is set, the runtime auto-seals a Planner-
    /// generated Root Oath at the start of every non-trivial process_message
    /// by calling `temm1e_witness::planner::seal_oath_via_planner`. The
    /// generated Oath is sealed in the witness Ledger before the main agent
    /// loop starts, so the gate hook at the end of the loop can verify it.
    /// Adds one extra LLM call per Complex task. Default: false.
    auto_seal_planner_oath: bool,
    /// P6: optional per-runtime tool filter. When set, tools for which the
    /// predicate returns `false` are hidden from the provider's tool list
    /// (the model physically cannot call them). Composes AND with the
    /// existing role-based filter. Default: None = all role-permitted tools.
    ///
    /// Used by JIT swarm workers to exclude `spawn_swarm` (recursion block).
    tool_filter: Option<ToolFilter>,
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
            // v5.3.6: max_tool_rounds = 0 means unlimited (matches
            // max_task_duration_secs convention). Stagnation detection +
            // budget + duration are the real safety nets; iteration count
            // alone is a proxy, not a meaningful limit. Users who want a
            // hard ceiling can set a positive value in their TOML config.
            max_tool_rounds: 0,
            max_task_duration: Duration::from_secs(0),
            circuit_breaker: CircuitBreaker::default(),
            verification_enabled: true,
            max_consecutive_failures: 2,
            task_queue: None,
            budget: Arc::new(BudgetTracker::new(0.0)),
            hive_enabled: false,
            model_pricing,
            v2_optimizations: true,
            parallel_phases: false,
            shared_mode: None,
            shared_memory_strategy: None,
            consciousness: None,
            perpetuum_temporal: None,
            personality: None,
            social_storage: None,
            social_config: None,
            social_turn_count: Arc::new(AtomicU32::new(0)),
            social_evaluating: Arc::new(AtomicBool::new(false)),
            eigen_tune: None,
            eigen_tune_local_routing: false,
            witness: None,
            witness_strictness: temm1e_witness::config::WitnessStrictness::Observe,
            witness_show_readout: false,
            cambium_trust: None,
            auto_seal_planner_oath: false,
            tool_filter: None,
        }
    }

    /// Attach a Witness verification layer to this runtime.
    ///
    /// The Witness will be consulted after the agent decides it is done,
    /// before the final reply is returned. If the Witness has a sealed
    /// Oath for the current session, it runs `verify_oath()` and rewrites
    /// `reply_text` per `strictness`. If no Oath is sealed for the session
    /// (the common case in Phase 2 while the Planner Oath generation is
    /// being built out), the gate is a no-op.
    ///
    /// Cambium trust wiring is now Phase 4 — see `with_cambium_trust()`.
    pub fn with_witness(
        mut self,
        witness: Arc<temm1e_witness::Witness>,
        strictness: temm1e_witness::config::WitnessStrictness,
        show_readout: bool,
    ) -> Self {
        self.witness = Some(witness);
        self.witness_strictness = strictness;
        self.witness_show_readout = show_readout;
        self
    }

    /// Attach a Cambium `TrustEngine` to this runtime.
    ///
    /// When BOTH `with_witness(...)` and `with_cambium_trust(...)` are set,
    /// the runtime gate calls `trust.record_verdict(passed, level)` after
    /// every Witness verdict. PASS verdicts feed `record_success`, FAIL
    /// verdicts feed `record_failure`, Inconclusive verdicts are deliberately
    /// skipped (Witness couldn't decide, so trust shouldn't move either way).
    ///
    /// Wraps the engine in `tokio::sync::Mutex` because `TrustEngine`'s
    /// write paths take `&mut self`. The lock is held only for the duration
    /// of `record_verdict` — it is not held across any await points outside
    /// the call itself.
    pub fn with_cambium_trust(
        mut self,
        trust: Arc<tokio::sync::Mutex<temm1e_cambium::trust::TrustEngine>>,
    ) -> Self {
        self.cambium_trust = Some(trust);
        self
    }

    /// Enable Phase 4 auto-Oath generation by the Planner.
    ///
    /// When this is set AND `with_witness(...)` is also set, the runtime
    /// will call `temm1e_witness::planner::seal_oath_via_planner` at the
    /// start of every `process_message` call. The Planner LLM is invoked
    /// with the static `OATH_GENERATION_PROMPT` plus the user's request,
    /// the response is parsed into a Root Oath, and the Oath is sealed in
    /// the Witness Ledger. The gate hook at the end of the agent loop will
    /// then verify it.
    ///
    /// Adds **one extra LLM call per process_message** (clean-slate context,
    /// max_tokens=1024). Cost on a typical model: ~$0.001 per call. Failures
    /// (LLM error, parse error, Spec Reviewer rejection) are non-fatal —
    /// they're logged and the runtime proceeds with no sealed Oath, so the
    /// gate hook becomes a no-op for that session (Law 5: zero downside).
    pub fn with_auto_planner_oath(mut self, enabled: bool) -> Self {
        self.auto_seal_planner_oath = enabled;
        self
    }

    /// P6: install a per-runtime tool filter. The closure is called for each
    /// registered tool and must return `true` for tools that should be visible
    /// to the provider, `false` to hide them. Composes AND with the session
    /// role filter.
    ///
    /// Primary use: JIT swarm workers set a filter that excludes `spawn_swarm`
    /// from the worker's toolset, making nested swarm recursion physically
    /// impossible.
    pub fn with_tool_filter(mut self, filter: ToolFilter) -> Self {
        self.tool_filter = Some(filter);
        self
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
            budget: Arc::new(BudgetTracker::new(max_spend_usd)),
            hive_enabled: false,
            model_pricing,
            v2_optimizations: true,
            parallel_phases: false,
            shared_mode: None,
            shared_memory_strategy: None,
            consciousness: None,
            perpetuum_temporal: None,
            personality: None,
            social_storage: None,
            social_config: None,
            social_turn_count: Arc::new(AtomicU32::new(0)),
            social_evaluating: Arc::new(AtomicBool::new(false)),
            eigen_tune: None,
            eigen_tune_local_routing: false,
            witness: None,
            witness_strictness: temm1e_witness::config::WitnessStrictness::Observe,
            witness_show_readout: false,
            cambium_trust: None,
            auto_seal_planner_oath: false,
            tool_filter: None,
        }
    }

    /// Get a shared reference to the budget tracker (for TemDOS core sharing).
    pub fn budget(&self) -> Arc<BudgetTracker> {
        self.budget.clone()
    }

    /// Get the model pricing (for TemDOS core sharing).
    pub fn model_pricing(&self) -> &ModelPricing {
        &self.model_pricing
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

    /// Enable Tem Conscious consciousness observer.
    pub fn with_consciousness(
        mut self,
        engine: crate::consciousness_engine::ConsciousnessEngine,
    ) -> Self {
        self.consciousness = Some(engine);
        self
    }

    /// Inject the Eigen-Tune self-tuning distillation engine.
    ///
    /// When set, all five hooks fire after each provider call and tool
    /// execution. Fire-and-forget — errors are logged but never propagated
    /// to the user. Default-config users (engine=None) see zero new code
    /// paths exercised.
    ///
    /// `enable_local_routing` controls the second of the double opt-in
    /// switches: even if the engine is set, local routing only fires when
    /// this is also `true`. See `LOCAL_ROUTING_SAFETY.md` §2.
    pub fn with_eigen_tune(
        mut self,
        engine: Arc<EigenTuneEngine>,
        enable_local_routing: bool,
    ) -> Self {
        self.eigen_tune = Some(engine);
        self.eigen_tune_local_routing = enable_local_routing;
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

    /// Set the Perpetuum temporal context injection handle.
    /// The Arc<RwLock<String>> is updated externally by Perpetuum before each message.
    pub fn with_perpetuum_temporal(mut self, temporal: Arc<RwLock<String>>) -> Self {
        self.perpetuum_temporal = Some(temporal);
        self
    }

    /// Set the personality configuration for social intelligence.
    /// When set, replaces hardcoded personality text in system prompts and classifier.
    pub fn with_personality(mut self, p: Arc<PersonalityConfig>) -> Self {
        self.personality = Some(p);
        self
    }

    /// Set the social intelligence storage and configuration.
    /// When storage is Some, enables user profiling, fact collection, and background evaluation.
    pub fn with_social(
        mut self,
        storage: Option<Arc<SocialStorage>>,
        config: Option<temm1e_core::types::config::SocialConfig>,
    ) -> Self {
        self.social_storage = storage;
        self.social_config = config;
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

        // ── Phase 4: auto-seal a Planner-generated Root Oath ────────
        // When witness + auto_seal_planner_oath are both configured, ask the
        // Planner LLM to emit a Root Oath for this user message and seal it
        // in the Witness ledger BEFORE the main agent loop starts. The gate
        // hook at the end of the loop will then verify this Oath against
        // whatever the agent produces.
        //
        // Failures (LLM error, parse error, Spec Reviewer rejection) are
        // non-fatal — Law 5: zero downside. The hook simply skips on failure
        // and the runtime proceeds with no sealed Oath, making the gate hook
        // a no-op for this session.
        //
        // We track the sealed Oath in a turn-local Option and verify THAT
        // Oath directly at end of turn (see Witness gate around line 2050).
        // We deliberately do NOT fall back to `active_oath()` — a turn that
        // did not seal its own Oath must not verify someone else's. This
        // prevents orphan Oaths (from e.g. HiveRoute early-returns, or
        // Planner-sealed turns whose workspace state was mutated by a
        // sibling session) from being applied to unrelated replies.
        let mut oath_sealed_this_turn: Option<temm1e_witness::types::Oath> = None;
        if self.auto_seal_planner_oath {
            if let Some(ref witness) = self.witness {
                let user_text = msg.text.as_deref().unwrap_or("");
                if !user_text.trim().is_empty() {
                    // Complexity gate (Phase 4.5): fire the Planner LLM only
                    // when the turn is code-shaped. Law 5 still applies: even
                    // if the gate lets a non-code turn through, the Spec
                    // Reviewer will reject the Oath (no concrete
                    // postconditions) and the runtime proceeds with no
                    // sealed Oath — the verifier becomes a no-op. The gate
                    // just avoids paying the Planner's +5-10s on turns we
                    // know can't produce grounded verification.
                    let (is_code_shaped, complexity) =
                        turn_is_code_shaped(session.history.len(), user_text);
                    if !is_code_shaped {
                        tracing::debug!(
                            session_id = %session.session_id,
                            complexity = ?complexity,
                            "phase4.5: planner oath skipped (turn not code-shaped)"
                        );
                    } else {
                        let planner_req = temm1e_witness::planner::PlannerOathRequest {
                            witness,
                            provider: self.provider.clone(),
                            model: self.model.clone(),
                            user_request: user_text,
                            workspace_root: &session.workspace_path,
                            session_id: session.session_id.clone(),
                            root_goal_id: format!("root-{}", session.session_id),
                            subtask_id: format!("rootst-{}", session.session_id),
                        };
                        match temm1e_witness::planner::seal_oath_via_planner(planner_req).await {
                            Ok((sealed, entry_id)) => {
                                tracing::info!(
                                    session_id = %session.session_id,
                                    oath_hash = %sealed.sealed_hash[..16.min(sealed.sealed_hash.len())],
                                    ledger_entry_id = entry_id,
                                    postcondition_count = sealed.postconditions.len(),
                                    "phase4: planner oath sealed for session"
                                );
                                oath_sealed_this_turn = Some(sealed);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    session_id = %session.session_id,
                                    error = %e,
                                    "phase4: planner oath generation failed (Law 5: continuing without)"
                                );
                            }
                        }
                    }
                }
            }
        }

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

        // Tem Conscious: observation accumulators (collected during the turn)
        let mut classification_label = String::new();
        let mut difficulty_label = String::new();
        // Eigen-Tune complexity tier (set by classifier below).
        // String form of EigenTier: "simple"|"standard"|"complex". Defaults to
        // "standard" if neither classification path runs (e.g., when
        // v2_optimizations is disabled). Used by the routing wrapper at the
        // provider call site (Phase 13) and the collection hook (Phase 11).
        let mut eigentune_complexity: String = "standard".to_string();
        let mut tools_called_this_turn: Vec<String> = Vec::new();
        let mut tool_results_this_turn: Vec<String> = Vec::new();
        let mut max_consecutive_failures_seen: u32 = 0;
        let mut strategy_rotations_count: u32 = 0;
        let mut had_whisper = false;

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

        // ── Eigen-Tune: user-message signal (Phase 14, fire-and-forget) ──
        // Detect if the new message is a retry or rejection of the previous
        // assistant turn. Tier 1 heuristics only (no embedding) — Tier 2
        // would require an Ollama embedding call per message which is too
        // expensive for the hot path.
        if let Some(et) = &self.eigen_tune {
            let prev_user: Option<String> = session
                .history
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .and_then(|m| match &m.content {
                    MessageContent::Text(t) => Some(t.clone()),
                    MessageContent::Parts(parts) => parts.iter().find_map(|p| match p {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    }),
                });
            // Time window: hardcoded 0 since Session may not track per-message
            // timestamps. Passing 0 makes retry detection always-on for the
            // edit-distance check (the 60s window is the disqualifier).
            let elapsed_secs: u64 = 0;
            let (agree, signal_kind) = temm1e_distill::judge::behavior::behavior_observation(
                &user_text,
                prev_user.as_deref(),
                elapsed_secs,
                false, // tool_failed: not relevant for an incoming message
            );
            if !agree {
                let signal = match signal_kind {
                    "explicit_rejection" => temm1e_distill::types::QualitySignal::UserRejected,
                    "retry_rephrase" => temm1e_distill::types::QualitySignal::UserRetried,
                    _ => temm1e_distill::types::QualitySignal::UserRetried,
                };
                let engine = et.clone();
                let chat_id = msg.chat_id.clone();
                tokio::spawn(async move {
                    engine.on_signal(&chat_id, signal).await;
                });
            }
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

        // ── Social intelligence: collect raw facts ───────────────────
        if let (Some(storage), Some(config)) = (&self.social_storage, &self.social_config) {
            if config.enabled {
                let msg_facts = temm1e_anima::facts::collect_message_facts(&user_text);
                let interaction = temm1e_anima::facts::collect_interaction_facts(
                    0, // seconds_since_last — not yet tracked
                    session.history.len() as u32,
                    false,
                    false,
                    0,
                );
                let turn_facts = temm1e_anima::TurnFacts {
                    turn_number: session.history.len() as u32,
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    user_message: msg_facts,
                    tem_response: temm1e_anima::facts::collect_message_facts(""),
                    interaction,
                };
                let social_user_id = msg.user_id.clone();
                let social_storage = storage.clone();
                let turn = turn_facts.turn_number;
                let facts_clone = turn_facts.clone();
                let text_clone = user_text.clone();
                tokio::spawn(async move {
                    if let Err(e) = social_storage
                        .buffer_facts(&social_user_id, turn, &facts_clone, &text_clone)
                        .await
                    {
                        tracing::debug!(error = %e, "Failed to buffer social facts");
                    }
                });
                self.social_turn_count.fetch_add(1, Ordering::Relaxed);
            }
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

            // Social intelligence: load profile summary for classifier context
            let profile_summary = if let (Some(storage), Some(config)) =
                (&self.social_storage, &self.social_config)
            {
                if config.enabled {
                    match storage.get_profile(&msg.user_id).await {
                        Ok(Some(profile)) => Some(
                            temm1e_anima::communication::classifier_profile_summary(&profile),
                        ),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // Safety-net timeout on the classifier LLM call (30s). Under normal
            // operation the classifier returns in 1–6 s across all supported
            // providers. A longer stall indicates a provider-side incident;
            // falling through to the rule-based classifier keeps the session
            // alive rather than blocking the user. Root-cause notes:
            // docs/full-sweep-1/TUI-CLASSIFIER-HANG.md.
            let classify_fut = crate::llm_classifier::classify_message(
                self.provider.as_ref(),
                &self.model,
                &user_text,
                &session.history,
                &blueprint_categories,
                current_mode,
                self.personality.as_deref(),
                profile_summary.as_deref(),
            );
            let classify_result =
                tokio::time::timeout(std::time::Duration::from_secs(30), classify_fut).await;
            let unwrapped = classify_result.unwrap_or_else(|_| {
                tracing::warn!(
                    "classify_message hit 30s safety timeout — falling back to rule-based classifier"
                );
                Err(Temm1eError::Provider(
                    "classify_message 30s timeout".to_string(),
                ))
            });
            match unwrapped {
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

                    // Tem Conscious: capture classification for observation
                    classification_label = format!("{:?}", classification.category);
                    difficulty_label = format!("{:?}", classification.difficulty);

                    match classification.category {
                        crate::llm_classifier::MessageCategory::Chat => {
                            // ── Chat: fall through to agentic loop ───────
                            // Previously this returned immediately with the
                            // classifier's pre-generated text, but that caused
                            // the agent to "lie" — claiming it did work without
                            // ever calling tools. Now Chat is treated the same
                            // as Order: the model gets full tool access and
                            // decides whether to use them. For pure greetings
                            // and thanks, the model naturally responds without
                            // tools in the first iteration.
                            info!("V2: Chat classified — entering agentic loop with tools");
                            eigentune_complexity = "simple".to_string();
                            Some(crate::llm_classifier::TaskDifficulty::Simple.execution_profile())
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
                            // Capture complexity for Eigen-Tune routing (Phase 11)
                            eigentune_complexity = match classification.difficulty {
                                crate::llm_classifier::TaskDifficulty::Simple => "simple",
                                crate::llm_classifier::TaskDifficulty::Standard => "standard",
                                crate::llm_classifier::TaskDifficulty::Complex => "complex",
                            }
                            .to_string();
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

                    // Capture complexity for Eigen-Tune routing (Phase 11)
                    eigentune_complexity = match complexity {
                        crate::model_router::TaskComplexity::Trivial
                        | crate::model_router::TaskComplexity::Simple => "simple",
                        crate::model_router::TaskComplexity::Standard => "standard",
                        crate::model_router::TaskComplexity::Complex => "complex",
                    }
                    .to_string();

                    let profile = complexity.execution_profile();
                    info!(
                        complexity = ?complexity,
                        prompt_tier = ?profile.prompt_tier,
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
        // v5.3.6: stagnation detector — breaks the loop when the model gets
        // stuck calling the same tool with the same input and getting the
        // same result repeatedly. Conservative window (4) tolerates natural
        // 2-3 step retry flows.
        let mut stagnation = crate::stagnation::StagnationDetector::new();
        let mut stagnation_detected = false;
        // Observability: once-per-turn flag for the soft cost advisory warning.
        let mut soft_cost_warning_emitted = false;
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

            // v5.3.1: Duration::from_secs(0) = "unlimited" sentinel.
            // Reasoning models on long refactors routinely exceed 30 minutes;
            // cost/turn/tool-round caps are the real ceilings. Only enforce
            // the wall-clock when the user has explicitly opted in to a
            // positive duration (configured via [agent] max_task_duration_secs).
            if !self.max_task_duration.is_zero() && task_start.elapsed() > self.max_task_duration {
                warn!(
                    elapsed_secs = task_start.elapsed().as_secs(),
                    limit_secs = self.max_task_duration.as_secs(),
                    "Task duration exceeded limit, forcing text reply"
                );
                break;
            }

            // v5.3.6: max_tool_rounds = 0 means unlimited. Only enforce the
            // ceiling when the user has explicitly opted into a positive value.
            if self.max_tool_rounds > 0 && rounds > self.max_tool_rounds {
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

            // Role-based tool filtering + P6 per-runtime tool filter.
            // Both filters compose with AND: a tool must be permitted by the
            // session role AND pass the runtime filter (if set) to be visible.
            let effective_tools: Vec<Arc<dyn Tool>> = self
                .tools
                .iter()
                .filter(|t| {
                    let role_ok =
                        session.role.has_all_tools() || session.role.is_tool_allowed(t.name());
                    let filter_ok = match &self.tool_filter {
                        Some(f) => f(t.as_ref()),
                        None => true,
                    };
                    role_ok && filter_ok
                })
                .cloned()
                .collect();

            let mut request = build_context(
                session,
                self.memory.as_ref(),
                &effective_tools,
                &self.model,
                self.system_prompt.as_deref(),
                self.max_turns,
                self.max_context_tokens,
                prompt_tier,
                &matched_blueprints,
                lambda_enabled,
                self.personality.as_deref(),
            )
            .await;

            // ── Personality mode injection ──────────────────────────────
            // P2: route to volatile tail so the stable base stays cacheable.
            if let Some(ref shared_mode) = self.shared_mode {
                let mode = *shared_mode.read().await;
                let mode_block = if let Some(ref p) = self.personality {
                    p.generate_runtime_mode_block(mode)
                } else {
                    mode_prompt_block(mode)
                };
                request.prepend_system_volatile(&mode_block);
            }

            // ── Social intelligence: inject user profile into system prompt ──
            // P2: volatile (profile evolves between turns).
            if let (Some(storage), Some(config)) = (&self.social_storage, &self.social_config) {
                if config.enabled {
                    if let Ok(Some(profile)) = storage.get_profile(&msg.user_id).await {
                        let profile_section =
                            temm1e_anima::communication::section_user_profile(&profile);
                        if !profile_section.is_empty() {
                            request.append_system_volatile(&profile_section);
                        }
                    }
                }
            }

            // ── Perpetuum: temporal context injection ─────────────────────
            // P2: volatile (time-of-day changes every turn).
            if let Some(ref temporal) = self.perpetuum_temporal {
                let temporal_str = temporal.read().await.clone();
                if !temporal_str.is_empty() {
                    request.prepend_system_volatile(&temporal_str);
                }
            }

            // ── Tem Conscious: PRE-LLM consciousness (LLM-powered) ──────────
            // A separate LLM call that THINKS about the upcoming turn. Gated
            // by `turn_is_code_shaped` — on chat / channel turns the observer
            // has no codebase trajectory to reason about, so the LLM call is
            // pure +3-5s latency tax. On code-shaped turns we still want the
            // observer so it can flag drift or inefficiency.
            let consciousness_should_fire = self.consciousness.is_some()
                && turn_is_code_shaped(session.history.len(), &user_text).0;
            if let (true, Some(consciousness_observer)) =
                (consciousness_should_fire, self.consciousness.as_ref())
            {
                let pre_obs = crate::consciousness_engine::PreObservation {
                    user_message: user_text.clone(),
                    category: classification_label.clone(),
                    difficulty: difficulty_label.clone(),
                    turn_number: turn_api_calls + 1,
                    session_id: session.session_id.clone(),
                    cumulative_cost_usd: self.budget.total_spend_usd(),
                    budget_limit_usd: self.budget.max_spend_usd(),
                };
                let (injection, consciousness_usage) =
                    consciousness_observer.pre_observe(&pre_obs).await;
                if let Some(cu) = consciousness_usage {
                    turn_api_calls = turn_api_calls.saturating_add(1);
                    turn_input_tokens = turn_input_tokens.saturating_add(cu.input_tokens);
                    turn_output_tokens = turn_output_tokens.saturating_add(cu.output_tokens);
                    turn_cost_usd += cu.cost_usd;
                    self.budget
                        .record_usage(cu.input_tokens, cu.output_tokens, cu.cost_usd);
                }
                if let Some(injection) = injection {
                    had_whisper = true;
                    let consciousness_block = format!(
                        "{{{{consciousness}}}}\n\
                         [Your consciousness — a separate observer watching this conversation — shares this insight:]\n\
                         {}\n\
                         {{{{/consciousness}}}}",
                        injection
                    );
                    // P2: volatile (consciousness observer fires fresh per turn).
                    request.prepend_system_volatile(&consciousness_block);
                }
            }

            // ── Prompted mode: move tools from API body into system prompt ──
            // P2: volatile (tool list may change between turns; retry hint is per-turn).
            if prompted_mode && !request.tools.is_empty() {
                let tool_prompt = prompted_tool_calling::format_tools_prompt(&request.tools);
                request.append_system_volatile(&tool_prompt);
                // If this is a JSON retry, append the stricter instruction
                if prompted_json_retries > 0 {
                    let retry_hint = prompted_tool_calling::format_strict_retry_prompt();
                    request.append_system_volatile(retry_hint);
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

            // Pre-extract Eigen-Tune collection data so `request` can be
            // moved (not cloned) into the routing match below.
            let eigentune_collection = if self.eigen_tune.is_some() {
                Some((
                    serde_json::to_string(&request.messages).unwrap_or_default(),
                    request.system.clone(),
                    if request.tools.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_string(&request.tools).unwrap_or_default())
                    },
                ))
            } else {
                None
            };

            // ── Eigen-Tune routing decision (Phase 13) ───────────────
            // Triple gate before local routing fires:
            //   1. Engine must be set (Some(et))
            //   2. enable_local_routing must be true (double opt-in)
            //   3. request.tools must be empty (Gate 2 — small models lack function calling)
            // Default-config users (eigen_tune=None) skip this entirely and
            // hit the unchanged Cloud branch below.
            let eigentune_route = if let Some(ref et) = self.eigen_tune {
                if self.eigen_tune_local_routing && request.tools.is_empty() {
                    et.route(&eigentune_complexity).await
                } else {
                    temm1e_distill::types::RouteDecision::Cloud
                }
            } else {
                temm1e_distill::types::RouteDecision::Cloud
            };

            let response = match eigentune_route {
                temm1e_distill::types::RouteDecision::Cloud => {
                    // Default unchanged path — preserves the existing
                    // prompted-tool-calling fallback logic verbatim.
                    // `request` is moved (not cloned) — collection data was
                    // pre-extracted above.
                    match self.provider.complete(request).await {
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
                                    continue;
                                }
                            }
                            record_cb_failure_unless_rate_limit(&self.circuit_breaker, &e);
                            return Err(e);
                        }
                    }
                }

                temm1e_distill::types::RouteDecision::Local(endpoint) => {
                    // ── Gate 5: 30s timeout + automatic cloud fallback ────
                    let local_provider = temm1e_providers::OpenAICompatProvider::new(String::new())
                        .with_base_url(endpoint.base_url.clone());
                    let mut local_req = request.clone(); // 1 clone — need modified copy for local model
                    local_req.model = endpoint.model_name.clone();
                    let local_result = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        local_provider.complete(local_req),
                    )
                    .await;
                    match local_result {
                        Ok(Ok(resp)) => {
                            tracing::info!(
                                model = %endpoint.model_name,
                                tier = %eigentune_complexity,
                                "Eigen-Tune: served from local model"
                            );
                            self.circuit_breaker.record_success();
                            drop(request); // success — no fallback needed
                            resp
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(
                                model = %endpoint.model_name,
                                error = %e,
                                "Eigen-Tune: local call failed, falling back to cloud"
                            );
                            match self.provider.complete(request).await {
                                // move, not clone
                                Ok(resp) => {
                                    self.circuit_breaker.record_success();
                                    resp
                                }
                                Err(e) => {
                                    record_cb_failure_unless_rate_limit(&self.circuit_breaker, &e);
                                    return Err(e);
                                }
                            }
                        }
                        Err(_) => {
                            tracing::warn!(
                                model = %endpoint.model_name,
                                "Eigen-Tune: local call timed out (30s), falling back to cloud"
                            );
                            match self.provider.complete(request).await {
                                // move, not clone
                                Ok(resp) => {
                                    self.circuit_breaker.record_success();
                                    resp
                                }
                                Err(e) => {
                                    record_cb_failure_unless_rate_limit(&self.circuit_breaker, &e);
                                    return Err(e);
                                }
                            }
                        }
                    }
                }

                temm1e_distill::types::RouteDecision::Monitor(endpoint) => {
                    // Local serves; cloud sampled in parallel for CUSUM drift detection (Gate 4).
                    let local_provider = temm1e_providers::OpenAICompatProvider::new(String::new())
                        .with_base_url(endpoint.base_url.clone());
                    let mut local_req = request.clone(); // 1 clone — need modified copy for local model
                    local_req.model = endpoint.model_name.clone();
                    let local_result = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        local_provider.complete(local_req),
                    )
                    .await;
                    match local_result {
                        Ok(Ok(local_resp)) => {
                            // Fire-and-forget cloud comparison for CUSUM —
                            // move `request` into spawn instead of cloning
                            if let Some(et) = self.eigen_tune.clone() {
                                let cloud_provider = self.provider.clone();
                                let tier = temm1e_distill::types::EigenTier::from_str(
                                    &eigentune_complexity,
                                );
                                let local_text = response_to_text(&local_resp);
                                tokio::spawn(async move {
                                    if let Ok(Ok(cloud_resp)) = tokio::time::timeout(
                                        std::time::Duration::from_secs(30),
                                        cloud_provider.complete(request),
                                    )
                                    .await
                                    {
                                        let cloud_text = response_to_text(&cloud_resp);
                                        let agree = temm1e_distill::judge::embedding::cheap_equivalence_check(
                                            &local_text, &cloud_text,
                                        )
                                        .unwrap_or(true);
                                        et.on_monitor_observation(tier, agree).await;
                                    }
                                });
                            }
                            self.circuit_breaker.record_success();
                            local_resp
                        }
                        _ => {
                            tracing::warn!(
                                "Eigen-Tune: monitor-mode local call failed, falling back to cloud"
                            );
                            match self.provider.complete(request).await {
                                // move, not clone
                                Ok(resp) => {
                                    self.circuit_breaker.record_success();
                                    resp
                                }
                                Err(e) => {
                                    record_cb_failure_unless_rate_limit(&self.circuit_breaker, &e);
                                    return Err(e);
                                }
                            }
                        }
                    }
                }

                temm1e_distill::types::RouteDecision::Shadow(endpoint) => {
                    // Cloud serves the user; local runs in parallel for SPRT evidence.
                    // Clone request for the spawn; move original into cloud call.
                    let spawn_req = request.clone(); // 1 clone — needed for async spawn
                    let cloud_resp = match self.provider.complete(request).await {
                        // move
                        Ok(resp) => {
                            self.circuit_breaker.record_success();
                            resp
                        }
                        Err(e) => {
                            record_cb_failure_unless_rate_limit(&self.circuit_breaker, &e);
                            return Err(e);
                        }
                    };

                    if let Some(et) = self.eigen_tune.clone() {
                        let endpoint_clone = endpoint.clone();
                        let tier =
                            temm1e_distill::types::EigenTier::from_str(&eigentune_complexity);
                        let cloud_text = response_to_text(&cloud_resp);
                        tokio::spawn(async move {
                            let local_provider =
                                temm1e_providers::OpenAICompatProvider::new(String::new())
                                    .with_base_url(endpoint_clone.base_url.clone());
                            let mut local_req = spawn_req;
                            local_req.model = endpoint_clone.model_name.clone();
                            if let Ok(Ok(local_resp)) = tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                local_provider.complete(local_req),
                            )
                            .await
                            {
                                let local_text = response_to_text(&local_resp);
                                let agree =
                                    temm1e_distill::judge::embedding::cheap_equivalence_check(
                                        &local_text,
                                        &cloud_text,
                                    )
                                    .unwrap_or(true);
                                et.on_shadow_observation(tier, agree).await;
                            }
                        });
                    }
                    cloud_resp
                }
            };

            // ── Eigen-Tune: collection hook (fire-and-forget, Phase 11) ──
            // Uses pre-extracted data — `request` was moved into the route above.
            if let (Some((messages_json, system_prompt, tools_json)), Some(engine)) =
                (eigentune_collection, self.eigen_tune.as_ref())
            {
                let engine = engine.clone();
                let pair_data = temm1e_distill::collector::EigenTunePairData {
                    messages_json,
                    system_prompt,
                    tools_json,
                    response_json: serde_json::to_string(&response).unwrap_or_default(),
                    model: self.model.clone(),
                    provider: self.provider.name().to_string(),
                    complexity: eigentune_complexity.clone(),
                    conversation_id: msg.chat_id.clone(),
                    turn: session.history.len() as i32,
                    tokens_in: Some(response.usage.input_tokens),
                    tokens_out: Some(response.usage.output_tokens),
                    cost_usd: None, // call_cost is computed in the next block
                };
                tokio::spawn(async move {
                    engine.on_completion(pair_data).await;
                });
            }

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

            // Observability: soft warning when a single turn crosses the cost
            // advisory threshold. This is log-only — no behavior change. Users
            // who need a hard ceiling can set [agent] max_spend_usd in config.
            if turn_cost_usd > SOFT_TURN_COST_WARNING_USD && !soft_cost_warning_emitted {
                tracing::warn!(
                    turn_cost_usd = format!("{:.4}", turn_cost_usd),
                    threshold_usd = SOFT_TURN_COST_WARNING_USD,
                    rounds = rounds,
                    api_calls = turn_api_calls,
                    "High per-turn cost — if the model appears to be looping on an \
                     ambiguous prompt, consider setting [agent] max_spend_usd as a \
                     hard ceiling or issuing a /stop"
                );
                soft_cost_warning_emitted = true;
            }

            // ── Cancellation check point (v4.8.0) ───────────────────
            // The top-of-loop check catches cancels between rounds,
            // but single-round turns (text-only reply, no tool calls)
            // never iterate. Check here, AFTER the provider returns,
            // so a user pressing Escape during the provider call is
            // honored before we commit the response to history.
            if let Some(ref flag) = interrupt {
                if flag.load(Ordering::Relaxed) {
                    info!("Agent interrupted after provider call at round {}", rounds);
                    if let Some(ref tx) = status_tx {
                        tx.send_modify(|s| {
                            s.input_tokens = turn_input_tokens;
                            s.output_tokens = turn_output_tokens;
                            s.cost_usd = turn_cost_usd;
                            s.phase = AgentTaskPhase::Interrupted {
                                round: rounds as u32,
                            };
                        });
                    }
                    interrupted = true;
                    break;
                }
            }

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
                        recall_boost: 0.0,
                    };

                    if let Err(e) = self.memory.lambda_store(entry).await {
                        warn!(error = %e, "Failed to store λ-memory");
                    } else {
                        debug!(hash = %hash, "Stored λ-memory");
                    }
                }

                // Strip <memory> and <learning> blocks from text before user sees them
                for part in &mut text_parts {
                    *part = crate::lambda_memory::strip_memory_blocks(part);
                    *part = crate::learning::strip_learning_blocks(part);
                }
            }

            // If no tool calls, we have our final reply
            if tool_uses.is_empty() {
                // P5: log outcome-derived difficulty alongside intent-based label.
                // This is strictly more informative signal — reflects what actually
                // happened, not what the classifier predicted would happen. Kept as
                // a log line for now; future work will feed it into memory priors
                // and eigen-tune routing.
                let outcome_difficulty = derive_outcome_difficulty(rounds);
                tracing::info!(
                    rounds = rounds,
                    intent_difficulty = %difficulty_label,
                    outcome_difficulty = %outcome_difficulty,
                    "P5: turn outcome classification"
                );

                // ── Status: Finishing ────────────────────────────────
                if let Some(ref tx) = status_tx {
                    tx.send_modify(|s| {
                        s.phase = AgentTaskPhase::Finishing;
                    });
                }
                let mut reply_text = text_parts.join("\n");

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

                // ── Witness gate ─────────────────────────────────────
                // Phase 2: if a Witness is attached AND THIS turn sealed its
                // own Oath (via the Planner hook at the start of
                // process_message), run the Tier 0/1 verification pipeline
                // and rewrite reply_text per the configured strictness.
                //
                // We deliberately do NOT call `active_oath(session_id)` — a
                // turn that didn't seal its own Oath must not verify someone
                // else's (orphan Oaths from HiveRoute early-returns or
                // mid-session manual seals would otherwise get applied to
                // unrelated replies, producing false footers).
                //
                // Law 5 (Narrative-Only FAIL): any error in verification
                // leaves reply_text untouched — delivery is never blocked,
                // files are never mutated. Witness only controls the
                // narrative.
                if let (Some(witness), Some(oath)) =
                    (self.witness.as_ref(), oath_sealed_this_turn.as_ref())
                {
                    match witness.verify_oath(oath).await {
                        Ok(verdict) => {
                            tracing::info!(
                                session_id = %session.session_id,
                                outcome = ?verdict.outcome,
                                pass = verdict.pass_count(),
                                fail = verdict.fail_count(),
                                inconclusive = verdict.inconclusive_count(),
                                cost_usd = verdict.cost_usd,
                                latency_ms = verdict.latency_ms,
                                "witness verdict rendered"
                            );

                            // Phase 4: feed the verdict outcome into the
                            // Cambium TrustEngine if one is attached.
                            // Inconclusive verdicts are deliberately skipped
                            // — Witness couldn't decide, so trust shouldn't
                            // move either way.
                            if let Some(ref trust) = self.cambium_trust {
                                use temm1e_witness::types::VerdictOutcome;
                                match verdict.outcome {
                                    VerdictOutcome::Pass => {
                                        let mut t = trust.lock().await;
                                        t.record_verdict(
                                            true,
                                            temm1e_core::types::cambium::TrustLevel::AutonomousBasic,
                                        );
                                        tracing::debug!("cambium trust: recorded PASS verdict");
                                    }
                                    VerdictOutcome::Fail => {
                                        let mut t = trust.lock().await;
                                        t.record_verdict(
                                            false,
                                            temm1e_core::types::cambium::TrustLevel::AutonomousBasic,
                                        );
                                        tracing::debug!("cambium trust: recorded FAIL verdict");
                                    }
                                    VerdictOutcome::Inconclusive => {
                                        tracing::debug!(
                                            "cambium trust: skipping inconclusive verdict"
                                        );
                                    }
                                }
                            }

                            reply_text = witness.compose_final_reply_ex(
                                &reply_text,
                                &verdict,
                                self.witness_strictness,
                                self.witness_show_readout,
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session.session_id,
                                error = %e,
                                "witness verification error; reply unchanged (Law 5)"
                            );
                        }
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

                // ── Cross-Task Learning (V3: LLM-powered extraction) ─
                // Primary path: parse <learning> blocks emitted by the LLM
                // in the final response.  Fallback: legacy rule-based extraction
                // if the LLM did not emit a block (e.g. trivial task, short
                // conversation where the instruction was not in the prompt).
                let should_learn = execution_profile.as_ref().is_none_or(|p| p.use_learn);
                let learnings: Vec<learning::TaskLearning> = if should_learn
                    && learning::had_tool_use(&session.history)
                {
                    let tools_used = learning::collect_tools_used(&session.history);

                    // Try LLM-parsed learning first
                    let learning_opt = learning::parse_learning_block(&reply_text)
                        .map(|parsed| learning::learning_from_parsed(parsed, tools_used.clone()));

                    // Fallback to legacy if LLM didn't emit a <learning> block
                    match learning_opt {
                        Some(l) => vec![l],
                        None => learning::extract_learnings_legacy(&session.history),
                    }
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
                            "quality_alpha": l.quality_alpha,
                            "quality_beta": l.quality_beta,
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
                            quality = l.quality_alpha / (l.quality_alpha + l.quality_beta),
                            "Persisted task learning (V3)"
                        );
                    }
                }

                // ── Classification outcome tracking (v4.6.0 self-learning) ──
                {
                    let tools_count = tools_called_this_turn.len() as u32;
                    let tier_name = prompt_tier.map(|t| format!("{:?}", t)).unwrap_or_default();
                    let _ = self
                        .memory
                        .record_classification_outcome(
                            &classification_label,
                            &difficulty_label,
                            rounds as u32,
                            tools_count,
                            turn_cost_usd,
                            !interrupted,
                            &tier_name,
                            had_whisper,
                        )
                        .await;
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

                // ── Tem Conscious: POST-LLM consciousness (LLM-powered) ────
                // A separate LLM call that EVALUATES what just happened.
                // Gated to code-shaped turns to match pre_observe — avoid
                // paying the post-LLM round-trip when pre-observer also
                // skipped. Symmetric: observer fires as a pair or not at all.
                let post_consciousness_should_fire = self.consciousness.is_some()
                    && turn_is_code_shaped(session.history.len(), &user_text).0;
                if let (true, Some(consciousness_observer)) =
                    (post_consciousness_should_fire, self.consciousness.as_ref())
                {
                    let obs = crate::consciousness::TurnObservation {
                        turn_number: turn_api_calls,
                        session_id: session.session_id.clone(),
                        user_message_preview: crate::consciousness::safe_preview(&user_text, 200),
                        category: classification_label.clone(),
                        difficulty: difficulty_label.clone(),
                        model_used: self.model.clone(),
                        input_tokens: turn_input_tokens,
                        output_tokens: turn_output_tokens,
                        cost_usd: turn_cost_usd,
                        cumulative_cost_usd: self.budget.total_spend_usd(),
                        budget_limit_usd: self.budget.max_spend_usd(),
                        tools_called: tools_called_this_turn.clone(),
                        tool_results: tool_results_this_turn.clone(),
                        max_consecutive_failures: max_consecutive_failures_seen,
                        strategy_rotations: strategy_rotations_count,
                        response_preview: crate::consciousness::safe_preview(&reply_text, 200),
                        circuit_breaker_state: "active".to_string(),
                        previous_notes: consciousness_observer.session_notes(),
                    };
                    if let Some(cu) = consciousness_observer.post_observe(&obs).await {
                        turn_api_calls = turn_api_calls.saturating_add(1);
                        turn_input_tokens = turn_input_tokens.saturating_add(cu.input_tokens);
                        turn_output_tokens = turn_output_tokens.saturating_add(cu.output_tokens);
                        turn_cost_usd += cu.cost_usd;
                        self.budget
                            .record_usage(cu.input_tokens, cu.output_tokens, cu.cost_usd);
                    }
                }

                // ── Social intelligence: trigger background evaluation ─────
                if let (Some(storage), Some(social_config)) =
                    (&self.social_storage, &self.social_config)
                {
                    if social_config.enabled && !self.social_evaluating.load(Ordering::Relaxed) {
                        let turn_count = self.social_turn_count.load(Ordering::Relaxed);
                        let profile_data = storage.get_profile(&msg.user_id).await.ok().flatten();
                        let last_eval = profile_data
                            .as_ref()
                            .map(|p| p.last_evaluated_at)
                            .unwrap_or(0);
                        let effective_n = profile_data
                            .as_ref()
                            .map(|p| {
                                if p.n_next > 0 {
                                    p.n_next
                                } else {
                                    social_config.turn_interval
                                }
                            })
                            .unwrap_or(social_config.turn_interval);
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        if temm1e_anima::user_model::should_evaluate_raw(
                            turn_count,
                            last_eval,
                            now,
                            effective_n,
                            social_config.min_interval_seconds,
                        ) {
                            let eval_storage = storage.clone();
                            let eval_provider = self.provider.clone();
                            let eval_model = self.model.clone();
                            let eval_user_id = msg.user_id.clone();
                            let personality_name = self
                                .personality
                                .as_ref()
                                .map(|p| p.identity.name.clone())
                                .unwrap_or_else(|| "Tem".to_string());
                            let evaluating = self.social_evaluating.clone();
                            evaluating.store(true, Ordering::Relaxed);
                            tokio::spawn(async move {
                                let result = tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    run_social_evaluation(
                                        &eval_storage,
                                        eval_provider.as_ref(),
                                        &eval_model,
                                        &eval_user_id,
                                        &personality_name,
                                    ),
                                )
                                .await;
                                evaluating.store(false, Ordering::Relaxed);
                                match result {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => {
                                        tracing::debug!(error = %e, "Social evaluation failed")
                                    }
                                    Err(_) => {
                                        tracing::warn!("Social evaluation timed out after 30s")
                                    }
                                }
                            });
                            self.social_turn_count.store(0, Ordering::Relaxed);
                        }
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
                // ── Cancellation check point (v4.8.0) ─────────
                // Honor Escape/Ctrl+C between tools in a multi-tool round.
                if let Some(ref flag) = interrupt {
                    if flag.load(Ordering::Relaxed) {
                        info!(
                            "Agent interrupted before tool {} of {} at round {}",
                            tool_index + 1,
                            tool_total,
                            rounds
                        );
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

                turn_tools_used = turn_tools_used.saturating_add(1);
                info!(tool = %tool_name, id = %tool_use_id, "Executing tool call");

                // ── Status: ExecutingTool (v4.8.0 — enriched) ────
                let args_preview = truncate_json_preview(arguments, 80);
                let tool_started = Instant::now();
                if let Some(ref tx) = status_tx {
                    let tname = tool_name.clone();
                    let tidx = tool_index as u32;
                    let ttotal = tool_total;
                    let ap = args_preview.clone();
                    tx.send_modify(|s| {
                        let started_at_ms = s.started_at.elapsed().as_millis() as u64;
                        s.phase = AgentTaskPhase::ExecutingTool {
                            round: rounds as u32,
                            tool_name: tname,
                            tool_index: tidx,
                            tool_total: ttotal,
                            args_preview: ap,
                            started_at_ms,
                        };
                    });
                }

                let result = execute_tool(tool_name, arguments.clone(), &self.tools, session).await;
                let tool_duration_ms = tool_started.elapsed().as_millis() as u64;

                // ── Status: ToolCompleted (v4.8.0 — new variant) ─
                // Emit BEFORE the (potentially large) tool-output processing
                // block so observers see the completion promptly.
                let (completion_ok, completion_preview) = match &result {
                    Ok(out) => (!out.is_error, first_nonempty_line_preview(&out.content, 80)),
                    Err(e) => (false, first_nonempty_line_preview(&e.to_string(), 80)),
                };
                if let Some(ref tx) = status_tx {
                    let tname = tool_name.clone();
                    let tidx = tool_index as u32;
                    let ttotal = tool_total;
                    let preview = completion_preview.clone();
                    tx.send_modify(|s| {
                        s.phase = AgentTaskPhase::ToolCompleted {
                            round: rounds as u32,
                            tool_name: tname,
                            tool_index: tidx,
                            tool_total: ttotal,
                            duration_ms: tool_duration_ms,
                            ok: completion_ok,
                            result_preview: preview,
                        };
                    });
                }

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

                // ── Eigen-Tune: tool result signal (Phase 12, fire-and-forget) ──
                if let Some(et) = &self.eigen_tune {
                    let engine = et.clone();
                    let chat_id = msg.chat_id.clone();
                    let signal = if is_error {
                        temm1e_distill::types::QualitySignal::ResponseError
                    } else {
                        temm1e_distill::types::QualitySignal::ToolCallSucceeded
                    };
                    tokio::spawn(async move {
                        engine.on_signal(&chat_id, signal).await;
                    });
                }

                // ── Stagnation detection (P4) ─────────────────────────
                // Observe (tool_name, input, result). When the same call with
                // the same input produces the same result N times in a row,
                // break the loop and let the final-reply block ask the model
                // to synthesize what it has.
                if let crate::stagnation::StagnationSignal::Stuck { count } =
                    stagnation.observe(tool_name, arguments, &content)
                {
                    tracing::warn!(
                        tool = %tool_name,
                        count = count,
                        "Stagnation detected — {} identical (call, result) pairs. Forcing synthesis.",
                        count
                    );
                    stagnation_detected = true;
                    break;
                }

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
                        strategy_rotations_count += 1;
                        content.push_str(&rotation_prompt);
                    }
                } else {
                    failure_tracker.record_success(tool_name);
                }

                // Tem Conscious: track tool calls and results for observation
                tools_called_this_turn.push(tool_name.to_string());
                if is_error {
                    tool_results_this_turn.push(crate::consciousness::safe_preview(&content, 100));
                    let fc = failure_tracker.failure_count(tool_name) as u32;
                    if fc > max_consecutive_failures_seen {
                        max_consecutive_failures_seen = fc;
                    }
                } else {
                    tool_results_this_turn.push("success".to_string());
                }

                // Tool reliability tracking (v4.6.0 self-learning)
                {
                    let task_label = format!("{}:{}", &classification_label, &difficulty_label);
                    let _ = self
                        .memory
                        .record_tool_outcome(tool_name, &task_label, !is_error)
                        .await;
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

            // If the per-tool cancel check fired, break out of the main
            // tool-use loop too (the for-loop break above only exits the
            // inner tool iteration).
            if interrupted {
                break;
            }

            // Stagnation break from inner for-loop → exit outer round loop too.
            // The final-reply block below handles synthesis from what we have.
            if stagnation_detected {
                break;
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

        // Fallback: exited loop due to interruption, stagnation, or max rounds.
        // P5: emit outcome-derived difficulty label for observability. This is
        // strictly more informative than the classifier's intent-based label —
        // it reflects what actually happened, not what we guessed would happen.
        let outcome_difficulty = derive_outcome_difficulty(rounds);
        tracing::info!(
            rounds = rounds,
            intent_difficulty = %difficulty_label,
            outcome_difficulty = %outcome_difficulty,
            "P5: turn outcome classification (fallback-path)"
        );

        let text = if interrupted {
            // Task was cancelled — no resume capability exists.
            // Keep message short and factual. No false promises.
            "Task stopped.".to_string()
        } else if stagnation_detected {
            "I was looping on the same action without making progress. Here is what I have so far."
                .to_string()
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

    /// Atomic snapshot of this runtime's accumulated token/cost totals.
    /// Used by the JIT swarm worker flow to return usage to the parent
    /// without sharing a mutable BudgetTracker instance.
    pub fn budget_snapshot(&self) -> budget::BudgetSnapshot {
        self.budget.snapshot()
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
        // Per project rule (feedback_no_max_tokens): never hardcode output
        // caps. Let the provider adapter route to the model's declared max.
        max_tokens: None,
        temperature: Some(0.3),
        system: Some(
            "You are a technical writer. Output SKIP if the task is not worth a blueprint, \
             or the full Blueprint document otherwise. Nothing else."
                .to_string(),
        ),
        system_volatile: None,
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
        // Per project rule (feedback_no_max_tokens): never hardcode caps.
        max_tokens: None,
        temperature: Some(0.3),
        system: Some(
            "You are a technical writer. Output only the updated Blueprint document, nothing else."
                .to_string(),
        ),
        system_volatile: None,
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

/// Run social intelligence evaluation in background.
/// Called every N turns to update user profile via an LLM evaluation pass.
async fn run_social_evaluation(
    storage: &SocialStorage,
    provider: &dyn Provider,
    model: &str,
    user_id: &str,
    personality_name: &str,
) -> Result<(), Temm1eError> {
    // 1. Read current profile
    let mut profile = storage
        .get_profile(user_id)
        .await?
        .unwrap_or_else(|| temm1e_anima::user_model::new_profile(user_id));

    // 2. Read facts buffer
    let facts = storage.get_buffered_facts(user_id).await?;
    if facts.is_empty() {
        return Ok(()); // Nothing to evaluate
    }

    // 3. Build evaluation prompt
    let (system_prompt, user_prompt) =
        temm1e_anima::evaluator::build_evaluation_prompt(&profile, &facts, personality_name);

    // 4. Call LLM
    let request = temm1e_core::types::message::CompletionRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(user_prompt),
        }],
        tools: Vec::new(),
        temperature: Some(0.3),
        max_tokens: None,
        system: Some(system_prompt),
        system_volatile: None,
    };
    let response = provider.complete(request).await?;
    let response_text = extract_text_from_response(&response.content);

    // 5. Parse and apply
    let eval = temm1e_anima::evaluator::parse_evaluation_output(&response_text)?;
    temm1e_anima::evaluator::apply_evaluation(&mut profile, &eval);

    // 6. Persist
    storage.upsert_profile(&profile).await?;
    let eval_json = serde_json::to_string(&eval).unwrap_or_default();
    storage
        .log_evaluation(user_id, &eval_json, model, response.usage.output_tokens)
        .await?;
    storage.clear_buffer(user_id).await?;

    // 7. Store observations
    for obs in &eval.observations {
        storage.add_observation(user_id, obs).await?;
    }

    info!(
        user_id = %user_id,
        eval_count = profile.evaluation_count,
        "Social evaluation complete"
    );
    Ok(())
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

    // ── turn_is_code_shaped — shared gate for Witness + Consciousness ──

    #[test]
    fn turn_is_code_shaped_skips_trivial_and_simple() {
        assert!(!turn_is_code_shaped(0, "hey").0);
        assert!(!turn_is_code_shaped(0, "ok thanks").0);
        assert!(!turn_is_code_shaped(0, "yes").0);
    }

    #[test]
    fn turn_is_code_shaped_skips_standard_without_code_signal() {
        // These classify as Standard but have no code signal → skip.
        assert!(
            !turn_is_code_shaped(
                0,
                "Write a single haiku about the Rust borrow checker. Reply with only the haiku."
            )
            .0,
            "haiku request should not activate observers"
        );
        assert!(
            !turn_is_code_shaped(
                0,
                "What is 73 * 84? Show the multiplication step-by-step in two short lines."
            )
            .0,
            "math QA should not activate observers"
        );
        assert!(
            !turn_is_code_shaped(
                0,
                "Suggest 3 catchy product names for an AI agent runtime. Just the names, comma-separated."
            ).0,
            "creative chat should not activate observers"
        );
    }

    #[test]
    fn turn_is_code_shaped_fires_on_code_prompts() {
        assert!(
            turn_is_code_shaped(
                0,
                "In the workspace, write `demo.rs` with pub fn greet(name: &str) -> String."
            )
            .0,
            "Rust file work should activate observers"
        );
        assert!(
            turn_is_code_shaped(0, "use file_write to create manifest.json with two fields").0,
            "file_write tool request should activate observers"
        );
        assert!(
            turn_is_code_shaped(0, "refactor src/oath.rs to split the helper").0,
            "source file path should activate observers"
        );
    }

    #[test]
    fn turn_is_code_shaped_respects_code_fence() {
        assert!(
            turn_is_code_shaped(0, "Here is a snippet:\n```rust\nfn x() {}\n```\nexplain it").0,
            "triple-backtick code fence is a valid code signal"
        );
    }

    // ── P5: outcome-derived difficulty helper ───────────────────

    #[test]
    fn outcome_difficulty_tiers() {
        assert_eq!(derive_outcome_difficulty(0), "simple");
        assert_eq!(derive_outcome_difficulty(1), "simple");
        assert_eq!(derive_outcome_difficulty(2), "simple");
        assert_eq!(derive_outcome_difficulty(3), "standard");
        assert_eq!(derive_outcome_difficulty(5), "standard");
        assert_eq!(derive_outcome_difficulty(10), "standard");
        assert_eq!(derive_outcome_difficulty(11), "complex");
        assert_eq!(derive_outcome_difficulty(100), "complex");
    }

    // ── P6: tool filter composition ─────────────────────────────

    #[test]
    fn tool_filter_closure_composes_correctly() {
        // We can't trivially construct a full AgentRuntime in a unit test
        // (requires real Provider/Memory/Tool instances). Instead, verify the
        // filter closure type + behaviour that the runtime will compose.
        //
        // In runtime.rs:1206, effective_tools is built from:
        //   role_ok AND filter_ok
        // where filter_ok = tool_filter.map_or(true, |f| f(t))
        //
        // This test mirrors that logic with mock inputs.
        struct MockTool(&'static str);

        fn role_ok(tool_name: &str) -> bool {
            // "admin" role sees everything.
            tool_name != "blocked_by_role"
        }

        let filter: Arc<dyn Fn(&MockTool) -> bool + Send + Sync> =
            Arc::new(|t| t.0 != "spawn_swarm");

        let tools = [
            MockTool("shell"),
            MockTool("spawn_swarm"),
            MockTool("blocked_by_role"),
        ];

        let visible: Vec<&str> = tools
            .iter()
            .filter(|t| role_ok(t.0) && filter(t))
            .map(|t| t.0)
            .collect();

        assert_eq!(visible, ["shell"]);
    }

    #[test]
    fn tool_filter_none_permits_all_role_tools() {
        // When tool_filter is None, only role filter applies.
        struct MockTool(&'static str);

        fn role_ok(tool_name: &str) -> bool {
            tool_name != "blocked_by_role"
        }

        type MockFilter = Arc<dyn Fn(&MockTool) -> bool + Send + Sync>;
        let filter: Option<MockFilter> = None;

        let tools = [
            MockTool("shell"),
            MockTool("spawn_swarm"),
            MockTool("blocked_by_role"),
        ];

        let visible: Vec<&str> = tools
            .iter()
            .filter(|t| {
                let r = role_ok(t.0);
                let f = match &filter {
                    Some(ff) => ff(t),
                    None => true,
                };
                r && f
            })
            .map(|t| t.0)
            .collect();

        // With filter=None, spawn_swarm is permitted (role_ok allows it).
        assert_eq!(visible, vec!["shell", "spawn_swarm"]);
    }

    // ── Circuit breaker rate-limit exemption (P1) ───────────────

    #[test]
    fn rate_limit_does_not_trip_cb() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(30));
        let rate_limit = Temm1eError::RateLimited("429".into());

        // Feed 10 rate-limit "failures" — CB must stay Closed.
        for _ in 0..10 {
            record_cb_failure_unless_rate_limit(&cb, &rate_limit);
        }
        assert_eq!(
            cb.state(),
            crate::circuit_breaker::CircuitState::Closed,
            "CB should remain Closed after rate-limit errors only"
        );
        assert!(cb.can_execute());
    }

    #[test]
    fn provider_error_still_trips_cb() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(30));
        let provider_err = Temm1eError::Provider("500 bad gateway".into());

        for _ in 0..3 {
            record_cb_failure_unless_rate_limit(&cb, &provider_err);
        }
        assert_eq!(
            cb.state(),
            crate::circuit_breaker::CircuitState::Open,
            "CB should Open after threshold provider errors"
        );
    }

    #[test]
    fn mixed_errors_only_non_rate_limit_count() {
        let cb = CircuitBreaker::new(3, std::time::Duration::from_secs(30));
        let rate_limit = Temm1eError::RateLimited("429".into());
        let auth_err = Temm1eError::Auth("bad key".into());

        // Interleave — only Auth counts toward threshold.
        record_cb_failure_unless_rate_limit(&cb, &rate_limit);
        record_cb_failure_unless_rate_limit(&cb, &auth_err);
        record_cb_failure_unless_rate_limit(&cb, &rate_limit);
        record_cb_failure_unless_rate_limit(&cb, &auth_err);
        // 2 Auth failures so far → still Closed
        assert_eq!(cb.state(), crate::circuit_breaker::CircuitState::Closed);

        record_cb_failure_unless_rate_limit(&cb, &auth_err);
        // 3 Auth failures → threshold hit → Open
        assert_eq!(cb.state(), crate::circuit_breaker::CircuitState::Open);
    }

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
