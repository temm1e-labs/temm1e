//! Shell tool — executes commands on the host via tokio::process::Command.

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum output size returned to the model (32 KB).
const MAX_OUTPUT_SIZE: usize = 32 * 1024;

#[derive(Default)]
pub struct ShellTool;

impl ShellTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command on the host machine and return stdout/stderr. \
         Commands run in the session workspace directory with a 30-second timeout. \
         Use this for system tasks, file operations, package management, git, etc."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (e.g., 'ls -la', 'cat file.txt', 'git status')"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 30, max: 300)"
                }
            },
            "required": ["command"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: Vec::new(),
            shell_access: true,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let command = input
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: command".into()))?;

        let timeout_secs = input
            .arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(300);

        tracing::info!(command = %command, timeout = timeout_secs, "Executing shell command");

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&ctx.workspace_path)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str("[stderr]\n");
                    content.push_str(&stderr);
                }

                if content.is_empty() {
                    content = format!(
                        "Command completed with exit code {}",
                        output.status.code().unwrap_or(-1)
                    );
                }

                // Truncate if too large
                if content.len() > MAX_OUTPUT_SIZE {
                    content.truncate(MAX_OUTPUT_SIZE);
                    content.push_str("\n... [output truncated]");
                }

                let is_error = !output.status.success();
                Ok(ToolOutput { content, is_error })
            }
            Ok(Err(e)) => Ok(ToolOutput {
                content: format!("Failed to execute command: {}", e),
                is_error: true,
            }),
            Err(_) => Ok(ToolOutput {
                content: format!("Command timed out after {} seconds", timeout_secs),
                is_error: true,
            }),
        }
    }
}
