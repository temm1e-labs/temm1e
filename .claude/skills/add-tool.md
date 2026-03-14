# Skill: Add a new tool to TEMM1E

## When to use

Use this skill when the user asks to add a new tool capability for the TEMM1E agent (e.g., HTTP client, database query, image generation, code execution, web scraping).

## Reference implementation

Study the existing definitions:
- `crates/temm1e-core/src/traits/tool.rs` -- the `Tool` trait, `ToolDeclarations`, `ToolInput`, `ToolOutput`, `ToolContext`, `PathAccess`
- `crates/temm1e-tools/src/lib.rs` -- the tools crate (currently minimal, ready for implementations)
- `crates/temm1e-tools/Cargo.toml` -- tool crate dependencies and feature flags

## Steps

### 1. Create the tool source file

Create `crates/temm1e-tools/src/<tool_name>.rs` using the template below.

### 2. Add the module to lib.rs

Edit `crates/temm1e-tools/src/lib.rs`:
- Add `pub mod <tool_name>;`
- Add `pub use <tool_name>::<ToolName>Tool;`
- If the tool requires an optional dependency, gate it: `#[cfg(feature = "<tool_name>")] pub mod <tool_name>;`

### 3. Add dependencies and feature flags if needed

Edit `crates/temm1e-tools/Cargo.toml`:
- Add any tool-specific dependencies (mark optional if feature-gated)
- Add feature flag if needed: `<tool_name> = ["dep:some-crate"]`

If feature-gated, also edit root `Cargo.toml`:
- Add the feature flag: `<tool_name> = ["temm1e-tools/<tool_name>"]`

### 4. Write tests

Include tests in the tool source file:
- Test `name()` returns the correct string
- Test `description()` is non-empty
- Test `parameters_schema()` returns valid JSON Schema
- Test `declarations()` declares appropriate file/network/shell access
- Test `execute()` with valid input
- Test `execute()` with invalid/missing arguments returns an error ToolOutput
- Test sandbox boundary enforcement (e.g., path traversal is blocked)

### 5. Verify

```bash
cargo check -p temm1e-tools
cargo test -p temm1e-tools
cargo clippy -p temm1e-tools -- -D warnings
```

## Template

```rust
//! <ToolName> tool -- <brief description>.

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolDeclarations, ToolInput, ToolOutput, ToolContext, PathAccess};

/// <ToolName> tool for the TEMM1E agent.
pub struct <ToolName>Tool {
    // TODO: Add any state the tool needs (e.g., HTTP client, config)
}

impl <ToolName>Tool {
    pub fn new() -> Self {
        Self {
            // TODO: Initialize tool state
        }
    }
}

#[async_trait]
impl Tool for <ToolName>Tool {
    fn name(&self) -> &str {
        "<tool_name>"
    }

    fn description(&self) -> &str {
        // This is shown to the AI model so it knows when to use the tool.
        // Be specific and concise.
        "<One-line description of what this tool does>"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // JSON Schema describing the tool's input parameters.
        // The AI model uses this to construct valid tool calls.
        serde_json::json!({
            "type": "object",
            "properties": {
                // TODO: Define parameters
                // Example:
                // "url": {
                //     "type": "string",
                //     "description": "The URL to fetch"
                // },
                // "method": {
                //     "type": "string",
                //     "enum": ["GET", "POST", "PUT", "DELETE"],
                //     "default": "GET",
                //     "description": "HTTP method to use"
                // }
            },
            "required": [
                // TODO: List required parameter names
            ]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        // Declare what resources this tool needs.
        // The sandbox enforcer checks these before execution.
        ToolDeclarations {
            file_access: vec![
                // Examples:
                // PathAccess::Read("/workspace".to_string()),
                // PathAccess::Write("/workspace/output".to_string()),
                // PathAccess::ReadWrite("/tmp".to_string()),
            ],
            network_access: vec![
                // Examples:
                // "*".to_string(),                    // any domain
                // "api.example.com".to_string(),      // specific domain
            ],
            shell_access: false,
            // Set to true only if the tool runs shell commands
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        // 1. Parse and validate input arguments
        // let some_param = input.arguments.get("some_param")
        //     .and_then(|v| v.as_str())
        //     .ok_or_else(|| Temm1eError::Tool("Missing required parameter: some_param".into()))?;

        // 2. Enforce sandbox rules
        //    - Validate file paths are within ctx.workspace_path
        //    - Check network access against declarations
        //    - Log the operation for audit

        // 3. Execute the tool logic
        // let result = do_something(some_param).await?;

        // 4. Return the result
        // Ok(ToolOutput {
        //     content: result,
        //     is_error: false,
        // })

        // On error, return ToolOutput with is_error: true instead of Err()
        // when the error is a "tool produced an error result" vs "tool failed to run"
        // Ok(ToolOutput {
        //     content: format!("Error: {}", e),
        //     is_error: true,
        // })

        todo!("Implement execute for <ToolName>Tool")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_path: PathBuf::from("/tmp/test-workspace"),
            session_id: "test-session".to_string(),
        }
    }

    #[test]
    fn tool_name() {
        let tool = <ToolName>Tool::new();
        assert_eq!(tool.name(), "<tool_name>");
    }

    #[test]
    fn tool_description_not_empty() {
        let tool = <ToolName>Tool::new();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn parameters_schema_is_valid_object() {
        let tool = <ToolName>Tool::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema.get("properties").is_some());
    }

    #[test]
    fn declarations_are_appropriate() {
        let tool = <ToolName>Tool::new();
        let decl = tool.declarations();
        // TODO: Assert specific declaration requirements
        // e.g., assert!(!decl.shell_access);
        // e.g., assert!(decl.network_access.contains(&"api.example.com".to_string()));
        let _ = decl; // remove this line when adding assertions
    }

    #[tokio::test]
    async fn execute_with_valid_input() {
        // let tool = <ToolName>Tool::new();
        // let input = ToolInput {
        //     name: "<tool_name>".to_string(),
        //     arguments: serde_json::json!({
        //         "param": "value"
        //     }),
        // };
        // let result = tool.execute(input, &test_ctx()).await.unwrap();
        // assert!(!result.is_error);
    }

    #[tokio::test]
    async fn execute_with_missing_param_returns_error() {
        // let tool = <ToolName>Tool::new();
        // let input = ToolInput {
        //     name: "<tool_name>".to_string(),
        //     arguments: serde_json::json!({}),
        // };
        // let result = tool.execute(input, &test_ctx()).await;
        // // Should either return Ok(ToolOutput { is_error: true, .. })
        // // or Err(Temm1eError::Tool(...))
    }
}
```

## Key conventions

- **Error vs. tool error**: Return `Err(Temm1eError::Tool(...))` when the tool infrastructure fails (e.g., missing dependency). Return `Ok(ToolOutput { is_error: true, content: "..." })` when the tool ran but the operation failed (e.g., HTTP 404, file not found). The AI model sees `is_error: true` results and can retry or adjust.
- **Sandbox declarations**: Every tool MUST declare its resource needs in `declarations()`. The sandbox enforcer validates these at runtime. Be specific -- declare only the domains and paths the tool actually needs.
- **Path safety**: Always validate that file paths are within `ctx.workspace_path`. Reject path traversal attempts (`../`). Use `path.canonicalize()` and check `starts_with(ctx.workspace_path)`.
- **Parameters schema**: Use JSON Schema format. The AI model reads this to understand how to call the tool. Include `description` for each property.
- **Async**: All tool execution is async. Use `tokio` for I/O operations.
- **Feature gating**: If the tool depends on a heavy optional crate (e.g., `chromiumoxide` for browser), put it behind a feature flag.
- **Audit logging**: Use `tracing::debug!` or `tracing::info!` to log tool invocations for the security audit trail.
