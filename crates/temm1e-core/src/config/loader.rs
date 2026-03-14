use crate::types::config::{AgentAccessibleConfig, Temm1eConfig};
use crate::types::error::Temm1eError;
use std::path::{Path, PathBuf};

/// Discover config file locations in priority order
fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System config
    paths.push(PathBuf::from("/etc/temm1e/config.toml"));

    // 2. User config
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".temm1e").join("config.toml"));
    }

    // 3. Workspace config
    paths.push(PathBuf::from("config.toml"));
    paths.push(PathBuf::from("temm1e.toml"));

    paths
}

/// Load configuration from discovered config files, merging in order
pub fn load_config(explicit_path: Option<&Path>) -> Result<Temm1eConfig, Temm1eError> {
    let mut config_content = String::new();

    if let Some(path) = explicit_path {
        config_content = std::fs::read_to_string(path).map_err(|e| {
            Temm1eError::Config(format!("Failed to read {}: {}", path.display(), e))
        })?;
    } else {
        for path in config_paths() {
            if path.exists() {
                config_content = std::fs::read_to_string(&path).map_err(|e| {
                    Temm1eError::Config(format!("Failed to read {}: {}", path.display(), e))
                })?;
                break;
            }
        }
    }

    if config_content.is_empty() {
        return Ok(Temm1eConfig::default());
    }

    // Expand environment variables
    let expanded = super::env::expand_env_vars(&config_content);

    // Try TOML first (native format + ZeroClaw compat)
    if let Ok(config) = toml::from_str::<Temm1eConfig>(&expanded) {
        return Ok(config);
    }

    // Try YAML (OpenClaw compat)
    if let Ok(config) = serde_yaml::from_str::<Temm1eConfig>(&expanded) {
        return Ok(config);
    }

    Err(Temm1eError::Config(
        "Failed to parse config as TOML or YAML".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Agent-Accessible Config
// ---------------------------------------------------------------------------

/// Discover agent config file locations in priority order
fn agent_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. User agent config
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".temm1e").join("agent-config.toml"));
    }

    // 2. Workspace agent config
    paths.push(PathBuf::from("agent-config.toml"));

    paths
}

/// Returns the default agent config file path (`~/.temm1e/agent-config.toml`)
pub fn default_agent_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".temm1e").join("agent-config.toml"))
}

/// Load agent-accessible config from discovered agent config files.
/// Returns `None` if no agent config file is found.
pub fn load_agent_config(
    explicit_path: Option<&Path>,
) -> Result<Option<AgentAccessibleConfig>, Temm1eError> {
    let mut config_content = String::new();

    if let Some(path) = explicit_path {
        if path.exists() {
            config_content = std::fs::read_to_string(path).map_err(|e| {
                Temm1eError::Config(format!("Failed to read {}: {}", path.display(), e))
            })?;
        } else {
            return Ok(None);
        }
    } else {
        for path in agent_config_paths() {
            if path.exists() {
                config_content = std::fs::read_to_string(&path).map_err(|e| {
                    Temm1eError::Config(format!("Failed to read {}: {}", path.display(), e))
                })?;
                break;
            }
        }
    }

    if config_content.is_empty() {
        return Ok(None);
    }

    let expanded = super::env::expand_env_vars(&config_content);

    let agent_config: AgentAccessibleConfig = toml::from_str(&expanded)
        .map_err(|e| Temm1eError::Config(format!("Failed to parse agent config: {e}")))?;

    agent_config.validate()?;

    Ok(Some(agent_config))
}

/// Load full config with agent config overlay merged in.
/// Agent-accessible fields from the agent config override the master config.
pub fn load_config_with_agent_overlay(
    config_path: Option<&Path>,
    agent_config_path: Option<&Path>,
) -> Result<Temm1eConfig, Temm1eError> {
    let mut config = load_config(config_path)?;

    if let Some(agent_config) = load_agent_config(agent_config_path)? {
        agent_config.apply_to(&mut config);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_no_file() {
        // When no config file exists, load_config should return defaults
        let config = load_config(None).unwrap();
        assert_eq!(config.gateway.host, "127.0.0.1");
        assert_eq!(config.gateway.port, 8080);
        assert_eq!(config.memory.backend, "sqlite");
        assert_eq!(config.vault.backend, "local-chacha20");
        assert_eq!(config.security.sandbox, "mandatory");
        assert!(config.channel.is_empty());
    }

    #[test]
    fn test_load_toml_config() {
        let toml_content = r#"
[gateway]
host = "0.0.0.0"
port = 9090
tls = true

[provider]
name = "anthropic"
api_key = "test-key-123"
model = "claude-sonnet-4-6"

[memory]
backend = "markdown"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.host, "0.0.0.0");
        assert_eq!(config.gateway.port, 9090);
        assert!(config.gateway.tls);
        assert_eq!(config.provider.name.as_deref(), Some("anthropic"));
        assert_eq!(config.provider.api_key.as_deref(), Some("test-key-123"));
        assert_eq!(config.memory.backend, "markdown");
    }

    #[test]
    fn test_load_yaml_config() {
        let yaml_content = r#"
gateway:
  host: "10.0.0.1"
  port: 3000
memory:
  backend: "sqlite"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.host, "10.0.0.1");
        assert_eq!(config.gateway.port, 3000);
        assert_eq!(config.memory.backend, "sqlite");
    }

    #[test]
    fn test_env_var_expansion_in_config() {
        std::env::set_var("TEMM1E_TEST_API_KEY", "expanded-key-value");
        let toml_content = r#"
[provider]
name = "anthropic"
api_key = "${TEMM1E_TEST_API_KEY}"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(
            config.provider.api_key.as_deref(),
            Some("expanded-key-value")
        );
        std::env::remove_var("TEMM1E_TEST_API_KEY");
    }

    #[test]
    fn test_invalid_config_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "this is not valid TOML {{ or YAML").unwrap();

        let result = load_config(Some(tmp.path()));
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_config_file() {
        let result = load_config(Some(std::path::Path::new(
            "/tmp/nonexistent_temm1e_config_12345.toml",
        )));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_with_channels() {
        let toml_content = r#"
[channel.telegram]
enabled = true
token = "bot123"
allowlist = ["user1", "@user2"]
file_transfer = true
max_file_size = "50MB"

[channel.discord]
enabled = false
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.channel.len(), 2);
        let tg = &config.channel["telegram"];
        assert!(tg.enabled);
        assert_eq!(tg.token.as_deref(), Some("bot123"));
        assert_eq!(tg.allowlist, vec!["user1", "@user2"]);
        assert!(tg.file_transfer);

        let dc = &config.channel["discord"];
        assert!(!dc.enabled);
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[test]
    fn test_empty_config_file_returns_defaults() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        // Empty file should produce default config
        assert_eq!(config.gateway.host, "127.0.0.1");
        assert_eq!(config.gateway.port, 8080);
    }

    #[test]
    fn test_config_with_security_settings() {
        let toml_content = r#"
[security]
sandbox = "permissive"
file_scanning = false
skill_signing = "optional"
audit_log = false

[security.rate_limit]
requests_per_minute = 100
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.security.sandbox, "permissive");
        assert!(!config.security.file_scanning);
        assert_eq!(config.security.skill_signing, "optional");
        assert!(!config.security.audit_log);
        assert!(config.security.rate_limit.is_some());
        assert_eq!(config.security.rate_limit.unwrap().requests_per_minute, 100);
    }

    #[test]
    fn test_config_partial_overrides_keep_defaults() {
        let toml_content = r#"
[gateway]
port = 3000
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.port, 3000);
        // Host should still be default
        assert_eq!(config.gateway.host, "127.0.0.1");
        // Other sections should be defaults
        assert_eq!(config.memory.backend, "sqlite");
        assert_eq!(config.vault.backend, "local-chacha20");
    }

    #[test]
    fn test_env_var_expansion_missing_var() {
        let toml_content = r#"
[provider]
name = "anthropic"
api_key = "${NONEXISTENT_TEMM1E_VAR_99999}"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        // Missing env var expands to empty string
        assert_eq!(config.provider.api_key.as_deref(), Some(""));
    }

    #[test]
    fn test_config_with_tunnel() {
        let toml_content = r#"
[tunnel]
provider = "cloudflare"
token = "cf-token-123"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert!(config.tunnel.is_some());
        let tunnel = config.tunnel.unwrap();
        assert_eq!(tunnel.provider, "cloudflare");
        assert_eq!(tunnel.token.as_deref(), Some("cf-token-123"));
    }

    #[test]
    fn test_config_observability() {
        let toml_content = r#"
[observability]
log_level = "debug"
otel_enabled = true
otel_endpoint = "http://localhost:4317"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.observability.log_level, "debug");
        assert!(config.observability.otel_enabled);
        assert_eq!(
            config.observability.otel_endpoint.as_deref(),
            Some("http://localhost:4317")
        );
    }

    #[test]
    #[ignore] // Performance test
    fn test_config_parsing_performance() {
        let toml_content = r#"
[gateway]
host = "0.0.0.0"
port = 8080

[provider]
name = "anthropic"
api_key = "sk-test"

[memory]
backend = "sqlite"

[security]
sandbox = "mandatory"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = load_config(Some(tmp.path())).unwrap();
        }
        let elapsed = start.elapsed();
        let per_parse = elapsed / 100;
        assert!(
            per_parse.as_millis() < 10,
            "Config parse took {}ms, expected <10ms",
            per_parse.as_millis()
        );
    }

    // ── Agent config loader tests ─────────────────────────────────────

    #[test]
    fn test_load_agent_config_none_when_missing() {
        let result = load_agent_config(Some(std::path::Path::new(
            "/tmp/nonexistent_agent_config_99999.toml",
        )));
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_load_agent_config_valid() {
        let content = r#"
[agent]
max_turns = 50
max_context_tokens = 20000
max_tool_rounds = 30
max_task_duration_secs = 600

[tools]
shell = true
browser = false
file = true
git = true
cron = false
http = true

[heartbeat]
enabled = true
interval = "10m"

[memory.search]
vector_weight = 0.6
keyword_weight = 0.4

[observability]
log_level = "debug"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();

        let agent_cfg = load_agent_config(Some(tmp.path())).unwrap().unwrap();
        assert_eq!(agent_cfg.agent.max_turns, 50);
        assert!(!agent_cfg.tools.browser);
        assert!(agent_cfg.heartbeat.enabled);
        assert_eq!(agent_cfg.memory.search.vector_weight, 0.6);
        assert_eq!(agent_cfg.observability.log_level, "debug");
    }

    #[test]
    fn test_load_agent_config_rejects_invalid() {
        let content = r#"
[agent]
max_turns = 0
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), content).unwrap();

        let result = load_agent_config(Some(tmp.path()));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_with_agent_overlay() {
        // Master config
        let master_content = r#"
[gateway]
host = "0.0.0.0"
port = 9090

[provider]
name = "anthropic"
api_key = "sk-secret-key"

[agent]
max_turns = 200
max_context_tokens = 30000

[observability]
log_level = "info"
otel_enabled = true
otel_endpoint = "http://otel:4317"
"#;
        let master_tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(master_tmp.path(), master_content).unwrap();

        // Agent config overrides
        let agent_content = r#"
[agent]
max_turns = 50
max_context_tokens = 15000

[observability]
log_level = "debug"
"#;
        let agent_tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(agent_tmp.path(), agent_content).unwrap();

        let config =
            load_config_with_agent_overlay(Some(master_tmp.path()), Some(agent_tmp.path()))
                .unwrap();

        // Agent-accessible fields overridden
        assert_eq!(config.agent.max_turns, 50);
        assert_eq!(config.agent.max_context_tokens, 15_000);
        assert_eq!(config.observability.log_level, "debug");

        // System fields preserved from master
        assert_eq!(config.gateway.host, "0.0.0.0");
        assert_eq!(config.gateway.port, 9090);
        assert_eq!(config.provider.api_key.as_deref(), Some("sk-secret-key"));
        assert!(config.observability.otel_enabled);
        assert_eq!(
            config.observability.otel_endpoint.as_deref(),
            Some("http://otel:4317")
        );
    }

    #[test]
    fn test_load_config_with_no_agent_overlay() {
        let master_content = r#"
[agent]
max_turns = 200
"#;
        let master_tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(master_tmp.path(), master_content).unwrap();

        // Agent config path doesn't exist
        let config = load_config_with_agent_overlay(
            Some(master_tmp.path()),
            Some(std::path::Path::new("/tmp/no_such_agent_config.toml")),
        )
        .unwrap();

        // Master values unchanged
        assert_eq!(config.agent.max_turns, 200);
    }

    #[test]
    fn test_default_agent_config_path() {
        let path = default_agent_config_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.to_string_lossy().contains(".temm1e"));
        assert!(p.to_string_lossy().ends_with("agent-config.toml"));
    }
}
