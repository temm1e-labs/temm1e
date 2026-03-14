//! Custom script-based tools — agent-authored tools that persist across sessions.
//!
//! The agent can create new tools at runtime by writing scripts (bash/python/node).
//! Tools are saved to `~/.temm1e/custom-tools/` with a companion metadata JSON file.
//! A `ScriptToolAdapter` wraps each script as a native `Tool` trait implementation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tracing::{debug, info, warn};

/// Metadata for a custom script tool, stored as `{name}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptToolMeta {
    pub name: String,
    pub description: String,
    pub language: String,              // "bash", "python", "node"
    pub parameters: serde_json::Value, // JSON Schema
}

/// Adapter that wraps a script file as a TEMM1E Tool.
pub struct ScriptToolAdapter {
    meta: ScriptToolMeta,
    script_path: PathBuf,
}

impl ScriptToolAdapter {
    /// Load a script tool from its metadata file.
    pub fn from_meta_file(meta_path: &Path) -> Result<Self, Temm1eError> {
        let content = std::fs::read_to_string(meta_path).map_err(|e| {
            Temm1eError::Tool(format!(
                "Cannot read tool metadata {}: {}",
                meta_path.display(),
                e
            ))
        })?;
        let meta: ScriptToolMeta = serde_json::from_str(&content).map_err(|e| {
            Temm1eError::Tool(format!(
                "Invalid tool metadata {}: {}",
                meta_path.display(),
                e
            ))
        })?;

        // Resolve script path (same directory, same name, language extension)
        let ext = match meta.language.as_str() {
            "python" => "py",
            "node" => "js",
            _ => "sh", // default to bash
        };
        let script_path = meta_path.with_extension(ext);
        if !script_path.exists() {
            return Err(Temm1eError::Tool(format!(
                "Script file not found: {}",
                script_path.display()
            )));
        }

        Ok(Self { meta, script_path })
    }
}

#[async_trait]
impl Tool for ScriptToolAdapter {
    fn name(&self) -> &str {
        &self.meta.name
    }

    fn description(&self) -> &str {
        &self.meta.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.meta.parameters.clone()
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec![],
            shell_access: true, // scripts require shell
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let interpreter = match self.meta.language.as_str() {
            "python" => "python3",
            "node" => "node",
            _ => "bash",
        };

        debug!(
            tool = %self.meta.name,
            script = %self.script_path.display(),
            interpreter = %interpreter,
            "Executing custom script tool"
        );

        // Pass arguments as JSON via stdin
        let input_json = serde_json::to_string(&input.arguments).unwrap_or_default();

        let result = tokio::process::Command::new(interpreter)
            .arg(&self.script_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let mut child = match result {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!(
                        "Failed to run {} {}: {}",
                        interpreter,
                        self.script_path.display(),
                        e
                    ),
                    is_error: true,
                });
            }
        };

        // Write JSON input to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(input_json.as_bytes()).await;
            drop(stdin);
        }

        // Capture stdout/stderr handles before wait
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // Wait with timeout (30 seconds)
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await;

        match timeout {
            Ok(Ok(status)) => {
                use tokio::io::AsyncReadExt;
                let mut stdout_buf = String::new();
                let mut stderr_buf = String::new();
                if let Some(mut h) = stdout_handle {
                    let _ = h.read_to_string(&mut stdout_buf).await;
                }
                if let Some(mut h) = stderr_handle {
                    let _ = h.read_to_string(&mut stderr_buf).await;
                }

                if status.success() {
                    Ok(ToolOutput {
                        content: if stdout_buf.is_empty() {
                            "(no output)".to_string()
                        } else {
                            stdout_buf
                        },
                        is_error: false,
                    })
                } else {
                    Ok(ToolOutput {
                        content: format!(
                            "Script exited with code {}.\nstdout: {}\nstderr: {}",
                            status.code().unwrap_or(-1),
                            stdout_buf,
                            stderr_buf
                        ),
                        is_error: true,
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolOutput {
                content: format!("Script execution error: {}", e),
                is_error: true,
            }),
            Err(_) => {
                let _ = child.kill().await;
                Ok(ToolOutput {
                    content: "Script timed out after 30 seconds.".to_string(),
                    is_error: true,
                })
            }
        }
    }
}

// ── Custom Tool Registry ────────────────────────────────────────────────────

/// Manages custom script tools — loading, creating, change detection.
pub struct CustomToolRegistry {
    tools_dir: PathBuf,
    tools_changed: AtomicBool,
}

impl CustomToolRegistry {
    pub fn new() -> Self {
        let tools_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e")
            .join("custom-tools");
        Self {
            tools_dir,
            tools_changed: AtomicBool::new(false),
        }
    }

    /// Load all custom tools from `~/.temm1e/custom-tools/`.
    pub fn load_tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        if !self.tools_dir.exists() {
            return tools;
        }

        let entries = match std::fs::read_dir(&self.tools_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "Cannot read custom tools directory");
                return tools;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                match ScriptToolAdapter::from_meta_file(&path) {
                    Ok(tool) => {
                        info!(tool = %tool.meta.name, "Loaded custom tool");
                        tools.push(Arc::new(tool));
                    }
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to load custom tool"
                        );
                    }
                }
            }
        }

        if !tools.is_empty() {
            info!(count = tools.len(), "Custom tools loaded");
        }

        tools
    }

    /// Create a new custom tool: write script + metadata, mark as changed.
    pub fn create_tool(
        &self,
        name: &str,
        description: &str,
        language: &str,
        script_content: &str,
        parameters: serde_json::Value,
    ) -> Result<String, Temm1eError> {
        // Validate name
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Temm1eError::Tool(
                "Tool name must be non-empty and contain only [a-zA-Z0-9_-]".to_string(),
            ));
        }

        // Validate language
        let ext = match language {
            "bash" | "sh" => "sh",
            "python" | "py" => "py",
            "node" | "js" | "javascript" => "js",
            other => {
                return Err(Temm1eError::Tool(format!(
                    "Unsupported language '{}'. Use: bash, python, or node.",
                    other
                )));
            }
        };

        // Normalize language name
        let lang = match ext {
            "py" => "python",
            "js" => "node",
            _ => "bash",
        };

        if script_content.trim().is_empty() {
            return Err(Temm1eError::Tool(
                "Script content cannot be empty.".to_string(),
            ));
        }

        // Create directory
        std::fs::create_dir_all(&self.tools_dir).map_err(|e| {
            Temm1eError::Tool(format!(
                "Cannot create custom tools directory {}: {}",
                self.tools_dir.display(),
                e
            ))
        })?;

        // Write script file
        let script_path = self.tools_dir.join(format!("{}.{}", name, ext));
        std::fs::write(&script_path, script_content).map_err(|e| {
            Temm1eError::Tool(format!(
                "Cannot write script {}: {}",
                script_path.display(),
                e
            ))
        })?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
        }

        // Write metadata
        let meta = ScriptToolMeta {
            name: name.to_string(),
            description: description.to_string(),
            language: lang.to_string(),
            parameters,
        };
        let meta_path = self.tools_dir.join(format!("{}.json", name));
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| Temm1eError::Tool(format!("Cannot serialize tool metadata: {}", e)))?;
        std::fs::write(&meta_path, meta_json).map_err(|e| {
            Temm1eError::Tool(format!(
                "Cannot write metadata {}: {}",
                meta_path.display(),
                e
            ))
        })?;

        // Signal tools changed
        self.tools_changed.store(true, Ordering::Relaxed);

        info!(tool = %name, language = %lang, "Custom tool created");

        Ok(format!(
            "Tool '{}' created successfully at {}.\n\
             Script: {}\n\
             The tool is now available — use it in your next action.",
            name,
            meta_path.display(),
            script_path.display()
        ))
    }

    /// List all custom tools (name + description).
    pub fn list_tools(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        if !self.tools_dir.exists() {
            return result;
        }
        if let Ok(entries) = std::fs::read_dir(&self.tools_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(meta) = serde_json::from_str::<ScriptToolMeta>(&content) {
                            result.push((meta.name, meta.description));
                        }
                    }
                }
            }
        }
        result
    }

    /// Delete a custom tool by name.
    pub fn delete_tool(&self, name: &str) -> Result<String, Temm1eError> {
        let meta_path = self.tools_dir.join(format!("{}.json", name));
        if !meta_path.exists() {
            return Err(Temm1eError::Tool(format!(
                "Custom tool '{}' not found.",
                name
            )));
        }

        // Read metadata to find script extension
        if let Ok(content) = std::fs::read_to_string(&meta_path) {
            if let Ok(meta) = serde_json::from_str::<ScriptToolMeta>(&content) {
                let ext = match meta.language.as_str() {
                    "python" => "py",
                    "node" => "js",
                    _ => "sh",
                };
                let script_path = self.tools_dir.join(format!("{}.{}", name, ext));
                let _ = std::fs::remove_file(script_path);
            }
        }

        let _ = std::fs::remove_file(&meta_path);
        self.tools_changed.store(true, Ordering::Relaxed);

        info!(tool = %name, "Custom tool deleted");
        Ok(format!("Custom tool '{}' deleted.", name))
    }

    /// Check and clear the tools_changed flag.
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::Relaxed)
    }
}

impl Default for CustomToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── SelfCreateTool ──────────────────────────────────────────────────────────

/// Agent tool for creating, listing, and deleting custom script tools.
pub struct SelfCreateTool {
    registry: Arc<CustomToolRegistry>,
}

impl SelfCreateTool {
    pub fn new(registry: Arc<CustomToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SelfCreateTool {
    fn name(&self) -> &str {
        "self_create_tool"
    }

    fn description(&self) -> &str {
        "Create, list, or delete custom script tools. Created tools persist across sessions. \
         To create: provide name, description, language (bash/python/node), script content, and \
         a JSON Schema for parameters. The script receives input as JSON via stdin and should \
         write its output to stdout. To list: set action to 'list'. To delete: set action to \
         'delete' and provide the name."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "delete"],
                    "description": "Action to perform: 'create' a new tool, 'list' existing tools, or 'delete' one"
                },
                "name": {
                    "type": "string",
                    "description": "Tool name (alphanumeric + underscore/hyphen, e.g., 'pdf_to_text')"
                },
                "description": {
                    "type": "string",
                    "description": "What the tool does (shown to the LLM)"
                },
                "language": {
                    "type": "string",
                    "enum": ["bash", "python", "node"],
                    "description": "Script language"
                },
                "script": {
                    "type": "string",
                    "description": "The script source code. Receives JSON input via stdin, outputs result to stdout."
                },
                "parameters": {
                    "type": "object",
                    "description": "JSON Schema for the tool's input parameters"
                }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec![],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("create");

        match action {
            "list" => {
                let tools = self.registry.list_tools();
                if tools.is_empty() {
                    Ok(ToolOutput {
                        content: "No custom tools created yet. Use action 'create' to make one."
                            .to_string(),
                        is_error: false,
                    })
                } else {
                    let mut output = format!("{} custom tool(s):\n\n", tools.len());
                    for (name, desc) in &tools {
                        output.push_str(&format!("  - {} — {}\n", name, desc));
                    }
                    Ok(ToolOutput {
                        content: output,
                        is_error: false,
                    })
                }
            }
            "delete" => {
                let name = input
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if name.is_empty() {
                    return Ok(ToolOutput {
                        content: "Missing 'name' — which tool to delete?".to_string(),
                        is_error: true,
                    });
                }
                match self.registry.delete_tool(name) {
                    Ok(msg) => Ok(ToolOutput {
                        content: msg,
                        is_error: false,
                    }),
                    Err(e) => Ok(ToolOutput {
                        content: format!("{}", e),
                        is_error: true,
                    }),
                }
            }
            _ => {
                let name = input
                    .arguments
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let description = input
                    .arguments
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let language = input
                    .arguments
                    .get("language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("bash");
                let script = input
                    .arguments
                    .get("script")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let parameters = input
                    .arguments
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "type": "object",
                            "properties": {}
                        })
                    });

                if name.is_empty() || description.is_empty() || script.is_empty() {
                    return Ok(ToolOutput {
                        content: "Missing required fields. Provide: name, description, script, \
                                  and optionally language (default: bash) and parameters (JSON Schema)."
                            .to_string(),
                        is_error: true,
                    });
                }

                match self
                    .registry
                    .create_tool(name, description, language, script, parameters)
                {
                    Ok(msg) => Ok(ToolOutput {
                        content: msg,
                        is_error: false,
                    }),
                    Err(e) => Ok(ToolOutput {
                        content: format!("{}", e),
                        is_error: true,
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_load_tool() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let result = registry.create_tool(
            "hello",
            "Says hello",
            "bash",
            "#!/bin/bash\necho \"Hello from custom tool!\"",
            serde_json::json!({"type": "object", "properties": {}}),
        );
        assert!(result.is_ok());

        let tools = registry.load_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "hello");
        assert!(registry.take_tools_changed());
    }

    #[test]
    fn invalid_tool_name() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let result = registry.create_tool(
            "bad name!",
            "desc",
            "bash",
            "echo hi",
            serde_json::json!({}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn empty_script_rejected() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let result = registry.create_tool("test", "desc", "bash", "   ", serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn delete_tool() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        registry
            .create_tool(
                "temp",
                "Temp tool",
                "bash",
                "echo temp",
                serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(registry.load_tools().len(), 1);

        registry.delete_tool("temp").unwrap();
        assert_eq!(registry.load_tools().len(), 0);
    }

    #[test]
    fn delete_nonexistent() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let result = registry.delete_tool("nope");
        assert!(result.is_err());
    }

    #[test]
    fn list_empty() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let tools = registry.list_tools();
        assert!(tools.is_empty());
    }

    #[test]
    fn unsupported_language() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        let result =
            registry.create_tool("test", "desc", "ruby", "puts 'hi'", serde_json::json!({}));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_bash_script() {
        let dir = tempdir().unwrap();
        let registry = CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        };

        registry
            .create_tool(
                "greet",
                "Says hello",
                "bash",
                "#!/bin/bash\nread input\necho \"Hello from script!\"",
                serde_json::json!({"type": "object", "properties": {}}),
            )
            .unwrap();

        let tools = registry.load_tools();
        assert_eq!(tools.len(), 1);

        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "greet".to_string(),
            arguments: serde_json::json!({}),
        };
        let output = tools[0].execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Hello from script!"));
    }

    #[tokio::test]
    async fn self_create_tool_execute() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(CustomToolRegistry {
            tools_dir: dir.path().to_path_buf(),
            tools_changed: AtomicBool::new(false),
        });

        let tool = SelfCreateTool::new(registry.clone());
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };

        // Create
        let input = ToolInput {
            name: "self_create_tool".to_string(),
            arguments: serde_json::json!({
                "action": "create",
                "name": "echo_tool",
                "description": "Echoes input back",
                "language": "bash",
                "script": "#!/bin/bash\ncat",
                "parameters": {"type": "object", "properties": {"text": {"type": "string"}}}
            }),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("created successfully"));

        // List
        let input = ToolInput {
            name: "self_create_tool".to_string(),
            arguments: serde_json::json!({"action": "list"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("echo_tool"));

        // Delete
        let input = ToolInput {
            name: "self_create_tool".to_string(),
            arguments: serde_json::json!({"action": "delete", "name": "echo_tool"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("deleted"));
    }

    #[test]
    fn parameters_schema_valid() {
        let registry = Arc::new(CustomToolRegistry::new());
        let tool = SelfCreateTool::new(registry);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["name"].is_object());
        assert!(schema["properties"]["script"].is_object());
    }
}
