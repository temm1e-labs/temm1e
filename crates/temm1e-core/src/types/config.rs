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
            self.keys.clone()
        } else if let Some(ref key) = self.api_key {
            vec![key.clone()]
        } else {
            vec![]
        }
    }
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |k: &str| -> String {
            if k.len() > 8 {
                format!("{}...{}", &k[..4], &k[k.len() - 4..])
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
            max_tool_rounds: 200,
            max_task_duration_secs: 1800,
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
    200
}
fn default_max_task_duration_secs() -> u64 {
    1800
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
    /// Browser idle timeout in seconds before auto-close (default: 300).
    /// Increased from 120s to support authenticated sessions that take longer.
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
            browser_timeout_secs: 300,
        }
    }
}

fn default_browser_timeout_secs() -> u64 {
    300
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
        if self.agent.max_tool_rounds == 0 {
            return Err(Temm1eError::Config(
                "agent.max_tool_rounds must be > 0".to_string(),
            ));
        }
        if self.agent.max_task_duration_secs < 10 {
            return Err(Temm1eError::Config(
                "agent.max_task_duration_secs must be >= 10".to_string(),
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
        assert_eq!(agent.max_tool_rounds, 200);
        assert_eq!(agent.max_task_duration_secs, 1800);
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
        let mut cfg = AgentAccessibleConfig::default();
        cfg.agent.max_task_duration_secs = 5;
        assert!(cfg.validate().is_err());
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
    fn browser_timeout_secs_default_is_300() {
        let tools = ToolsConfig::default();
        assert_eq!(tools.browser_timeout_secs, 300);
    }

    #[test]
    fn browser_timeout_secs_deserialize_default() {
        // When browser_timeout_secs is not specified, it should default to 300
        let toml_str = r#"
            shell = true
            browser = true
            file = true
            git = true
            cron = true
            http = true
        "#;
        let config: ToolsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.browser_timeout_secs, 300);
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
        // Full config with no tools section should default to 300
        let config = Temm1eConfig::default();
        assert_eq!(config.tools.browser_timeout_secs, 300);
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
