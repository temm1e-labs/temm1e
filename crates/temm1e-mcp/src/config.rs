//! MCP configuration — loading and saving `~/.temm1e/mcp.toml`.
//!
//! Users configure MCP servers here. The file is created on first `/mcp add`
//! and loaded at startup.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Top-level MCP configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub settings: McpSettings,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

/// Global MCP settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSettings {
    /// Maximum number of MCP servers allowed (default: 10).
    #[serde(default = "default_max_servers")]
    pub max_servers: usize,
    /// Timeout in seconds for JSON-RPC calls (default: 30).
    #[serde(default = "default_call_timeout")]
    pub call_timeout_secs: u64,
    /// Auto-restart crashed stdio servers (default: true).
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    /// Maximum restart attempts before giving up (default: 3).
    #[serde(default = "default_max_restarts")]
    pub max_restart_attempts: u32,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            max_servers: default_max_servers(),
            call_timeout_secs: default_call_timeout(),
            auto_restart: true,
            max_restart_attempts: default_max_restarts(),
        }
    }
}

fn default_max_servers() -> usize {
    10
}
fn default_call_timeout() -> u64 {
    30
}
fn default_true() -> bool {
    true
}
fn default_max_restarts() -> u32 {
    3
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name for this server (e.g., "sms", "documents").
    pub name: String,

    /// Transport type: "stdio" (default) or "http".
    #[serde(default = "default_transport")]
    pub transport: String,

    // ── stdio transport fields ──
    /// Command to execute (e.g., "npx", "python", "/usr/local/bin/my-mcp").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Arguments for the command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables to pass to the subprocess.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    // ── HTTP transport fields ──
    /// URL for HTTP transport (e.g., "https://my-mcp-server.example.com/mcp").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Extra headers for HTTP requests.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

impl McpServerConfig {
    /// Create a new stdio MCP server config.
    pub fn stdio(name: &str, command: &str, args: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some(command.to_string()),
            args,
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
        }
    }

    /// Create a new HTTP MCP server config.
    pub fn http(name: &str, url: &str) -> Self {
        Self {
            name: name.to_string(),
            transport: "http".to_string(),
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some(url.to_string()),
            headers: HashMap::new(),
        }
    }

    /// Validate that required fields are present for the transport type.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("Server name cannot be empty".to_string());
        }
        if self.name.contains(char::is_whitespace) {
            return Err("Server name cannot contain whitespace".to_string());
        }
        match self.transport.as_str() {
            "stdio" => {
                if self.command.is_none() || self.command.as_deref() == Some("") {
                    return Err(format!(
                        "Server '{}': stdio transport requires a command",
                        self.name
                    ));
                }
            }
            "http" => {
                if self.url.is_none() || self.url.as_deref() == Some("") {
                    return Err(format!(
                        "Server '{}': http transport requires a url",
                        self.name
                    ));
                }
            }
            other => {
                return Err(format!(
                    "Server '{}': unknown transport '{}'",
                    self.name, other
                ));
            }
        }
        Ok(())
    }
}

/// Path to `~/.temm1e/mcp.toml`.
pub fn mcp_config_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".temm1e").join("mcp.toml")
}

/// Load MCP config from `~/.temm1e/mcp.toml`.
/// Returns default (empty) config if file doesn't exist.
pub fn load_mcp_config() -> McpConfig {
    let path = mcp_config_path();
    if !path.exists() {
        debug!("No mcp.toml found at {:?} — using defaults", path);
        return McpConfig::default();
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => match toml::from_str::<McpConfig>(&contents) {
            Ok(config) => {
                info!(
                    servers = config.servers.len(),
                    "Loaded MCP config from {:?}", path
                );
                config
            }
            Err(e) => {
                warn!(error = %e, path = ?path, "Failed to parse mcp.toml — using defaults");
                McpConfig::default()
            }
        },
        Err(e) => {
            warn!(error = %e, path = ?path, "Failed to read mcp.toml — using defaults");
            McpConfig::default()
        }
    }
}

/// Save MCP config to `~/.temm1e/mcp.toml`.
pub fn save_mcp_config(config: &McpConfig) -> Result<(), String> {
    let path = mcp_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {:?}: {}", parent, e))?;
    }
    let contents =
        toml::to_string_pretty(config).map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(&path, contents).map_err(|e| format!("Failed to write {:?}: {}", path, e))?;
    info!(path = ?path, servers = config.servers.len(), "Saved MCP config");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_empty() {
        let config = McpConfig::default();
        assert!(config.servers.is_empty());
        assert_eq!(config.settings.max_servers, 10);
        assert_eq!(config.settings.call_timeout_secs, 30);
    }

    #[test]
    fn stdio_config_validates() {
        let config = McpServerConfig::stdio("test", "npx", vec!["-y".into(), "server".into()]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn http_config_validates() {
        let config = McpServerConfig::http("test", "https://example.com/mcp");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn empty_name_fails_validation() {
        let config = McpServerConfig::stdio("", "npx", vec![]);
        assert!(config.validate().is_err());
    }

    #[test]
    fn name_with_spaces_fails_validation() {
        let config = McpServerConfig::stdio("my server", "npx", vec![]);
        assert!(config.validate().is_err());
    }

    #[test]
    fn stdio_without_command_fails() {
        let mut config = McpServerConfig::stdio("test", "npx", vec![]);
        config.command = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn http_without_url_fails() {
        let mut config = McpServerConfig::http("test", "https://example.com/mcp");
        config.url = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_transport_fails() {
        let mut config = McpServerConfig::stdio("test", "npx", vec![]);
        config.transport = "grpc".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn roundtrip_serialize_deserialize() {
        let config = McpConfig {
            settings: McpSettings::default(),
            servers: vec![
                McpServerConfig::stdio("docs", "npx", vec!["-y".into(), "@company/docs".into()]),
                McpServerConfig::http("search", "https://search.dev/mcp"),
            ],
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let restored: McpConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.servers.len(), 2);
        assert_eq!(restored.servers[0].name, "docs");
        assert_eq!(restored.servers[1].transport, "http");
    }

    #[test]
    fn env_vars_roundtrip() {
        let mut config = McpServerConfig::stdio("sms", "npx", vec![]);
        config.env.insert("API_KEY".into(), "secret123".into());
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let restored: McpServerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(restored.env.get("API_KEY").unwrap(), "secret123");
    }
}
