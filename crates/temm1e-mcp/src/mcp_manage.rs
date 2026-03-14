//! McpManageTool — agent tool that lets the LLM add, remove, list, and restart MCP servers.
//!
//! Actions:
//! - "list": List all MCP servers and their tools
//! - "add": Add and connect a new MCP server
//! - "remove": Disconnect and remove an MCP server
//! - "restart": Restart a crashed or misbehaving server

use crate::config::McpServerConfig;
use crate::manager::McpManager;
use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tracing::{info, warn};

/// Agent tool for managing MCP servers at runtime.
pub struct McpManageTool {
    manager: Arc<McpManager>,
}

impl McpManageTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for McpManageTool {
    fn name(&self) -> &str {
        "mcp_manage"
    }

    fn description(&self) -> &str {
        "Manage MCP (Model Context Protocol) servers. Actions: 'list' (show all servers and tools), \
         'add' (connect a new MCP server), 'remove' (disconnect a server), 'restart' (restart a server). \
         MCP servers provide external tools like search, SMS, document stores, etc."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "remove", "restart"],
                    "description": "The action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Server name (required for add, remove, restart)"
                },
                "command": {
                    "type": "string",
                    "description": "Command to run for stdio transport (required for add with stdio)"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments for the command (optional, for add with stdio)"
                },
                "url": {
                    "type": "string",
                    "description": "URL for HTTP transport (required for add with http)"
                },
                "transport": {
                    "type": "string",
                    "enum": ["stdio", "http"],
                    "description": "Transport type (default: stdio). Use 'http' for remote servers."
                }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec!["*".to_string()],
            shell_access: true, // Spawns subprocesses for stdio transport
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, temm1e_core::types::error::Temm1eError> {
        let args = &input.arguments;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        match action {
            "list" => {
                let listing = self.manager.list_servers().await;
                Ok(ToolOutput {
                    content: listing,
                    is_error: false,
                })
            }

            "add" => {
                let name = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) if !n.is_empty() => n,
                    _ => {
                        return Ok(ToolOutput {
                            content: "Missing 'name' field for add action".to_string(),
                            is_error: true,
                        });
                    }
                };

                let transport = args
                    .get("transport")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stdio");

                let config = match transport {
                    "stdio" => {
                        let command = match args.get("command").and_then(|v| v.as_str()) {
                            Some(c) if !c.is_empty() => c,
                            _ => {
                                return Ok(ToolOutput {
                                    content: "Missing 'command' field for stdio transport"
                                        .to_string(),
                                    is_error: true,
                                });
                            }
                        };
                        let cmd_args: Vec<String> = args
                            .get("args")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        McpServerConfig::stdio(name, command, cmd_args)
                    }
                    "http" => {
                        let url = match args.get("url").and_then(|v| v.as_str()) {
                            Some(u) if !u.is_empty() => u,
                            _ => {
                                return Ok(ToolOutput {
                                    content: "Missing 'url' field for http transport".to_string(),
                                    is_error: true,
                                });
                            }
                        };
                        McpServerConfig::http(name, url)
                    }
                    other => {
                        return Ok(ToolOutput {
                            content: format!(
                                "Unknown transport '{}'. Use 'stdio' or 'http'.",
                                other
                            ),
                            is_error: true,
                        });
                    }
                };

                match self.manager.add_server(config).await {
                    Ok(tool_count) => {
                        info!(server = %name, tools = tool_count, "MCP server added via agent tool");
                        Ok(ToolOutput {
                            content: format!(
                                "MCP server '{}' connected successfully with {} tools. \
                                 The new tools will be available in the next message.",
                                name, tool_count
                            ),
                            is_error: false,
                        })
                    }
                    Err(e) => {
                        warn!(server = %name, error = %e, "Failed to add MCP server via agent tool");
                        Ok(ToolOutput {
                            content: format!("Failed to add MCP server '{}': {}", name, e),
                            is_error: true,
                        })
                    }
                }
            }

            "remove" => {
                let name = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) if !n.is_empty() => n,
                    _ => {
                        return Ok(ToolOutput {
                            content: "Missing 'name' field for remove action".to_string(),
                            is_error: true,
                        });
                    }
                };

                match self.manager.remove_server(name).await {
                    Ok(()) => {
                        info!(server = %name, "MCP server removed via agent tool");
                        Ok(ToolOutput {
                            content: format!("MCP server '{}' removed.", name),
                            is_error: false,
                        })
                    }
                    Err(e) => Ok(ToolOutput {
                        content: format!("Failed to remove MCP server '{}': {}", name, e),
                        is_error: true,
                    }),
                }
            }

            "restart" => {
                let name = match args.get("name").and_then(|v| v.as_str()) {
                    Some(n) if !n.is_empty() => n,
                    _ => {
                        return Ok(ToolOutput {
                            content: "Missing 'name' field for restart action".to_string(),
                            is_error: true,
                        });
                    }
                };

                match self.manager.restart_server(name).await {
                    Ok(tool_count) => {
                        info!(server = %name, tools = tool_count, "MCP server restarted via agent tool");
                        Ok(ToolOutput {
                            content: format!(
                                "MCP server '{}' restarted with {} tools.",
                                name, tool_count
                            ),
                            is_error: false,
                        })
                    }
                    Err(e) => Ok(ToolOutput {
                        content: format!("Failed to restart MCP server '{}': {}", name, e),
                        is_error: true,
                    }),
                }
            }

            _ => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Use: list, add, remove, restart.",
                    action
                ),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcp_manage_tool_name() {
        let manager = Arc::new(McpManager::new());
        let tool = McpManageTool::new(manager);
        assert_eq!(tool.name(), "mcp_manage");
    }

    #[tokio::test]
    async fn mcp_manage_list_empty() {
        let manager = Arc::new(McpManager::new());
        let tool = McpManageTool::new(manager);
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "mcp_manage".to_string(),
            arguments: serde_json::json!({"action": "list"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("No MCP servers configured"));
    }

    #[tokio::test]
    async fn mcp_manage_add_missing_name() {
        let manager = Arc::new(McpManager::new());
        let tool = McpManageTool::new(manager);
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "mcp_manage".to_string(),
            arguments: serde_json::json!({"action": "add"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Missing 'name'"));
    }

    #[tokio::test]
    async fn mcp_manage_unknown_action() {
        let manager = Arc::new(McpManager::new());
        let tool = McpManageTool::new(manager);
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "mcp_manage".to_string(),
            arguments: serde_json::json!({"action": "explode"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Unknown action"));
    }

    #[test]
    fn parameters_schema_is_valid() {
        let manager = Arc::new(McpManager::new());
        let tool = McpManageTool::new(manager);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
    }
}
