//! SelfAddMcpTool — agent tool for self-installing MCP servers.
//!
//! Purpose-built for the agent to extend its own capabilities. Unlike
//! `mcp_manage` (general CRUD), this tool is optimized for the self-extension
//! flow: the agent discovered a server via `self_extend_tool` and now wants
//! to install it.

use crate::config::McpServerConfig;
use crate::manager::McpManager;
use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tracing::{info, warn};

/// Agent tool for self-installing MCP servers.
pub struct SelfAddMcpTool {
    manager: Arc<McpManager>,
}

impl SelfAddMcpTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SelfAddMcpTool {
    fn name(&self) -> &str {
        "self_add_mcp"
    }

    fn description(&self) -> &str {
        "Install an MCP server to gain new tool capabilities. \
         Use self_extend_tool first to find the right server, then call this \
         with the name, command, and args. The server's tools become available \
         immediately. Always tell the user what you're installing and why."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Short name for the server (e.g., 'playwright', 'postgres', 'github')"
                },
                "command": {
                    "type": "string",
                    "description": "Command to run (e.g., 'npx')"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments for the command (e.g., ['@playwright/mcp@latest'])"
                },
                "url": {
                    "type": "string",
                    "description": "URL for HTTP transport (use instead of command/args for remote MCP servers)"
                }
            },
            "required": ["name"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec!["*".to_string()],
            shell_access: true,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, temm1e_core::types::error::Temm1eError> {
        let args = &input.arguments;

        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => {
                return Ok(ToolOutput {
                    content: "Missing 'name' — provide a short name for the MCP server."
                        .to_string(),
                    is_error: true,
                });
            }
        };

        // Determine transport: HTTP if url is provided, stdio if command is provided
        let has_url = args
            .get("url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| !u.is_empty());
        let has_command = args
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|c| !c.is_empty());

        if !has_url && !has_command {
            return Ok(ToolOutput {
                content: "Provide either 'command' (for stdio) or 'url' (for HTTP). \
                         Use self_extend_tool to find the right command for a capability."
                    .to_string(),
                is_error: true,
            });
        }

        let config = if has_url {
            let url = args["url"].as_str().unwrap();
            McpServerConfig::http(name, url)
        } else {
            let command = args["command"].as_str().unwrap();
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
        };

        info!(server = %name, "Agent self-installing MCP server");

        match self.manager.add_server(config).await {
            Ok(tool_count) => {
                let tools_list = {
                    let listing = self.manager.list_servers().await;
                    listing
                };
                info!(
                    server = %name,
                    tools = tool_count,
                    "MCP server self-installed successfully"
                );
                Ok(ToolOutput {
                    content: format!(
                        "MCP server '{}' installed with {} new tools. \
                         These tools are now available for use.\n\n{}",
                        name, tool_count, tools_list
                    ),
                    is_error: false,
                })
            }
            Err(e) => {
                warn!(server = %name, error = %e, "Failed to self-install MCP server");
                Ok(ToolOutput {
                    content: format!(
                        "Failed to install MCP server '{}': {}\n\n\
                         Troubleshooting:\n\
                         - Is the command installed? Try: which {}\n\
                         - For npx packages, ensure Node.js is installed\n\
                         - Check if required env vars are set\n\
                         - Use self_extend_tool to verify the correct install command",
                        name,
                        e,
                        if has_command {
                            args["command"].as_str().unwrap_or("npx")
                        } else {
                            "curl"
                        }
                    ),
                    is_error: true,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn self_add_tool_name() {
        let manager = Arc::new(McpManager::new());
        let tool = SelfAddMcpTool::new(manager);
        assert_eq!(tool.name(), "self_add_mcp");
    }

    #[tokio::test]
    async fn self_add_missing_name() {
        let manager = Arc::new(McpManager::new());
        let tool = SelfAddMcpTool::new(manager);
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "self_add_mcp".to_string(),
            arguments: serde_json::json!({}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Missing 'name'"));
    }

    #[tokio::test]
    async fn self_add_missing_command_and_url() {
        let manager = Arc::new(McpManager::new());
        let tool = SelfAddMcpTool::new(manager);
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "self_add_mcp".to_string(),
            arguments: serde_json::json!({"name": "test"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("command"));
    }

    #[test]
    fn parameters_schema_valid() {
        let manager = Arc::new(McpManager::new());
        let tool = SelfAddMcpTool::new(manager);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["name"].is_object());
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["properties"]["url"].is_object());
    }
}
