//! MCP client — handles the MCP protocol lifecycle:
//! initialize handshake, tool discovery, tool invocation, ping.

use crate::transport::Transport;
use std::sync::Arc;
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, info};

/// Information about a single tool exposed by an MCP server.
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Information about the connected MCP server.
#[derive(Debug, Clone)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
}

/// MCP client — wraps a transport and speaks the MCP protocol.
pub struct McpClient {
    transport: Arc<dyn Transport>,
    server_info: tokio::sync::RwLock<Option<McpServerInfo>>,
    server_name: String,
}

impl McpClient {
    pub fn new(server_name: &str, transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            server_info: tokio::sync::RwLock::new(None),
            server_name: server_name.to_string(),
        }
    }

    /// Perform the MCP initialize handshake.
    /// Must be called before any other operations.
    pub async fn initialize(&self) -> Result<McpServerInfo, Temm1eError> {
        let params = serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "tools": {}
            },
            "clientInfo": {
                "name": "temm1e",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        debug!(server = %self.server_name, "MCP initialize handshake");

        let response = self.transport.send("initialize", Some(params)).await?;

        let result = response.into_result()?;

        let server_info = McpServerInfo {
            name: result["serverInfo"]["name"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            version: result["serverInfo"]["version"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            protocol_version: result["protocolVersion"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
        };

        info!(
            server = %self.server_name,
            mcp_server_name = %server_info.name,
            mcp_server_version = %server_info.version,
            protocol = %server_info.protocol_version,
            "MCP server initialized"
        );

        *self.server_info.write().await = Some(server_info.clone());

        // Send initialized notification
        self.transport
            .notify("notifications/initialized", None)
            .await?;

        Ok(server_info)
    }

    /// List tools exposed by the MCP server.
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, Temm1eError> {
        let response = self.transport.send("tools/list", None).await?;
        let result = response.into_result()?;

        let tools_array = result["tools"]
            .as_array()
            .ok_or_else(|| Temm1eError::Tool("MCP tools/list: missing 'tools' array".into()))?;

        let mut tools = Vec::with_capacity(tools_array.len());
        for tool_value in tools_array {
            let name = tool_value["name"].as_str().unwrap_or("unnamed").to_string();
            let description = tool_value["description"].as_str().unwrap_or("").to_string();
            let input_schema = tool_value
                .get("inputSchema")
                .cloned()
                .unwrap_or(serde_json::json!({"type": "object"}));

            tools.push(McpToolInfo {
                name,
                description,
                input_schema,
            });
        }

        debug!(
            server = %self.server_name,
            tool_count = tools.len(),
            "Discovered MCP tools"
        );

        Ok(tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<McpToolResult, Temm1eError> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });

        debug!(
            server = %self.server_name,
            tool = %tool_name,
            "Calling MCP tool"
        );

        let response = self.transport.send("tools/call", Some(params)).await?;
        let result = response.into_result()?;

        // Extract text content from the MCP tool result
        let content_array = result["content"].as_array();
        let mut text_parts: Vec<String> = Vec::new();

        if let Some(contents) = content_array {
            for part in contents {
                if let Some(text) = part["text"].as_str() {
                    text_parts.push(text.to_string());
                } else if let Some(data) = part.get("data") {
                    // Binary/image content — include as JSON
                    text_parts.push(serde_json::to_string(data).unwrap_or_default());
                }
            }
        } else if let Some(text) = result.as_str() {
            // Some servers return plain text
            text_parts.push(text.to_string());
        }

        let is_error = result["isError"].as_bool().unwrap_or(false);

        Ok(McpToolResult {
            content: text_parts.join("\n"),
            is_error,
        })
    }

    /// Ping the MCP server to check health.
    pub async fn ping(&self) -> Result<(), Temm1eError> {
        let response = self.transport.send("ping", None).await?;
        let _ = response.into_result()?;
        Ok(())
    }

    /// Check if the underlying transport is alive.
    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    /// Close the client and transport.
    pub async fn close(&self) -> Result<(), Temm1eError> {
        self.transport.close().await
    }

    /// Get server info (available after initialize).
    pub async fn server_info(&self) -> Option<McpServerInfo> {
        self.server_info.read().await.clone()
    }
}

/// Result from calling an MCP tool.
#[derive(Debug, Clone)]
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_info_debug() {
        let info = McpToolInfo {
            name: "test".to_string(),
            description: "A test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        assert_eq!(info.name, "test");
        assert_eq!(info.description, "A test tool");
    }

    #[test]
    fn mcp_server_info_debug() {
        let info = McpServerInfo {
            name: "test-server".to_string(),
            version: "1.0.0".to_string(),
            protocol_version: "2025-11-25".to_string(),
        };
        assert_eq!(info.name, "test-server");
    }

    #[test]
    fn mcp_tool_result_error() {
        let result = McpToolResult {
            content: "Something went wrong".to_string(),
            is_error: true,
        };
        assert!(result.is_error);
    }
}
