//! McpManager — orchestrates MCP server lifecycle, tool discovery, and hot-loading.
//!
//! Resilience guarantees:
//! - Dead servers detected via transport `is_alive()`
//! - Auto-restart on crash (configurable, max N attempts)
//! - All errors are caught — never panics, never crashes the agent
//! - `tools_changed` flag signals the gateway to rebuild the agent
//! - Thread-safe: all state behind RwLock, safe for concurrent access

use crate::bridge::{resolve_display_name, McpBridgeTool};
use crate::client::{McpClient, McpToolInfo};
use crate::config::{load_mcp_config, save_mcp_config, McpServerConfig, McpSettings};
use crate::transport::http::HttpTransport;
use crate::transport::stdio::StdioTransport;
use crate::transport::Transport;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::Tool;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Status of an MCP server connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Connected,
    Disconnected,
    Failed,
}

impl std::fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerStatus::Connected => write!(f, "connected"),
            ServerStatus::Disconnected => write!(f, "disconnected"),
            ServerStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Handle for a connected MCP server.
struct McpServerHandle {
    config: McpServerConfig,
    client: Arc<McpClient>,
    tools: Vec<McpToolInfo>,
    status: ServerStatus,
    restart_count: u32,
}

/// Manages all MCP server connections and their lifecycle.
pub struct McpManager {
    servers: RwLock<HashMap<String, McpServerHandle>>,
    settings: RwLock<McpSettings>,
    tools_changed: AtomicBool,
}

impl McpManager {
    /// Create a new McpManager and load config from `~/.temm1e/mcp.toml`.
    pub fn new() -> Self {
        let config = load_mcp_config();
        Self {
            servers: RwLock::new(HashMap::new()),
            settings: RwLock::new(config.settings),
            tools_changed: AtomicBool::new(false),
        }
    }

    /// Connect to all servers from the config file.
    /// Errors are logged but don't prevent other servers from connecting.
    pub async fn connect_all(&self) {
        let config = load_mcp_config();
        for server_config in &config.servers {
            if let Err(e) = server_config.validate() {
                warn!(server = %server_config.name, error = %e, "Skipping invalid MCP server config");
                continue;
            }
            match self.connect_server(server_config.clone()).await {
                Ok(tool_count) => {
                    info!(
                        server = %server_config.name,
                        tools = tool_count,
                        "MCP server connected"
                    );
                }
                Err(e) => {
                    warn!(
                        server = %server_config.name,
                        error = %e,
                        "Failed to connect MCP server — will retry on demand"
                    );
                    // Store as failed so we can retry later
                    let mut servers = self.servers.write().await;
                    servers.insert(
                        server_config.name.clone(),
                        McpServerHandle {
                            config: server_config.clone(),
                            client: Arc::new(McpClient::new(
                                &server_config.name,
                                Arc::new(crate::transport::NullTransport),
                            )),
                            tools: vec![],
                            status: ServerStatus::Failed,
                            restart_count: 0,
                        },
                    );
                }
            }
        }
    }

    /// Connect to a single MCP server. Returns the number of tools discovered.
    async fn connect_server(&self, config: McpServerConfig) -> Result<usize, Temm1eError> {
        let settings = self.settings.read().await;

        // Check server limit
        let servers = self.servers.read().await;
        if servers.len() >= settings.max_servers {
            return Err(Temm1eError::Tool(format!(
                "Maximum MCP servers ({}) reached. Remove one first with /mcp remove <name>.",
                settings.max_servers
            )));
        }
        drop(servers);

        let timeout = Duration::from_secs(settings.call_timeout_secs);
        drop(settings);

        // Create transport based on type
        let transport: Arc<dyn Transport> = match config.transport.as_str() {
            "stdio" => Arc::new(StdioTransport::spawn(&config, timeout).await?),
            "http" => Arc::new(HttpTransport::new(
                &config.name,
                config
                    .url
                    .as_deref()
                    .ok_or_else(|| Temm1eError::Tool("HTTP transport requires a URL".into()))?,
                timeout,
                config.headers.clone(),
            )?),
            other => {
                return Err(Temm1eError::Tool(format!("Unknown transport: {}", other)));
            }
        };

        // Create client and initialize
        let client = Arc::new(McpClient::new(&config.name, transport));
        client.initialize().await?;

        // Discover tools
        let tools = client.list_tools().await?;
        let tool_count = tools.len();

        // Store handle
        let mut servers = self.servers.write().await;
        servers.insert(
            config.name.clone(),
            McpServerHandle {
                config,
                client,
                tools,
                status: ServerStatus::Connected,
                restart_count: 0,
            },
        );

        self.tools_changed.store(true, Ordering::Relaxed);

        Ok(tool_count)
    }

    /// Add a new MCP server, connect to it, and save to config.
    pub async fn add_server(&self, config: McpServerConfig) -> Result<usize, Temm1eError> {
        config.validate().map_err(Temm1eError::Tool)?;

        // Check for duplicate names
        let servers = self.servers.read().await;
        if servers.contains_key(&config.name) {
            return Err(Temm1eError::Tool(format!(
                "MCP server '{}' already exists. Remove it first with /mcp remove {}.",
                config.name, config.name
            )));
        }
        drop(servers);

        let name = config.name.clone();
        let tool_count = self.connect_server(config.clone()).await?;

        // Save to config file
        let mut mcp_config = load_mcp_config();
        // Remove any existing entry with the same name
        mcp_config.servers.retain(|s| s.name != name);
        mcp_config.servers.push(config);
        if let Err(e) = save_mcp_config(&mcp_config) {
            warn!(error = %e, "Failed to save MCP config — server connected but not persisted");
        }

        Ok(tool_count)
    }

    /// Remove an MCP server, disconnect, and save config.
    pub async fn remove_server(&self, name: &str) -> Result<(), Temm1eError> {
        let mut servers = self.servers.write().await;
        if let Some(handle) = servers.remove(name) {
            let _ = handle.client.close().await;
            info!(server = %name, "MCP server removed");
        } else {
            return Err(Temm1eError::Tool(format!(
                "MCP server '{}' not found",
                name
            )));
        }
        drop(servers);

        // Remove from config file
        let mut mcp_config = load_mcp_config();
        mcp_config.servers.retain(|s| s.name != name);
        if let Err(e) = save_mcp_config(&mcp_config) {
            warn!(error = %e, "Failed to save MCP config after removal");
        }

        self.tools_changed.store(true, Ordering::Relaxed);

        Ok(())
    }

    /// Restart an MCP server (disconnect and reconnect).
    pub async fn restart_server(&self, name: &str) -> Result<usize, Temm1eError> {
        let config = {
            let servers = self.servers.read().await;
            match servers.get(name) {
                Some(handle) => handle.config.clone(),
                None => {
                    return Err(Temm1eError::Tool(format!(
                        "MCP server '{}' not found",
                        name
                    )));
                }
            }
        };

        // Close existing connection
        {
            let mut servers = self.servers.write().await;
            if let Some(handle) = servers.remove(name) {
                let _ = handle.client.close().await;
            }
        }

        // Reconnect
        let tool_count = self.connect_server(config).await?;
        info!(server = %name, tools = tool_count, "MCP server restarted");

        Ok(tool_count)
    }

    /// Get all MCP bridge tools, ready to be injected into the agent runtime.
    pub async fn bridge_tools(&self, existing_tool_names: &[String]) -> Vec<Arc<dyn Tool>> {
        let servers = self.servers.read().await;
        let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();

        // Collect all MCP tool names first for cross-server collision detection
        let mut all_mcp_names: Vec<String> = Vec::new();
        for handle in servers.values() {
            if handle.status != ServerStatus::Connected {
                continue;
            }
            for tool in &handle.tools {
                all_mcp_names.push(tool.name.clone());
            }
        }

        // Check for collisions between MCP tools from different servers
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for name in &all_mcp_names {
            *name_counts.entry(name.clone()).or_default() += 1;
        }

        for handle in servers.values() {
            if handle.status != ServerStatus::Connected || !handle.client.is_alive() {
                continue;
            }

            for tool_info in &handle.tools {
                // Check collision with built-in tools
                let mut display_name =
                    resolve_display_name(&handle.config.name, &tool_info.name, existing_tool_names);

                // Also namespace if multiple MCP servers expose the same tool name
                if name_counts.get(&tool_info.name).copied().unwrap_or(0) > 1
                    && display_name == crate::bridge::sanitize_tool_name(&tool_info.name)
                {
                    display_name = crate::bridge::sanitize_tool_name(&format!(
                        "{}_{}",
                        handle.config.name, tool_info.name
                    ));
                }

                all_tools.push(Arc::new(McpBridgeTool::new(
                    &handle.config.name,
                    &tool_info.name,
                    &display_name,
                    &tool_info.description,
                    tool_info.input_schema.clone(),
                    handle.client.clone(),
                )));
            }
        }

        all_tools
    }

    /// List all servers and their status (for `/mcp` command).
    pub async fn list_servers(&self) -> String {
        let servers = self.servers.read().await;
        if servers.is_empty() {
            return "No MCP servers configured.\n\nAdd one: /mcp add <name> <command or url>"
                .to_string();
        }

        let mut lines = vec!["MCP Servers:".to_string(), String::new()];
        for (name, handle) in servers.iter() {
            let alive = if handle.status == ServerStatus::Connected && handle.client.is_alive() {
                "healthy"
            } else {
                &handle.status.to_string()
            };
            let transport = &handle.config.transport;
            let tool_count = handle.tools.len();
            lines.push(format!(
                "  {} ({}) — {} tools [{}]",
                name, transport, tool_count, alive
            ));
            for tool in &handle.tools {
                lines.push(format!("    • {}  — {}", tool.name, tool.description));
            }
        }

        let total_tools: usize = servers
            .values()
            .filter(|h| h.status == ServerStatus::Connected)
            .map(|h| h.tools.len())
            .sum();
        lines.push(String::new());
        lines.push(format!("{} total MCP tools active.", total_tools));

        lines.join("\n")
    }

    /// Check and clear the tools_changed flag.
    /// Returns true if tools have changed since last check.
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::Relaxed)
    }

    /// Check health of all servers, auto-restart crashed ones.
    pub async fn health_check(&self) {
        let settings = self.settings.read().await.clone();
        let mut to_restart: Vec<String> = Vec::new();

        {
            let mut servers = self.servers.write().await;
            for (name, handle) in servers.iter_mut() {
                if handle.status == ServerStatus::Connected && !handle.client.is_alive() {
                    warn!(server = %name, "MCP server detected as dead");
                    handle.status = ServerStatus::Disconnected;
                    if settings.auto_restart && handle.restart_count < settings.max_restart_attempts
                    {
                        handle.restart_count += 1;
                        to_restart.push(name.clone());
                    } else if handle.restart_count >= settings.max_restart_attempts {
                        handle.status = ServerStatus::Failed;
                        error!(
                            server = %name,
                            attempts = handle.restart_count,
                            "MCP server exceeded max restart attempts — marked as failed"
                        );
                    }
                }
            }
        }

        for name in to_restart {
            info!(server = %name, "Auto-restarting MCP server");
            match self.restart_server(&name).await {
                Ok(tools) => {
                    info!(server = %name, tools = tools, "MCP server auto-restarted");
                }
                Err(e) => {
                    warn!(server = %name, error = %e, "Failed to auto-restart MCP server");
                }
            }
        }
    }

    /// Shut down all MCP servers gracefully.
    pub async fn shutdown(&self) {
        let mut servers = self.servers.write().await;
        for (name, handle) in servers.drain() {
            info!(server = %name, "Shutting down MCP server");
            let _ = handle.client.close().await;
        }
    }

    /// Get total tool count across all connected servers.
    pub async fn total_tool_count(&self) -> usize {
        let servers = self.servers.read().await;
        servers
            .values()
            .filter(|h| h.status == ServerStatus::Connected)
            .map(|h| h.tools.len())
            .sum()
    }

    /// Get server count.
    pub async fn server_count(&self) -> usize {
        self.servers.read().await.len()
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_manager_has_no_servers() {
        let manager = McpManager::new();
        assert_eq!(manager.server_count().await, 0);
        assert_eq!(manager.total_tool_count().await, 0);
    }

    #[tokio::test]
    async fn list_servers_empty() {
        let manager = McpManager::new();
        let list = manager.list_servers().await;
        assert!(list.contains("No MCP servers configured"));
    }

    #[tokio::test]
    async fn remove_nonexistent_server() {
        let manager = McpManager::new();
        let result = manager.remove_server("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tools_changed_flag() {
        let manager = McpManager::new();
        assert!(!manager.take_tools_changed());
        manager.tools_changed.store(true, Ordering::Relaxed);
        assert!(manager.take_tools_changed());
        assert!(!manager.take_tools_changed()); // cleared after take
    }

    #[tokio::test]
    async fn bridge_tools_empty_when_no_servers() {
        let manager = McpManager::new();
        let tools = manager.bridge_tools(&[]).await;
        assert!(tools.is_empty());
    }
}
