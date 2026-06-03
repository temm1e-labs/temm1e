use crate::types::error::Temm1eError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── λ-Memory Types ─────────────────────────────────────────────

/// A single λ-memory entry with three fidelity layers.
///
/// Created with full/summary/essence at write time.
/// Decay score is computed lazily at read time — never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaMemoryEntry {
    /// Unique hash identifier (first 12 hex chars of SHA-256).
    pub hash: String,
    /// Unix epoch seconds when created.
    pub created_at: u64,
    /// Unix epoch seconds when last accessed (recalled or created).
    pub last_accessed: u64,
    /// Number of times recalled via lambda_recall tool.
    pub access_count: u32,
    /// Importance score assigned by LLM at creation (1.0–5.0).
    pub importance: f32,
    /// Whether the user explicitly asked to remember this.
    pub explicit_save: bool,
    /// Full-fidelity content (user message + assistant core response).
    pub full_text: String,
    /// One-sentence summary (LLM-generated at creation).
    pub summary_text: String,
    /// Five-word-max essence (LLM-generated at creation).
    pub essence_text: String,
    /// Up to 5 tags (LLM-generated at creation).
    pub tags: Vec<String>,
    /// Whether this is a conversation memory, knowledge, or learning.
    pub memory_type: LambdaMemoryType,
    /// Session that created this memory.
    pub session_id: String,
    /// Additive importance boost from recalls (+0.3 per recall, capped at 2.0).
    /// GC applies -0.1 penalty for entries with no access since last sweep.
    #[serde(default)]
    pub recall_boost: f32,
}

/// Classification of λ-memory entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LambdaMemoryType {
    /// Normal conversation turn memory.
    Conversation,
    /// Persistent knowledge (replaces old MemoryEntryType::Knowledge in context).
    Knowledge,
    /// Cross-task learning (replaces old learnings in context).
    Learning,
}

// ── Engram (permanent memory) types ────────────────────────────

/// Who pinned an Engram fact. `User` pins are sacrosanct — never annealed,
/// never auto-demoted, and the curator may not touch them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PinnedBy {
    #[default]
    None,
    Agent,
    User,
}

/// Visibility scope of an Engram fact — the injection filter that prevents
/// cross-user / cross-chat leakage on a multi-channel gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum MemoryScope {
    /// Visible in every session.
    #[default]
    Global,
    /// Visible only to a specific user id (across that user's chats).
    User(String),
    /// Visible only within a specific chat id.
    Chat(String),
}

impl MemoryScope {
    /// Whether a fact with this scope is visible in the given session.
    pub fn visible_in(&self, user_id: &str, chat_id: &str) -> bool {
        match self {
            MemoryScope::Global => true,
            MemoryScope::User(u) => u == user_id,
            MemoryScope::Chat(c) => c == chat_id,
        }
    }
    /// Stable string key for storage ("global" | "user:ID" | "chat:ID").
    pub fn as_key(&self) -> String {
        match self {
            MemoryScope::Global => "global".to_string(),
            MemoryScope::User(u) => format!("user:{u}"),
            MemoryScope::Chat(c) => format!("chat:{c}"),
        }
    }
    /// Parse the stored key form back into a scope.
    pub fn from_key(s: &str) -> Self {
        if let Some(u) = s.strip_prefix("user:") {
            MemoryScope::User(u.to_string())
        } else if let Some(c) = s.strip_prefix("chat:") {
            MemoryScope::Chat(c.to_string())
        } else {
            MemoryScope::Global
        }
    }
}

/// Semantic category of an Engram fact (drives grouping in the rendered block).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FactType {
    Identity,
    Preference,
    Project,
    Constraint,
    #[default]
    Reference,
}

/// A single Engram (permanent-memory) fact. Stored in its own `engram_facts`
/// table; reuses the Engram scoring engine (`temm1e_agent::engram`) for tiering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramFact {
    /// Stable id (12-hex content hash, or a uuid).
    pub id: String,
    /// Full-fidelity content.
    pub content: String,
    /// One-line summary (mid fidelity).
    pub summary: String,
    /// Five-word-max essence (low fidelity).
    pub essence: String,
    pub fact_type: FactType,
    pub scope: MemoryScope,
    pub pinned_by: PinnedBy,
    /// Supersession key — a newer fact with the same key replaces the older.
    pub subject_key: Option<String>,
    /// Stored importance `I ∈ [0,5]` (seed = first judgment, EMA on update).
    pub importance: f32,
    /// Unix epoch seconds when created.
    pub created_at: u64,
    /// Unix epoch seconds — reset on use; drives the lazy anneal.
    pub last_accessed: u64,
    pub tags: Vec<String>,
    /// Ids of related facts (`[[links]]`).
    pub links: Vec<String>,
}

/// A single memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub session_id: Option<String>,
    pub entry_type: MemoryEntryType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MemoryEntryType {
    Conversation,
    LongTerm,
    DailyLog,
    Skill,
    Knowledge,
    Blueprint,
}

/// Search options for memory queries
#[derive(Debug, Clone)]
pub struct SearchOpts {
    pub limit: usize,
    pub vector_weight: f32,
    pub keyword_weight: f32,
    pub session_filter: Option<String>,
    pub entry_type_filter: Option<MemoryEntryType>,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            limit: 10,
            vector_weight: 0.7,
            keyword_weight: 0.3,
            session_filter: None,
            entry_type_filter: None,
        }
    }
}

/// Memory backend trait — persistence for conversations, long-term memory, and skills
#[async_trait]
pub trait Memory: Send + Sync {
    /// Store a memory entry
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError>;

    /// Hybrid search: vector similarity + keyword matching
    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError>;

    /// Get a specific memory entry by ID
    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError>;

    /// Delete a memory entry
    async fn delete(&self, id: &str) -> Result<(), Temm1eError>;

    /// List all sessions
    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError>;

    /// Get conversation history for a session
    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError>;

    /// Backend name (e.g., "sqlite", "postgres", "markdown")
    fn backend_name(&self) -> &str;

    // ── λ-Memory methods (default no-op for backends that don't support it) ──

    /// Store a λ-memory entry.
    async fn lambda_store(&self, _entry: LambdaMemoryEntry) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Query λ-memory candidates ordered by importance DESC, limited to `limit`.
    async fn lambda_query_candidates(
        &self,
        _limit: usize,
    ) -> Result<Vec<LambdaMemoryEntry>, Temm1eError> {
        Ok(Vec::new())
    }

    /// Look up a λ-memory by hash prefix.
    async fn lambda_recall(
        &self,
        _hash_prefix: &str,
    ) -> Result<Option<LambdaMemoryEntry>, Temm1eError> {
        Ok(None)
    }

    /// Update last_accessed and increment access_count for a recalled memory.
    async fn lambda_touch(&self, _hash: &str) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// FTS5 search returning (hash, bm25_rank) pairs.
    async fn lambda_fts_search(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<(String, f64)>, Temm1eError> {
        Ok(Vec::new())
    }

    /// Garbage collect expired λ-memories. Returns count of deleted entries.
    async fn lambda_gc(&self, _now_epoch: u64, _max_age_secs: u64) -> Result<usize, Temm1eError> {
        Ok(0)
    }

    /// Update a λ-memory entry in place (for dedup merge).
    async fn lambda_update_entry(&self, _entry: &LambdaMemoryEntry) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Delete a λ-memory entry by hash.
    async fn lambda_delete(&self, _hash: &str) -> Result<(), Temm1eError> {
        Ok(())
    }

    // ── Tool reliability (v4.6.0) ─────────────────────────────────

    /// Record a tool execution outcome for cross-session reliability tracking.
    async fn record_tool_outcome(
        &self,
        _tool_name: &str,
        _task_type: &str,
        _success: bool,
    ) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Get tool reliability records (last 30 days, top 50 by sample size).
    async fn get_tool_reliability(&self) -> Result<Vec<ToolReliabilityRecord>, Temm1eError> {
        Ok(Vec::new())
    }

    // ── Classification outcomes (v4.6.0) ──────────────────────────

    /// Record a classification outcome for empirical prior tracking.
    #[allow(clippy::too_many_arguments)]
    async fn record_classification_outcome(
        &self,
        _category: &str,
        _difficulty: &str,
        _rounds: u32,
        _tools_used: u32,
        _cost_usd: f64,
        _success: bool,
        _prompt_tier: &str,
        _had_whisper: bool,
    ) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Get aggregated classification priors (last 30 days).
    async fn get_classification_priors(&self) -> Result<Vec<ClassificationPrior>, Temm1eError> {
        Ok(Vec::new())
    }

    // ── Skill usage (v4.6.0) ──────────────────────────────────────

    /// Record a skill invocation.
    async fn record_skill_usage(&self, _skill_name: &str) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Get skill usage records ordered by invocations DESC.
    async fn get_skill_usage(&self) -> Result<Vec<SkillUsageRecord>, Temm1eError> {
        Ok(Vec::new())
    }

    // ── Model discipline (GH-62, v5.6.0) ──────────────────────────
    // Per-(provider, model) telemetry for the Self-Audit Pass:
    // counts how often each model exits via text-only and how the
    // audit verdicts break down. Observability-only in v5.6.0; feeds
    // adaptive auto-disable in v5.7.0+.

    /// Record one outcome of the Self-Audit gate for a (provider, model).
    /// `was_text_only` distinguishes "audit ran" (true) from "audit was
    /// skipped because the turn ended via tool calls" (false).
    async fn record_audit_outcome(
        &self,
        _provider: &str,
        _model: &str,
        _outcome: AuditOutcomeKind,
        _was_text_only: bool,
    ) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// Fetch the discipline counters for one (provider, model) pair.
    async fn get_model_discipline(
        &self,
        _provider: &str,
        _model: &str,
    ) -> Result<Option<ModelDiscipline>, Temm1eError> {
        Ok(None)
    }

    // ── Engram (permanent memory) methods (default no-op) ─────────

    /// Store (insert-or-replace) an Engram fact.
    async fn engram_store(&self, _fact: EngramFact) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// List Engram facts visible in the given session scope, importance DESC.
    async fn engram_list(
        &self,
        _user_id: &str,
        _chat_id: &str,
        _limit: usize,
    ) -> Result<Vec<EngramFact>, Temm1eError> {
        Ok(Vec::new())
    }

    /// Fetch one Engram fact by id.
    async fn engram_get(&self, _id: &str) -> Result<Option<EngramFact>, Temm1eError> {
        Ok(None)
    }

    /// Look up an Engram fact by its supersession `subject_key` within a scope.
    async fn engram_by_subject(
        &self,
        _subject_key: &str,
        _user_id: &str,
        _chat_id: &str,
    ) -> Result<Option<EngramFact>, Temm1eError> {
        Ok(None)
    }

    /// Delete an Engram fact by id.
    async fn engram_forget(&self, _id: &str) -> Result<(), Temm1eError> {
        Ok(())
    }

    /// FTS recall over Engram facts visible in the given scope.
    async fn engram_recall(
        &self,
        _query: &str,
        _user_id: &str,
        _chat_id: &str,
        _limit: usize,
    ) -> Result<Vec<EngramFact>, Temm1eError> {
        Ok(Vec::new())
    }

    /// GC: delete non-pinned Engram facts whose effective importance has
    /// annealed below the archive floor. Returns the count deleted.
    async fn engram_gc(&self, _now_epoch: u64) -> Result<usize, Temm1eError> {
        Ok(0)
    }
}

// ── Self-learning record types (v4.6.0) ───────────────────────────

/// Tool reliability record — success/failure rates per (tool, task_type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReliabilityRecord {
    pub tool_name: String,
    pub task_type: String,
    pub successes: u32,
    pub failures: u32,
    pub last_updated: u64,
}

impl ToolReliabilityRecord {
    pub fn success_rate(&self) -> f64 {
        let total = self.successes + self.failures;
        if total == 0 {
            return 0.5;
        }
        self.successes as f64 / total as f64
    }
}

/// Classification empirical prior — aggregated resource usage per (category, difficulty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationPrior {
    pub category: String,
    pub difficulty: String,
    pub avg_rounds: f32,
    pub avg_tools: f32,
    pub avg_cost: f64,
    pub count: u32,
}

/// Skill usage record — invocation tracking per skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUsageRecord {
    pub skill_name: String,
    pub invocations: u32,
    pub last_invoked_at: u64,
}

/// Outcome categories for the Self-Audit Pass (GH-62).
///
/// Recorded per (provider, model) so v5.7.0+ can adaptively disable the
/// audit for models that prove disciplined, and force-enable it for
/// models that prove undisciplined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcomeKind {
    /// Model confirmed completion with the [DONE] marker — the original
    /// pre-audit text is the user-facing reply.
    Done,
    /// Audit prompted the model to emit the tool call it had previously
    /// promised but skipped — the loop continues.
    ToolCallTriggered,
    /// Audit response was malformed (no [DONE], no tool call). Fail-open:
    /// loop exits with the original text. No worse than baseline.
    FailedOpen,
    /// Audit was eligible but skipped (cost cap, hard cap reached, etc.).
    Skipped,
}

/// Aggregated Self-Audit telemetry for one (provider, model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDiscipline {
    pub provider: String,
    pub model: String,
    /// Total turns where the loop reached the audit branch (text-only with
    /// tools available) — i.e. the audit COULD have run.
    pub text_only_exits: u64,
    pub audit_done_responses: u64,
    pub audit_tool_call_responses: u64,
    pub audit_failed_responses: u64,
    pub audit_skipped: u64,
    pub last_updated: u64,
}

impl ModelDiscipline {
    /// How often, on text-only exits, the audit caught a stalled promise.
    /// Higher means the model is undisciplined and benefits from auditing.
    pub fn stall_catch_rate(&self) -> f64 {
        let audited = self.audit_done_responses
            + self.audit_tool_call_responses
            + self.audit_failed_responses;
        if audited == 0 {
            return 0.0;
        }
        self.audit_tool_call_responses as f64 / audited as f64
    }
}
