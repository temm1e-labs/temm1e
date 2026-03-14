//! TEMM1E MCP — Model Context Protocol client integration.
//!
//! Connects to external MCP servers (stdio subprocess or HTTP), discovers
//! their tools, and exposes them as native TEMM1E `Tool` implementations
//! via the `McpBridgeTool` adapter. The agent sees MCP tools identically
//! to built-in tools.
//!
//! # Architecture
//!
//! ```text
//! Agent Runtime
//!   └── McpBridgeTool (implements Tool trait)
//!         └── McpClient (MCP protocol: initialize, list_tools, call_tool)
//!               └── Transport (stdio subprocess or HTTP)
//!                     └── JSON-RPC 2.0 over newline-delimited JSON
//! ```
//!
//! # Usage
//!
//! ```ignore
//! let manager = McpManager::new();
//! manager.connect_all().await;
//! let mcp_tools = manager.bridge_tools(&existing_tool_names).await;
//! // Add mcp_tools to the agent's tool vec
//! ```

pub mod bridge;
pub mod client;
pub mod config;
pub mod jsonrpc;
pub mod manager;
pub mod mcp_manage;
pub mod self_add;
pub mod self_extend;
pub mod transport;

pub use config::{McpConfig, McpServerConfig};
pub use manager::McpManager;
pub use mcp_manage::McpManageTool;
pub use self_add::SelfAddMcpTool;
pub use self_extend::SelfExtendTool;
