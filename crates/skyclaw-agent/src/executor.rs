//! Tool executor — validates tool calls against declarations and executes them
//! within workspace-scoped sandboxing.

use std::sync::Arc;

use skyclaw_core::{Tool, ToolContext, ToolInput, ToolOutput, PathAccess};
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::session::SessionContext;
use tracing::{info, warn};

/// Execute a tool call, validating sandbox constraints first.
pub async fn execute_tool(
    tool_name: &str,
    arguments: serde_json::Value,
    tools: &[Arc<dyn Tool>],
    session: &SessionContext,
) -> Result<ToolOutput, SkyclawError> {
    // Find the matching tool
    let tool = tools
        .iter()
        .find(|t| t.name() == tool_name)
        .ok_or_else(|| {
            SkyclawError::Tool(format!("Unknown tool: {}", tool_name))
        })?;

    // Validate sandbox declarations against workspace scope
    validate_sandbox(tool.as_ref(), session)?;

    let ctx = ToolContext {
        workspace_path: session.workspace_path.clone(),
        session_id: session.session_id.clone(),
    };

    let input = ToolInput {
        name: tool_name.to_string(),
        arguments,
    };

    info!(tool = tool_name, session = %session.session_id, "Executing tool");

    match tool.execute(input, &ctx).await {
        Ok(output) => {
            if output.is_error {
                warn!(tool = tool_name, "Tool returned error: {}", output.content);
            }
            Ok(output)
        }
        Err(e) => {
            warn!(tool = tool_name, error = %e, "Tool execution failed");
            Err(e)
        }
    }
}

/// Validate that a tool's declared resource access is within the session's workspace scope.
fn validate_sandbox(tool: &dyn Tool, session: &SessionContext) -> Result<(), SkyclawError> {
    let declarations = tool.declarations();
    let workspace = &session.workspace_path;

    // Check file access paths are within the workspace
    for path_access in &declarations.file_access {
        let path_str = match path_access {
            PathAccess::Read(p) => p,
            PathAccess::Write(p) => p,
            PathAccess::ReadWrite(p) => p,
        };

        let path = std::path::Path::new(path_str);

        // Resolve to absolute if relative
        let abs_path = if path.is_relative() {
            workspace.join(path)
        } else {
            path.to_path_buf()
        };

        // Canonicalize workspace for comparison (best-effort)
        let workspace_canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        let path_canonical = abs_path
            .canonicalize()
            .unwrap_or(abs_path);

        if !path_canonical.starts_with(&workspace_canonical) {
            return Err(SkyclawError::SandboxViolation(format!(
                "Tool '{}' declares access to '{}' which is outside workspace '{}'",
                tool.name(),
                path_str,
                workspace.display()
            )));
        }
    }

    Ok(())
}
