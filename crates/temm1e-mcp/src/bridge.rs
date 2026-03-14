//! McpBridgeTool — adapter that wraps an MCP server's tool as a TEMM1E Tool.
//!
//! The agent sees MCP tools identically to built-in tools. The bridge
//! translates between TEMM1E's Tool trait and MCP's JSON-RPC protocol.

use crate::client::McpClient;
use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tracing::{debug, warn};

/// Bridge adapter: wraps a single MCP tool as a TEMM1E Tool.
pub struct McpBridgeTool {
    /// The server-qualified display name (e.g., "sms.send_sms" if namespaced).
    display_name: String,
    /// The original tool name on the MCP server.
    mcp_tool_name: String,
    /// The MCP server name (for logging).
    server_name: String,
    /// Tool description from the MCP server.
    description: String,
    /// JSON Schema for tool parameters.
    input_schema: serde_json::Value,
    /// The MCP client to send calls through.
    client: Arc<McpClient>,
}

impl McpBridgeTool {
    pub fn new(
        server_name: &str,
        mcp_tool_name: &str,
        display_name: &str,
        description: &str,
        input_schema: serde_json::Value,
        client: Arc<McpClient>,
    ) -> Self {
        Self {
            display_name: display_name.to_string(),
            mcp_tool_name: mcp_tool_name.to_string(),
            server_name: server_name.to_string(),
            description: description.to_string(),
            input_schema,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpBridgeTool {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    fn declarations(&self) -> ToolDeclarations {
        // MCP tools have opaque resource needs — we declare network access
        // since they may call external services, but can't know specifics.
        ToolDeclarations {
            file_access: vec![],
            network_access: vec!["*".to_string()], // MCP tools may access any network
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, temm1e_core::types::error::Temm1eError> {
        // Check if the MCP server is still alive
        if !self.client.is_alive() {
            warn!(
                server = %self.server_name,
                tool = %self.mcp_tool_name,
                "MCP server is not running — returning error"
            );
            return Ok(ToolOutput {
                content: format!(
                    "MCP server '{}' is not running. Use /mcp restart {} to reconnect.",
                    self.server_name, self.server_name
                ),
                is_error: true,
            });
        }

        debug!(
            server = %self.server_name,
            tool = %self.mcp_tool_name,
            "Executing MCP bridge tool"
        );

        // Call the MCP tool — catch all errors and convert to ToolOutput
        match self
            .client
            .call_tool(&self.mcp_tool_name, input.arguments)
            .await
        {
            Ok(result) => Ok(ToolOutput {
                content: result.content,
                is_error: result.is_error,
            }),
            Err(e) => {
                warn!(
                    server = %self.server_name,
                    tool = %self.mcp_tool_name,
                    error = %e,
                    "MCP tool call failed"
                );
                Ok(ToolOutput {
                    content: format!(
                        "MCP tool '{}' on server '{}' failed: {}",
                        self.mcp_tool_name, self.server_name, e
                    ),
                    is_error: true,
                })
            }
        }
    }
}

/// Sanitize a tool name to match the OpenAI function name pattern `^[a-zA-Z0-9_-]+$`.
/// Replaces any invalid character with `_`.
pub fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Resolve display name for an MCP tool, handling collisions with existing tools.
/// Output is always sanitized to `[a-zA-Z0-9_-]+` for provider compatibility.
pub fn resolve_display_name(
    server_name: &str,
    tool_name: &str,
    existing_tool_names: &[String],
) -> String {
    let name = if existing_tool_names.contains(&sanitize_tool_name(tool_name)) {
        // Collision: namespace with server name using underscore (not dot)
        format!("{}_{}", server_name, tool_name)
    } else {
        tool_name.to_string()
    };
    sanitize_tool_name(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_name_no_collision() {
        let existing = vec!["shell".to_string(), "file_read".to_string()];
        assert_eq!(
            resolve_display_name("sms", "send_sms", &existing),
            "send_sms"
        );
    }

    #[test]
    fn resolve_name_with_collision() {
        let existing = vec!["shell".to_string(), "search".to_string()];
        assert_eq!(
            resolve_display_name("web", "search", &existing),
            "web_search"
        );
    }

    #[test]
    fn sanitize_dots_and_special_chars() {
        assert_eq!(sanitize_tool_name("server.tool"), "server_tool");
        assert_eq!(sanitize_tool_name("my-tool_v2"), "my-tool_v2");
        assert_eq!(sanitize_tool_name("tool@name!"), "tool_name_");
        assert_eq!(sanitize_tool_name("normal_name"), "normal_name");
    }

    #[test]
    fn resolve_name_empty_existing() {
        let existing: Vec<String> = vec![];
        assert_eq!(
            resolve_display_name("test", "my_tool", &existing),
            "my_tool"
        );
    }
}
