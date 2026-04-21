use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::error::Temm1eError;

/// Temm1e's operational personality mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Temm1eMode {
    /// PLAY mode — hype, warm, chaotic, :3-permitted
    #[default]
    Play,
    /// WORK mode — sharp, analytical, precise, >:3-permitted
    Work,
    /// PRO mode — professional, business-grade, no emoticons
    Pro,
    /// NONE mode — no personality, minimal identity prompt only
    None,
}

impl std::fmt::Display for Temm1eMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Temm1eMode::Play => write!(f, "PLAY :3"),
            Temm1eMode::Work => write!(f, "WORK >:3"),
            Temm1eMode::Pro => write!(f, "PRO"),
            Temm1eMode::None => write!(f, "NONE"),
        }
    }
}

/// Top-level TEMM1E configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Temm1eConfig {
    /// Temm1e's personality mode (play or work). Defaults to play.
    #[serde(default)]
    pub mode: Temm1eMode,
    #[serde(default)]
    pub temm1e: Temm1eSection,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub vault: VaultConfig,
    #[serde(default)]
    pub filestore: FileStoreConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub cron: CronConfig,
    #[serde(default)]
    pub channel: HashMap<String, ChannelConfig>,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub tunnel: Option<TunnelConfig>,
    #[serde(default)]
    pub observability: ObservabilityConfig,
    #[serde(default)]
    pub gaze: GazeConfig,
    #[serde(default)]
    pub consciousness: ConsciousnessConfig,
    #[serde(default)]
    pub perpetuum: PerpetualConfig,
    #[serde(default)]
    pub vigil: VigilConfig,
    #[serde(default)]
    pub social: SocialConfig,
    #[serde(default)]
    pub cambium: CambiumConfig,
    #[serde(default)]
    pub witness: WitnessConfig,
}

/// Social intelligence configuration — user profiling and emotional intelligence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialConfig {
    /// Enable social intelligence system
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Evaluate user profile every N turns
    #[serde(default = "default_social_turn_interval")]
    pub turn_interval: u32,
    /// Minimum seconds between evaluations
    #[serde(default = "default_social_min_interval")]
    pub min_interval_seconds: u64,
    /// Maximum turns to buffer before forcing evaluation
    #[serde(default = "default_social_max_buffer")]
    pub max_buffer_turns: u32,
}

fn default_social_turn_interval() -> u32 {
    5
}
fn default_social_min_interval() -> u64 {
    120
}
fn default_social_max_buffer() -> u32 {
    30
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            turn_interval: default_social_turn_interval(),
            min_interval_seconds: default_social_min_interval(),
            max_buffer_turns: default_social_max_buffer(),
        }
    }
}

/// Cambium configuration — gap-driven code evolution.
///
/// Cambium is the layer where Tem grows new capabilities at the edge while
/// the heartwood (immutable kernel) stays stable. Named after the biological
/// cambium — the thin layer of growth tissue under tree bark where new wood
/// is added each year.
///
/// **Enabled by default.** Users can opt out via `/cambium off` slash
/// command at runtime, or set `enabled = false` in `[cambium]` section of
/// the config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CambiumConfig {
    /// Master switch. When false, no cambium activity occurs.
    /// Default: true (enabled).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum lines of code changed per cambium session.
    #[serde(default = "default_sg_max_lines")]
    pub max_lines_per_session: usize,
    /// Maximum files touched per cambium session.
    #[serde(default = "default_sg_max_files")]
    pub max_files_per_session: usize,
    /// Maximum cambium sessions per 24-hour period.
    #[serde(default = "default_sg_max_sessions")]
    pub max_sessions_per_day: usize,
    /// Cooldown in seconds between cambium sessions.
    #[serde(default = "default_sg_cooldown")]
    pub cooldown_secs: u64,
    /// Cooldown in seconds after a failed session.
    #[serde(default = "default_sg_failure_cooldown")]
    pub failure_cooldown_secs: u64,
    /// Trust level override. None = use earned trust state machine.
    #[serde(default)]
    pub trust_level_override: Option<String>,
    /// Path to codebase self-model docs (relative to workspace root).
    #[serde(default = "default_sg_self_model_path")]
    pub self_model_path: String,
    /// Wire 4: auto-deploy after successful pipeline run. When false
    /// (default), pipeline commits to a branch only. When true, pipeline
    /// Stage 11 calls `Deployer::swap()` to replace the running binary.
    /// Requires track record of successful sessions before enabling.
    #[serde(default)]
    pub auto_deploy: bool,
    /// Wire 2: route vigil-detected bugs to cambium inbox. When true,
    /// Vigil writes bug signatures to ~/.temm1e/cambium/inbox.jsonl
    /// after creating a GitHub issue. Users can review the inbox and
    /// run `/cambium grow fix <bug>` to address specific bugs.
    #[serde(default = "default_true")]
    pub vigil_bridge_enabled: bool,
    /// Wire 5: Anima wish-pattern detection. When true, Anima scans
    /// recent conversations for "I wish you could…" patterns and
    /// surfaces suggestions for `/cambium grow`. Default on.
    #[serde(default = "default_true")]
    pub wish_detection_enabled: bool,
}

fn default_sg_max_lines() -> usize {
    500
}
fn default_sg_max_files() -> usize {
    5
}
fn default_sg_max_sessions() -> usize {
    3
}
fn default_sg_cooldown() -> u64 {
    3600
}
fn default_sg_failure_cooldown() -> u64 {
    86400
}
fn default_sg_self_model_path() -> String {
    "docs/lab/cambium".to_string()
}

impl Default for CambiumConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_lines_per_session: default_sg_max_lines(),
            max_files_per_session: default_sg_max_files(),
            max_sessions_per_day: default_sg_max_sessions(),
            cooldown_secs: default_sg_cooldown(),
            failure_cooldown_secs: default_sg_failure_cooldown(),
            trust_level_override: None,
            self_model_path: default_sg_self_model_path(),
            // SAFETY: auto_deploy defaults to false. Users must explicitly
            // opt in. Wire 4 is the highest-risk wire — it actually replaces
            // the running binary. Enable only after Wire 1 has a track record.
            auto_deploy: false,
            vigil_bridge_enabled: true,
            wish_detection_enabled: true,
        }
    }
}

/// Witness verification configuration — pre-committed Oath + verdict gating.
///
/// Wires three independent hooks into the agent runtime:
/// - `with_witness` — verifier gate at end of `process_message`
/// - `with_cambium_trust` — telemetry only (verdict counts feed TrustEngine)
/// - `with_auto_planner_oath` — clean-slate Planner LLM call before each turn
///
/// Default rollout: `Warn` strictness. Verdicts append a footer on FAIL/Inconclusive
/// but the agent's reply is preserved (never destroyed). Promote to `Block` after
/// production telemetry shows acceptable false-positive rate per task class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessConfig {
    /// Master switch. When false, no Witness wiring occurs.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Strictness mode applied to verdicts: "observe" | "warn" | "block" | "block_with_retry".
    /// Default: "warn" (visible footer on FAIL, agent reply preserved).
    #[serde(default = "default_witness_strictness")]
    pub strictness: String,
    /// When true, runs the Planner LLM (clean-slate) before each `process_message`
    /// to seal a Root Oath. Adds ~1 LLM call per turn (~$0.001 on Claude 3.5 Sonnet).
    #[serde(default = "default_true")]
    pub auto_planner_oath: bool,
    /// When true, append a one-line readout (`Witness: 4/5 PASS. Cost: $X. Latency: +Yms.`)
    /// to every reply regardless of strictness. Default false (telemetry-only).
    #[serde(default)]
    pub show_readout: bool,
    /// Maximum LLM cost overhead allowed (% of base agent cost) before degrading
    /// to Tier 0 only. Default 15.0 (matches lab theory's 12-14% target with margin).
    #[serde(default = "default_witness_max_overhead_pct")]
    pub max_overhead_pct: f64,
    /// Enable Tier 1 LLM-backed AspectVerifier. Default true.
    #[serde(default = "default_true")]
    pub tier1_enabled: bool,
    /// Enable Tier 2 LLM-backed AdversarialJudge (advisory only). Default true.
    #[serde(default = "default_true")]
    pub tier2_enabled: bool,
    /// Path to the Witness Ledger SQLite DB. None = ~/.temm1e/witness.db.
    #[serde(default)]
    pub ledger_path: Option<String>,
}

impl Default for WitnessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strictness: default_witness_strictness(),
            auto_planner_oath: true,
            show_readout: false,
            max_overhead_pct: default_witness_max_overhead_pct(),
            tier1_enabled: true,
            tier2_enabled: true,
            ledger_path: None,
        }
    }
}

fn default_witness_strictness() -> String {
    "warn".to_string()
}
fn default_witness_max_overhead_pct() -> f64 {
    15.0
}

/// Perpetuum configuration — perpetual time-aware entity framework.
/// Enabled by default. Set `enabled = false` to opt out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpetualConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_perpetuum_timezone")]
    pub timezone: String,
    #[serde(default = "default_perpetuum_max_concerns")]
    pub max_concerns: usize,
    #[serde(default)]
    pub conscience_idle_threshold_secs: Option<u64>,
    #[serde(default)]
    pub conscience_dream_threshold_secs: Option<u64>,
    #[serde(default = "default_perpetuum_review_every_n")]
    pub review_every_n_checks: u32,
    #[serde(default)]
    pub volition_enabled: bool,
    #[serde(default = "default_perpetuum_volition_interval")]
    pub volition_interval_secs: u64,
    #[serde(default = "default_perpetuum_max_actions")]
    pub volition_max_actions: usize,
}

impl Default for PerpetualConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timezone: default_perpetuum_timezone(),
            max_concerns: default_perpetuum_max_concerns(),
            conscience_idle_threshold_secs: None,
            conscience_dream_threshold_secs: None,
            review_every_n_checks: default_perpetuum_review_every_n(),
            volition_enabled: false,
            volition_interval_secs: default_perpetuum_volition_interval(),
            volition_max_actions: default_perpetuum_max_actions(),
        }
    }
}

fn default_perpetuum_timezone() -> String {
    "UTC".to_string()
}
fn default_perpetuum_max_concerns() -> usize {
    100
}
fn default_perpetuum_review_every_n() -> u32 {
    20
}
fn default_perpetuum_volition_interval() -> u64 {
    900
}
fn default_perpetuum_max_actions() -> usize {
    2
}

/// Tem Conscious configuration — consciousness observer sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsciousnessConfig {
    /// Enable consciousness observation. ON by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum confidence to inject a whisper (0.0-1.0).
    #[serde(default = "default_consciousness_confidence")]
    pub confidence_threshold: f64,
    /// Maximum interventions per session.
    #[serde(default = "default_consciousness_max_interventions")]
    pub max_interventions_per_session: u32,
    /// Observation mode: "rules_first", "always_llm", "rules_only".
    #[serde(default = "default_consciousness_observation_mode")]
    pub observation_mode: String,
}

impl Default for ConsciousnessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            confidence_threshold: default_consciousness_confidence(),
            max_interventions_per_session: default_consciousness_max_interventions(),
            observation_mode: default_consciousness_observation_mode(),
        }
    }
}

/// Bug reporter configuration — self-diagnosing bug reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VigilConfig {
    /// Enable bug reporting. ON by default, but requires consent + GitHub PAT.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Has the user explicitly approved sending reports?
    #[serde(default)]
    pub consent_given: bool,
    /// Auto-report after 60-second window (skip 3-step flow).
    #[serde(default)]
    pub auto_report: bool,
}

impl Default for VigilConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            consent_given: false,
            auto_report: false,
        }
    }
}

fn default_consciousness_confidence() -> f64 {
    0.7
}
fn default_consciousness_max_interventions() -> u32 {
    10
}
fn default_consciousness_observation_mode() -> String {
    "rules_first".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Temm1eSection {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub tenant_isolation: bool,
}

impl Default for Temm1eSection {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
            tenant_isolation: false,
        }
    }
}

fn default_mode() -> String {
    "auto".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            tls: false,
            tls_cert: None,
            tls_key: None,
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Multiple API keys for the same provider. Used for key rotation on rate-limit/auth errors.
    /// Takes precedence over `api_key` when non-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Extra HTTP headers sent with every provider request (e.g. OpenRouter attribution).
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
}

impl ProviderConfig {
    /// Returns all API keys — merges `keys` and `api_key` into a single list.
    /// If `keys` is non-empty, returns `keys`. Otherwise falls back to `api_key`.
    pub fn all_keys(&self) -> Vec<String> {
        if !self.keys.is_empty() {
            self.keys
                .iter()
                .filter(|k| !k.is_empty())
                .cloned()
                .collect()
        } else if let Some(ref key) = self.api_key {
            if key.is_empty() {
                vec![]
            } else {
                vec![key.clone()]
            }
        } else {
            vec![]
        }
    }
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |k: &str| -> String {
            let chars: Vec<char> = k.chars().collect();
            if chars.len() > 8 {
                let prefix: String = chars[..4].iter().collect();
                let suffix: String = chars[chars.len() - 4..].iter().collect();
                format!("{prefix}...{suffix}")
            } else {
                "***".to_string()
            }
        };
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("api_key", &self.api_key.as_ref().map(|k| redact(k)))
            .field(
                "keys",
                &self.keys.iter().map(|k| redact(k)).collect::<Vec<_>>(),
            )
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_backend")]
    pub backend: String,
    pub path: Option<String>,
    pub connection_string: Option<String>,
    #[serde(default)]
    pub search: SearchConfig,
    /// λ-Memory configuration — continuous decay with hash-based recall.
    #[serde(default)]
    pub lambda: LambdaMemoryConfig,
}

/// Active memory strategy — switchable at runtime via `/memory` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStrategy {
    /// λ-Memory: exponential decay, fidelity tiers, hash-based recall, cross-session.
    Lambda,
    /// Echo Memory: keyword search over recent context window. No persistence.
    #[default]
    Echo,
}

impl std::fmt::Display for MemoryStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryStrategy::Lambda => write!(f, "λ-Memory"),
            MemoryStrategy::Echo => write!(f, "Echo Memory"),
        }
    }
}

/// Configuration for λ-Memory — continuous decay with hash-based recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaMemoryConfig {
    /// Whether λ-Memory is enabled (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Decay rate constant (λ). Higher = faster decay (default: 0.01).
    #[serde(default = "default_decay_lambda")]
    pub decay_lambda: f32,
    /// Threshold for full text display (default: 2.0).
    #[serde(default = "default_hot")]
    pub hot_threshold: f32,
    /// Threshold for summary display (default: 1.0).
    #[serde(default = "default_warm")]
    pub warm_threshold: f32,
    /// Threshold for essence display (default: 0.3).
    #[serde(default = "default_cool")]
    pub cool_threshold: f32,
    /// Max memories to score per turn (default: 500).
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: usize,
}

fn default_decay_lambda() -> f32 {
    0.01
}
fn default_hot() -> f32 {
    2.0
}
fn default_warm() -> f32 {
    1.0
}
fn default_cool() -> f32 {
    0.3
}
fn default_candidate_limit() -> usize {
    500
}

impl Default for LambdaMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            decay_lambda: 0.01,
            hot_threshold: 2.0,
            warm_threshold: 1.0,
            cool_threshold: 0.3,
            candidate_limit: 500,
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".to_string(),
            path: None,
            connection_string: None,
            search: SearchConfig::default(),
            lambda: LambdaMemoryConfig::default(),
        }
    }
}

fn default_memory_backend() -> String {
    "sqlite".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_vector_weight")]
    pub vector_weight: f32,
    #[serde(default = "default_keyword_weight")]
    pub keyword_weight: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            vector_weight: 0.7,
            keyword_weight: 0.3,
        }
    }
}

fn default_vector_weight() -> f32 {
    0.7
}
fn default_keyword_weight() -> f32 {
    0.3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    #[serde(default = "default_vault_backend")]
    pub backend: String,
    pub key_file: Option<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            backend: "local-chacha20".to_string(),
            key_file: None,
        }
    }
}

fn default_vault_backend() -> String {
    "local-chacha20".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStoreConfig {
    #[serde(default = "default_filestore_backend")]
    pub backend: String,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub path: Option<String>,
}

impl Default for FileStoreConfig {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            bucket: None,
            region: None,
            endpoint: None,
            path: None,
        }
    }
}

fn default_filestore_backend() -> String {
    "local".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default = "default_sandbox")]
    pub sandbox: String,
    #[serde(default = "default_true")]
    pub file_scanning: bool,
    #[serde(default = "default_skill_signing")]
    pub skill_signing: String,
    #[serde(default = "default_true")]
    pub audit_log: bool,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            sandbox: "mandatory".to_string(),
            file_scanning: true,
            skill_signing: "required".to_string(),
            audit_log: true,
            rate_limit: None,
        }
    }
}

fn default_sandbox() -> String {
    "mandatory".to_string()
}
fn default_skill_signing() -> String {
    "required".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval")]
    pub interval: String,
    #[serde(default = "default_heartbeat_checklist")]
    pub checklist: String,
    /// Chat ID to send heartbeat reports to (e.g. Telegram chat).
    /// If unset, heartbeat responses are only logged.
    #[serde(default)]
    pub report_to: Option<String>,
    /// Active hours window (24h format). Heartbeats only fire within
    /// this range. Example: "08:00-22:00". Unset = always active.
    #[serde(default)]
    pub active_hours: Option<String>,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: "30m".to_string(),
            checklist: "HEARTBEAT.md".to_string(),
            report_to: None,
            active_hours: None,
        }
    }
}

fn default_heartbeat_interval() -> String {
    "30m".to_string()
}
fn default_heartbeat_checklist() -> String {
    "HEARTBEAT.md".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronConfig {
    #[serde(default = "default_cron_storage")]
    pub storage: String,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            storage: "sqlite".to_string(),
        }
    }
}

fn default_cron_storage() -> String {
    "sqlite".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    #[serde(default)]
    pub enabled: bool,
    pub token: Option<String>,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_true")]
    pub file_transfer: bool,
    pub max_file_size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Maximum number of recent message pairs (user+assistant) to keep in context.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// Maximum estimated token count for the entire context window.
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    /// Maximum number of tool-use rounds per message before forcing a text reply.
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: usize,
    /// Maximum wall-clock seconds for a single task before forcing a text reply.
    /// **Default: 0 = unlimited** (the right choice for reasoning-model workloads
    /// and CLI/long-form tasks where cost + turn/tool-round caps are the real
    /// ceilings). Set a positive value only if you want a hard time SLA —
    /// e.g. a messaging-channel bot where users expect replies within N minutes.
    /// Values 1..=9 are rejected; use 0 (disabled) or >= 10.
    #[serde(default = "default_max_task_duration_secs")]
    pub max_task_duration_secs: u64,
    /// Whether to stream incremental text responses to the user (default: true).
    #[serde(default = "default_true")]
    pub streaming_enabled: bool,
    /// Minimum interval (ms) between flushing accumulated streamed tokens (default: 1000).
    #[serde(default = "default_streaming_flush_interval_ms")]
    pub streaming_flush_interval_ms: u64,
    /// Whether to send tool-lifecycle status updates to the user (default: true).
    #[serde(default = "default_true")]
    pub streaming_tool_updates: bool,
    /// Maximum total USD spend allowed per session (0.0 = unlimited).
    #[serde(default = "default_max_spend_usd")]
    pub max_spend_usd: f64,
    /// Enable v2 Tem's Mind optimizations: complexity classification,
    /// prompt stratification, structured failures, trivial fast-path.
    /// Default: true (v2 behavior). Set to false to revert to v1 behavior.
    #[serde(default = "default_v2_optimizations")]
    pub v2_optimizations: bool,
    /// Legacy flag for executable DAG blueprint phase parallelism.
    /// Parallel phase execution has been removed — blueprints are always
    /// injected as context text and the LLM follows them holistically.
    /// This field is retained for config compatibility only.
    #[serde(default)]
    pub parallel_phases: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 200,
            max_context_tokens: 30_000,
            // v5.3.6: 0 = unlimited (matches max_task_duration_secs convention).
            // Stagnation detection + budget + duration are the real ceilings.
            max_tool_rounds: 0,
            max_task_duration_secs: 0, // 0 = unlimited (see doc above)
            streaming_enabled: true,
            streaming_flush_interval_ms: 1000,
            streaming_tool_updates: true,
            max_spend_usd: 0.0,
            v2_optimizations: true,
            parallel_phases: false,
        }
    }
}

fn default_max_turns() -> usize {
    200
}
fn default_max_context_tokens() -> usize {
    30_000
}
fn default_max_tool_rounds() -> usize {
    // v5.3.6: 0 = unlimited. Stagnation detection + budget + duration are
    // the real safety nets; iteration count alone is a proxy, not a limit.
    0
}
fn default_max_task_duration_secs() -> u64 {
    // 0 = unlimited. Reasoning models on long refactors routinely take 10-60+
    // minutes; cost/turn/tool-round caps are the real ceilings. Set a positive
    // value (>= 10) only if you want a hard wall-clock SLA.
    0
}
fn default_streaming_flush_interval_ms() -> u64 {
    1000
}
fn default_max_spend_usd() -> f64 {
    0.0
}

fn default_v2_optimizations() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub shell: bool,
    #[serde(default = "default_true")]
    pub browser: bool,
    #[serde(default = "default_true")]
    pub file: bool,
    #[serde(default = "default_true")]
    pub git: bool,
    #[serde(default = "default_true")]
    pub cron: bool,
    #[serde(default = "default_true")]
    pub http: bool,
    /// Browser idle timeout in seconds. 0 = disabled (persistent browser, V2 default).
    /// If >0, auto-closes after this many seconds of inactivity (legacy behavior).
    #[serde(default = "default_browser_timeout_secs")]
    pub browser_timeout_secs: u64,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            shell: true,
            browser: true,
            file: true,
            git: true,
            cron: true,
            http: true,
            browser_timeout_secs: 0,
        }
    }
}

fn default_browser_timeout_secs() -> u64 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub provider: String,
    pub token: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub otel_enabled: bool,
    pub otel_endpoint: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            otel_enabled: false,
            otel_endpoint: None,
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Tem Gaze configuration — vision-based interaction enhancements.
///
/// Controls browser vision grounding (Prowl V2) and future desktop control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GazeConfig {
    /// Enable Gaze vision enhancements (SoM overlay, zoom-refine, blueprint bypass).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Confidence threshold for direct click without zoom-refine.
    #[serde(default = "default_gaze_high_confidence")]
    pub high_confidence: f64,
    /// Confidence threshold below which SoM fallback is used.
    #[serde(default = "default_gaze_medium_confidence")]
    pub medium_confidence: f64,
    /// Minimum confidence to accept a post-action verification as passing.
    #[serde(default = "default_gaze_verify_threshold")]
    pub verify_threshold: f64,
    /// Maximum retry attempts after verification failure.
    #[serde(default = "default_gaze_max_retries")]
    pub max_retries: u32,
    /// Milliseconds to wait after an action for the UI to settle.
    #[serde(default = "default_gaze_ui_settle_ms")]
    pub ui_settle_ms: u64,
    /// Verification mode: "off", "high_stakes", or "always".
    #[serde(default = "default_gaze_verify_mode")]
    pub verify_mode: String,
    /// Monitor index for desktop control (0 = primary).
    #[serde(default)]
    pub monitor: usize,
    /// Use OS accessibility APIs when available (cost optimizer, off by default).
    #[serde(default)]
    pub use_accessibility: bool,
    /// Browser-specific Gaze settings (Prowl V2).
    #[serde(default)]
    pub browser: GazeBrowserConfig,
}

impl Default for GazeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            high_confidence: default_gaze_high_confidence(),
            medium_confidence: default_gaze_medium_confidence(),
            verify_threshold: default_gaze_verify_threshold(),
            max_retries: default_gaze_max_retries(),
            ui_settle_ms: default_gaze_ui_settle_ms(),
            verify_mode: default_gaze_verify_mode(),
            monitor: 0,
            use_accessibility: false,
            browser: GazeBrowserConfig::default(),
        }
    }
}

/// Browser-specific Gaze configuration (Prowl V2 enhancements).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GazeBrowserConfig {
    /// Enable Set-of-Mark overlays on Tier 3 observations.
    #[serde(default = "default_true")]
    pub som_overlay: bool,
    /// Enable the zoom_region action for detailed region analysis.
    #[serde(default = "default_true")]
    pub zoom_region: bool,
    /// Check Prowl blueprints before vision grounding (0 LLM calls for known flows).
    #[serde(default = "default_true")]
    pub blueprint_bypass: bool,
}

impl Default for GazeBrowserConfig {
    fn default() -> Self {
        Self {
            som_overlay: true,
            zoom_region: true,
            blueprint_bypass: true,
        }
    }
}

fn default_gaze_high_confidence() -> f64 {
    0.85
}
fn default_gaze_medium_confidence() -> f64 {
    0.40
}
fn default_gaze_verify_threshold() -> f64 {
    0.70
}
fn default_gaze_max_retries() -> u32 {
    2
}
fn default_gaze_ui_settle_ms() -> u64 {
    500
}
fn default_gaze_verify_mode() -> String {
    "high_stakes".into()
}

// ---------------------------------------------------------------------------
// Agent-Accessible Config — safe subset the agent can read and modify
// ---------------------------------------------------------------------------

/// Memory settings the agent can adjust (search tuning only).
/// Backend, path, and connection_string are system-only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMemoryConfig {
    #[serde(default)]
    pub search: SearchConfig,
}

/// Observability settings the agent can adjust.
/// otel_enabled and otel_endpoint are system-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentObservabilityConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for AgentObservabilityConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
        }
    }
}

/// Agent-accessible configuration — the safe subset of Temm1eConfig that
/// the agent can read and modify at runtime without breaking system invariants.
///
/// Loaded from `agent-config.toml` and merged onto the master config.
/// System-critical fields (provider API keys, gateway bind address, channel
/// tokens, vault keys, security policy, etc.) are NOT exposed here.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentAccessibleConfig {
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,
    #[serde(default)]
    pub memory: AgentMemoryConfig,
    #[serde(default)]
    pub observability: AgentObservabilityConfig,
}

impl AgentAccessibleConfig {
    /// Extract agent-accessible config from a full Temm1eConfig.
    pub fn from_master(config: &Temm1eConfig) -> Self {
        Self {
            agent: config.agent.clone(),
            tools: config.tools.clone(),
            heartbeat: config.heartbeat.clone(),
            memory: AgentMemoryConfig {
                search: config.memory.search.clone(),
            },
            observability: AgentObservabilityConfig {
                log_level: config.observability.log_level.clone(),
            },
        }
    }

    /// Apply this agent config onto a master config, overriding only
    /// the agent-accessible fields. System-critical fields are untouched.
    pub fn apply_to(&self, config: &mut Temm1eConfig) {
        config.agent = self.agent.clone();
        config.tools = self.tools.clone();
        config.heartbeat = self.heartbeat.clone();
        config.memory.search = self.memory.search.clone();
        config.observability.log_level = self.observability.log_level.clone();
    }

    /// Validate that all values are within acceptable bounds.
    pub fn validate(&self) -> Result<(), Temm1eError> {
        if self.agent.max_turns == 0 {
            return Err(Temm1eError::Config(
                "agent.max_turns must be > 0".to_string(),
            ));
        }
        if self.agent.max_context_tokens < 1000 {
            return Err(Temm1eError::Config(
                "agent.max_context_tokens must be >= 1000".to_string(),
            ));
        }
        // v5.3.6: 0 = unlimited. Stagnation + budget + duration are the
        // real safety nets; a bare iteration cap is a proxy, not a limit.
        // Users who want a finite ceiling can still set a positive value.
        // 0 = unlimited (valid). Positive values < 10 are nonsensical
        // (a 9-second task ceiling will brick basically anything useful).
        if self.agent.max_task_duration_secs != 0 && self.agent.max_task_duration_secs < 10 {
            return Err(Temm1eError::Config(
                "agent.max_task_duration_secs must be 0 (unlimited) or >= 10".to_string(),
            ));
        }

        if self.memory.search.vector_weight < 0.0 || self.memory.search.keyword_weight < 0.0 {
            return Err(Temm1eError::Config(
                "memory.search weights must be non-negative".to_string(),
            ));
        }
        let total = self.memory.search.vector_weight + self.memory.search.keyword_weight;
        if total <= 0.0 {
            return Err(Temm1eError::Config(
                "memory.search weights must sum to > 0".to_string(),
            ));
        }

        if self.tools.browser_timeout_secs > 86400 {
            return Err(Temm1eError::Config(
                "tools.browser_timeout_secs must be <= 86400 (24 hours)".to_string(),
            ));
        }

        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.observability.log_level.to_lowercase().as_str()) {
            return Err(Temm1eError::Config(format!(
                "observability.log_level must be one of: {}",
                valid_levels.join(", ")
            )));
        }

        Ok(())
    }

    /// Save this agent config to the given path as TOML.
    pub fn save(&self, path: &Path) -> Result<(), Temm1eError> {
        self.validate()?;

        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| Temm1eError::Config(format!("Failed to serialize agent config: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Temm1eError::Config(format!("Failed to create config directory: {e}"))
            })?;
        }

        std::fs::write(path, toml_str)
            .map_err(|e| Temm1eError::Config(format!("Failed to write agent config: {e}")))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serde_roundtrip() {
        let config = Temm1eConfig {
            mode: Temm1eMode::Play,
            temm1e: Temm1eSection {
                mode: "cloud".to_string(),
                tenant_isolation: true,
            },
            gateway: GatewayConfig {
                host: "0.0.0.0".to_string(),
                port: 443,
                tls: true,
                tls_cert: Some("cert.pem".to_string()),
                tls_key: Some("key.pem".to_string()),
            },
            provider: ProviderConfig {
                name: Some("anthropic".to_string()),
                api_key: Some("sk-test".to_string()),
                keys: vec![],
                model: Some("claude-sonnet-4-6".to_string()),
                base_url: None,
                extra_headers: HashMap::new(),
            },
            memory: MemoryConfig::default(),
            vault: VaultConfig::default(),
            filestore: FileStoreConfig::default(),
            security: SecurityConfig::default(),
            heartbeat: HeartbeatConfig {
                enabled: true,
                ..Default::default()
            },
            cron: CronConfig::default(),
            channel: HashMap::new(),
            agent: AgentConfig::default(),
            tools: ToolsConfig::default(),
            tunnel: None,
            observability: ObservabilityConfig::default(),
            gaze: GazeConfig::default(),
            consciousness: ConsciousnessConfig::default(),
            perpetuum: PerpetualConfig::default(),
            vigil: VigilConfig::default(),
            social: SocialConfig::default(),
            cambium: CambiumConfig::default(),
            witness: WitnessConfig::default(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        let restored: Temm1eConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.temm1e.mode, "cloud");
        assert!(restored.temm1e.tenant_isolation);
        assert_eq!(restored.gateway.port, 443);
        assert!(restored.gateway.tls);
        assert_eq!(restored.provider.name.as_deref(), Some("anthropic"));
    }

    #[test]
    fn defaults_are_sensible() {
        let gw = GatewayConfig::default();
        assert_eq!(gw.host, "127.0.0.1");
        assert_eq!(gw.port, 8080);
        assert!(!gw.tls);

        let mem = MemoryConfig::default();
        assert_eq!(mem.backend, "sqlite");

        let sec = SecurityConfig::default();
        assert_eq!(sec.sandbox, "mandatory");
        assert!(sec.file_scanning);
        assert!(sec.audit_log);

        let tools = ToolsConfig::default();
        assert!(tools.shell);
        assert!(tools.browser);
        assert!(tools.file);

        let agent = AgentConfig::default();
        assert_eq!(agent.max_turns, 200);
        // v5.3.6: max_tool_rounds default 0 = unlimited.
        assert_eq!(agent.max_tool_rounds, 0);
        // v5.3.1: default is 0 (unlimited) — reasoning models on long tasks
        // need the cost/turn/tool-round caps as the real ceilings, not
        // wall-clock. Users opt in to a wall-clock SLA explicitly.
        assert_eq!(agent.max_task_duration_secs, 0);
    }

    // ── Agent-Accessible Config tests ─────────────────────────────────

    #[test]
    fn agent_config_extract_from_master() {
        let master = Temm1eConfig {
            agent: AgentConfig {
                max_turns: 50,
                max_context_tokens: 20_000,
                max_tool_rounds: 100,
                max_task_duration_secs: 600,
                ..Default::default()
            },
            tools: ToolsConfig {
                shell: true,
                browser: false,
                file: true,
                git: false,
                cron: false,
                http: true,
                browser_timeout_secs: 300,
            },
            memory: MemoryConfig {
                backend: "sqlite".to_string(),
                path: Some("/data/memory.db".to_string()),
                connection_string: None,
                search: SearchConfig {
                    vector_weight: 0.8,
                    keyword_weight: 0.2,
                },
                lambda: LambdaMemoryConfig::default(),
            },
            observability: ObservabilityConfig {
                log_level: "debug".to_string(),
                otel_enabled: true,
                otel_endpoint: Some("http://otel:4317".to_string()),
            },
            ..Default::default()
        };

        let agent_cfg = AgentAccessibleConfig::from_master(&master);
        assert_eq!(agent_cfg.agent.max_turns, 50);
        assert!(!agent_cfg.tools.browser);
        assert_eq!(agent_cfg.memory.search.vector_weight, 0.8);
        assert_eq!(agent_cfg.observability.log_level, "debug");
    }

    #[test]
    fn agent_config_apply_to_master() {
        let mut master = Temm1eConfig::default();
        assert_eq!(master.agent.max_turns, 200);
        assert_eq!(master.observability.log_level, "info");

        let agent_cfg = AgentAccessibleConfig {
            agent: AgentConfig {
                max_turns: 50,
                max_context_tokens: 15_000,
                max_tool_rounds: 30,
                max_task_duration_secs: 300,
                ..Default::default()
            },
            tools: ToolsConfig {
                shell: false,
                browser: false,
                file: true,
                git: true,
                cron: false,
                http: false,
                browser_timeout_secs: 300,
            },
            heartbeat: HeartbeatConfig {
                enabled: true,
                interval: "10m".to_string(),
                ..Default::default()
            },
            memory: AgentMemoryConfig {
                search: SearchConfig {
                    vector_weight: 0.5,
                    keyword_weight: 0.5,
                },
            },
            observability: AgentObservabilityConfig {
                log_level: "warn".to_string(),
            },
        };

        agent_cfg.apply_to(&mut master);

        // Agent-accessible fields changed
        assert_eq!(master.agent.max_turns, 50);
        assert!(!master.tools.shell);
        assert!(master.heartbeat.enabled);
        assert_eq!(master.memory.search.vector_weight, 0.5);
        assert_eq!(master.observability.log_level, "warn");

        // System fields untouched
        assert_eq!(master.gateway.port, 8080);
        assert_eq!(master.security.sandbox, "mandatory");
        assert_eq!(master.memory.backend, "sqlite");
        assert!(!master.observability.otel_enabled);
    }

    #[test]
    fn agent_config_roundtrip_preserves_system_fields() {
        let master = Temm1eConfig {
            provider: ProviderConfig {
                api_key: Some("sk-secret".to_string()),
                ..Default::default()
            },
            gateway: GatewayConfig {
                port: 9999,
                ..Default::default()
            },
            security: SecurityConfig {
                sandbox: "strict".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let agent_cfg = AgentAccessibleConfig::from_master(&master);
        let mut restored = master.clone();
        agent_cfg.apply_to(&mut restored);

        // System fields preserved exactly
        assert_eq!(restored.provider.api_key.as_deref(), Some("sk-secret"));
        assert_eq!(restored.gateway.port, 9999);
        assert_eq!(restored.security.sandbox, "strict");
    }

    #[test]
    fn agent_config_validate_ok() {
        let cfg = AgentAccessibleConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn agent_config_validate_zero_turns() {
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_turns = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_validate_low_context_tokens() {
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_context_tokens = 500;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_validate_low_task_duration() {
        // 5 is in the nonsensical 1..=9 range — validator must reject.
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_task_duration_secs = 5;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_validate_zero_task_duration_is_unlimited() {
        // v5.3.1: 0 is the "unlimited" sentinel, must validate successfully.
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_task_duration_secs = 0;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn agent_config_validate_positive_task_duration_ten_or_more_ok() {
        // Any positive value >= 10 validates as a hard wall-clock SLA.
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_task_duration_secs = 10;
        assert!(cfg.validate().is_ok());
        cfg.agent.max_task_duration_secs = 3600;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn agent_config_validate_negative_weights() {
        let mut cfg = AgentAccessibleConfig::default();
        cfg.memory.search.vector_weight = -0.1;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_validate_zero_weights() {
        let mut cfg = AgentAccessibleConfig::default();
        cfg.memory.search.vector_weight = 0.0;
        cfg.memory.search.keyword_weight = 0.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_validate_bad_log_level() {
        let mut cfg = AgentAccessibleConfig::default();
        cfg.observability.log_level = "verbose".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn agent_config_save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-config.toml");

        let cfg = AgentAccessibleConfig {
            agent: AgentConfig {
                max_turns: 75,
                max_context_tokens: 25_000,
                max_tool_rounds: 50,
                max_task_duration_secs: 900,
                ..Default::default()
            },
            observability: AgentObservabilityConfig {
                log_level: "debug".to_string(),
            },
            ..Default::default()
        };

        cfg.save(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let restored: AgentAccessibleConfig = toml::from_str(&content).unwrap();
        assert_eq!(restored.agent.max_turns, 75);
        assert_eq!(restored.observability.log_level, "debug");
    }

    #[test]
    fn agent_config_save_rejects_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-config.toml");

        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_turns = 0;
        assert!(cfg.save(&path).is_err());
        assert!(!path.exists());
    }

    // ── Browser timeout config tests ─────────────────────────────────

    #[test]
    fn browser_timeout_secs_default_is_zero_persistent() {
        let tools = ToolsConfig::default();
        assert_eq!(tools.browser_timeout_secs, 0);
    }

    #[test]
    fn browser_timeout_secs_deserialize_default() {
        // When browser_timeout_secs is not specified, it should default to 0 (persistent)
        let toml_str = r#"
            shell = true
            browser = true
            file = true
            git = true
            cron = true
            http = true
        "#;
        let config: ToolsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.browser_timeout_secs, 0);
    }

    #[test]
    fn browser_timeout_secs_deserialize_custom() {
        let toml_str = r#"
            shell = true
            browser = true
            file = true
            git = true
            cron = true
            http = true
            browser_timeout_secs = 600
        "#;
        let config: ToolsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.browser_timeout_secs, 600);
    }

    #[test]
    fn browser_timeout_secs_serialize_roundtrip() {
        let tools = ToolsConfig {
            browser_timeout_secs: 120,
            ..Default::default()
        };
        let toml_str = toml::to_string(&tools).unwrap();
        let restored: ToolsConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.browser_timeout_secs, 120);
    }

    #[test]
    fn browser_timeout_secs_in_full_config() {
        let toml_str = r#"
            [tools]
            shell = true
            browser = true
            file = true
            git = true
            cron = true
            http = true
            browser_timeout_secs = 900
        "#;
        let config: Temm1eConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tools.browser_timeout_secs, 900);
    }

    #[test]
    fn browser_timeout_secs_default_in_full_config() {
        // Full config with no tools section should default to 0 (persistent browser)
        let config = Temm1eConfig::default();
        assert_eq!(config.tools.browser_timeout_secs, 0);
    }

    // ── Cambium Config tests ──────────────────────────────────────────

    #[test]
    fn cambium_config_default_is_enabled() {
        let config = CambiumConfig::default();
        assert!(config.enabled, "Cambium should be enabled by default");
        assert_eq!(config.max_lines_per_session, 500);
        assert_eq!(config.max_files_per_session, 5);
        assert_eq!(config.max_sessions_per_day, 3);
        assert_eq!(config.cooldown_secs, 3600);
        assert_eq!(config.failure_cooldown_secs, 86400);
        assert!(config.trust_level_override.is_none());
        assert_eq!(config.self_model_path, "docs/lab/cambium");
    }

    #[test]
    fn config_without_cambium_section_defaults_to_enabled() {
        let toml_str = r#"
            [gateway]
            port = 8080
        "#;
        let config: Temm1eConfig = toml::from_str(toml_str).unwrap();
        // No [cambium] section -> defaults apply -> enabled = true
        assert!(config.cambium.enabled);
        assert_eq!(config.cambium.max_lines_per_session, 500);
    }

    #[test]
    fn config_can_explicitly_disable_cambium() {
        let toml_str = r#"
            [cambium]
            enabled = false
        "#;
        let config: Temm1eConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.cambium.enabled);
    }

    #[test]
    fn config_with_cambium_section_parses() {
        let toml_str = r#"
            [cambium]
            enabled = true
            max_lines_per_session = 200
        "#;
        let config: Temm1eConfig = toml::from_str(toml_str).unwrap();
        assert!(config.cambium.enabled);
        assert_eq!(config.cambium.max_lines_per_session, 200);
        assert_eq!(config.cambium.max_sessions_per_day, 3);
    }

    #[test]
    fn cambium_config_serde_roundtrip() {
        let config = CambiumConfig {
            enabled: true,
            max_lines_per_session: 300,
            max_files_per_session: 10,
            max_sessions_per_day: 5,
            cooldown_secs: 1800,
            failure_cooldown_secs: 43200,
            trust_level_override: Some("approval_required".to_string()),
            self_model_path: "docs/cambium".to_string(),
            auto_deploy: false,
            vigil_bridge_enabled: true,
            wish_detection_enabled: true,
        };
        let toml_str = toml::to_string(&config).unwrap();
        let restored: CambiumConfig = toml::from_str(&toml_str).unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.max_lines_per_session, 300);
        assert_eq!(
            restored.trust_level_override.as_deref(),
            Some("approval_required")
        );
    }

    #[test]
    fn agent_config_serde_roundtrip() {
        let cfg = AgentAccessibleConfig {
            agent: AgentConfig {
                max_turns: 100,
                max_context_tokens: 50_000,
                max_tool_rounds: 150,
                max_task_duration_secs: 3600,
                ..Default::default()
            },
            tools: ToolsConfig {
                shell: true,
                browser: false,
                file: true,
                git: true,
                cron: false,
                http: true,
                browser_timeout_secs: 300,
            },
            heartbeat: HeartbeatConfig {
                enabled: true,
                interval: "15m".to_string(),
                checklist: "HEARTBEAT.md".to_string(),
                report_to: Some("chat-123".to_string()),
                active_hours: Some("09:00-18:00".to_string()),
            },
            memory: AgentMemoryConfig {
                search: SearchConfig {
                    vector_weight: 0.6,
                    keyword_weight: 0.4,
                },
            },
            observability: AgentObservabilityConfig {
                log_level: "trace".to_string(),
            },
        };

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let restored: AgentAccessibleConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.agent.max_turns, 100);
        assert!(!restored.tools.browser);
        assert!(restored.heartbeat.enabled);
        assert_eq!(restored.memory.search.keyword_weight, 0.4);
        assert_eq!(restored.observability.log_level, "trace");
    }
}
