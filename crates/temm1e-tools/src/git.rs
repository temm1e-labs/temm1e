//! Git tool — typed git operations via tokio::process::Command.

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// Default command timeout in seconds (git ops can be slow).
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Maximum allowed timeout in seconds.
const MAX_TIMEOUT_SECS: u64 = 300;

/// Maximum output size returned to the model (32 KB).
const MAX_OUTPUT_SIZE: usize = 32 * 1024;

/// Valid git actions supported by this tool.
const VALID_ACTIONS: &[&str] = &[
    "clone", "pull", "push", "commit", "branch", "diff", "log", "status", "checkout", "add",
];

#[derive(Default)]
pub struct GitTool;

impl GitTool {
    pub fn new() -> Self {
        Self
    }

    /// Build command arguments for the given action and input arguments.
    /// Returns the git subcommand arguments or an error.
    fn build_args(action: &str, args: &serde_json::Value) -> Result<Vec<String>, Temm1eError> {
        let mut cmd_args: Vec<String> = vec![action.to_string()];

        match action {
            "clone" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Temm1eError::Tool("clone requires 'url' parameter".into()))?;
                cmd_args.push(url.to_string());

                if let Some(dir) = args.get("directory").and_then(|v| v.as_str()) {
                    cmd_args.push(dir.to_string());
                }
            }
            "pull" => {
                if let Some(remote) = args.get("remote").and_then(|v| v.as_str()) {
                    cmd_args.push(remote.to_string());
                }
                if let Some(branch) = args.get("branch").and_then(|v| v.as_str()) {
                    cmd_args.push(branch.to_string());
                }
            }
            "push" => {
                let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                let force_allowed = args
                    .get("force_allowed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let remote = args
                    .get("remote")
                    .and_then(|v| v.as_str())
                    .unwrap_or("origin");
                let branch = args.get("branch").and_then(|v| v.as_str());

                if force {
                    // Block force push to main/master unless explicitly allowed
                    let target = branch.unwrap_or("");
                    let is_protected = target.is_empty() || target == "main" || target == "master";

                    if is_protected && !force_allowed {
                        return Err(Temm1eError::Tool(
                            "Force push to main/master is blocked. Set force_allowed: true to override."
                                .into(),
                        ));
                    }
                    cmd_args.push("--force".to_string());
                }

                cmd_args.push(remote.to_string());
                if let Some(b) = branch {
                    cmd_args.push(b.to_string());
                }
            }
            "commit" => {
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("commit requires 'message' parameter".into())
                    })?;
                cmd_args.push("-m".to_string());
                cmd_args.push(message.to_string());

                if let Some(files) = args.get("files").and_then(|v| v.as_array()) {
                    // Use -- to separate paths
                    cmd_args.push("--".to_string());
                    for f in files {
                        if let Some(s) = f.as_str() {
                            cmd_args.push(s.to_string());
                        }
                    }
                }
            }
            "branch" => {
                let delete = args
                    .get("delete")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if let Some(name) = args.get("name").and_then(|v| v.as_str()) {
                    if delete {
                        cmd_args.push("-d".to_string());
                    }
                    cmd_args.push(name.to_string());
                }
                // No name = list branches (git branch)
            }
            "checkout" => {
                let branch = args.get("branch").and_then(|v| v.as_str()).ok_or_else(|| {
                    Temm1eError::Tool("checkout requires 'branch' parameter".into())
                })?;

                let create = args
                    .get("create")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if create {
                    cmd_args.push("-b".to_string());
                }
                cmd_args.push(branch.to_string());
            }
            "add" => {
                let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

                if all {
                    cmd_args.push("-A".to_string());
                } else {
                    let files = args
                        .get("files")
                        .and_then(|v| v.as_array())
                        .ok_or_else(|| {
                            Temm1eError::Tool("add requires 'files' array or 'all: true'".into())
                        })?;

                    if files.is_empty() {
                        return Err(Temm1eError::Tool(
                            "add requires at least one file in 'files' array".into(),
                        ));
                    }

                    for f in files {
                        if let Some(s) = f.as_str() {
                            cmd_args.push(s.to_string());
                        }
                    }
                }
            }
            "diff" | "log" | "status" => {
                if let Some(extra) = args.get("args").and_then(|v| v.as_array()) {
                    for a in extra {
                        if let Some(s) = a.as_str() {
                            // Block reset --hard smuggled through extra args
                            if action == "status" || action == "diff" || action == "log" {
                                cmd_args.push(s.to_string());
                            }
                        }
                    }
                }
            }
            _ => {
                return Err(Temm1eError::Tool(format!(
                    "Unknown git action '{}'. Valid actions: {}",
                    action,
                    VALID_ACTIONS.join(", ")
                )));
            }
        }

        Ok(cmd_args)
    }

    /// Check for dangerous operations that should be blocked.
    fn validate_safety(action: &str, args: &serde_json::Value) -> Result<(), Temm1eError> {
        // Block reset --hard unless explicitly allowed
        if action == "checkout" {
            // Checkout is fine, it's just branch switching
        }

        // General check: scan for reset --hard in any extra args
        if let Some(extra) = args.get("args").and_then(|v| v.as_array()) {
            for a in extra {
                if let Some(s) = a.as_str() {
                    if s == "--hard" {
                        return Err(Temm1eError::Tool(
                            "reset --hard is blocked for safety. Use the shell tool directly if needed."
                                .into(),
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn description(&self) -> &str {
        "Execute typed git operations in the workspace. Available actions: \
         clone (url, directory), pull (remote, branch), push (remote, branch, force, force_allowed), \
         commit (message, files), branch (name, delete), checkout (branch, create), \
         add (files, all), diff (args), log (args), status (args). \
         Force push to main/master is blocked by default. \
         All operations run in the session workspace directory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Git action to perform: clone, pull, push, commit, branch, diff, log, status, checkout, add",
                    "enum": VALID_ACTIONS
                },
                "args": {
                    "type": "object",
                    "description": "Action-specific arguments. clone: {url, directory?}. pull: {remote?, branch?}. push: {remote?, branch?, force?, force_allowed?}. commit: {message, files?}. branch: {name?, delete?}. checkout: {branch, create?}. add: {files?, all?}. diff/log/status: {args?: string[]}."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 60, max: 300)"
                }
            },
            "required": ["action"]
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
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: action".into()))?;

        if !VALID_ACTIONS.contains(&action) {
            return Err(Temm1eError::Tool(format!(
                "Unknown git action '{}'. Valid actions: {}",
                action,
                VALID_ACTIONS.join(", ")
            )));
        }

        let args = input
            .arguments
            .get("args")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        let timeout_secs = input
            .arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        // Safety checks
        Self::validate_safety(action, &args)?;

        // Build git command arguments
        let cmd_args = Self::build_args(action, &args)?;

        tracing::info!(
            action = %action,
            timeout = timeout_secs,
            "Executing git command"
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new("git")
                .args(&cmd_args)
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
                        "git {} completed with exit code {}",
                        action,
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
                content: format!("Failed to execute git command: {}", e),
                is_error: true,
            }),
            Err(_) => Ok(ToolOutput {
                content: format!("git {} timed out after {} seconds", action, timeout_secs),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let tool = GitTool::new();
        assert_eq!(tool.name(), "git");
    }

    #[test]
    fn test_parameters_schema_valid_json() {
        let tool = GitTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["args"].is_object());
        assert!(schema["properties"]["timeout"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn test_declarations_has_shell_access() {
        let tool = GitTool::new();
        let decl = tool.declarations();
        assert!(decl.shell_access);
        assert!(decl.file_access.is_empty());
        assert!(decl.network_access.is_empty());
    }

    #[test]
    fn test_force_push_to_main_blocked() {
        let args = serde_json::json!({
            "force": true,
            "branch": "main"
        });
        let result = GitTool::build_args("push", &args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Force push"));
        assert!(err.contains("main/master"));
    }

    #[test]
    fn test_force_push_to_master_blocked() {
        let args = serde_json::json!({
            "force": true,
            "branch": "master"
        });
        let result = GitTool::build_args("push", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_force_push_no_branch_blocked() {
        // Empty branch defaults to protected (could be main/master)
        let args = serde_json::json!({
            "force": true
        });
        let result = GitTool::build_args("push", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_force_push_to_feature_branch_with_force_allowed() {
        let args = serde_json::json!({
            "force": true,
            "force_allowed": true,
            "branch": "feature-x"
        });
        let result = GitTool::build_args("push", &args);
        assert!(result.is_ok());
        let cmd_args = result.unwrap();
        assert!(cmd_args.contains(&"--force".to_string()));
        assert!(cmd_args.contains(&"feature-x".to_string()));
    }

    #[test]
    fn test_force_push_to_main_with_force_allowed() {
        let args = serde_json::json!({
            "force": true,
            "force_allowed": true,
            "branch": "main"
        });
        let result = GitTool::build_args("push", &args);
        assert!(result.is_ok());
    }

    #[test]
    fn test_commit_builds_correct_command() {
        let args = serde_json::json!({
            "message": "fix: resolve bug",
            "files": ["src/main.rs", "Cargo.toml"]
        });
        let result = GitTool::build_args("commit", &args).unwrap();
        assert_eq!(result[0], "commit");
        assert_eq!(result[1], "-m");
        assert_eq!(result[2], "fix: resolve bug");
        assert_eq!(result[3], "--");
        assert_eq!(result[4], "src/main.rs");
        assert_eq!(result[5], "Cargo.toml");
    }

    #[test]
    fn test_commit_requires_message() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("commit", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_clone_requires_url() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("clone", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_clone_with_directory() {
        let args = serde_json::json!({
            "url": "https://github.com/example/repo.git",
            "directory": "my-repo"
        });
        let result = GitTool::build_args("clone", &args).unwrap();
        assert_eq!(result[0], "clone");
        assert_eq!(result[1], "https://github.com/example/repo.git");
        assert_eq!(result[2], "my-repo");
    }

    #[test]
    fn test_checkout_requires_branch() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("checkout", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_checkout_with_create() {
        let args = serde_json::json!({
            "branch": "feature-new",
            "create": true
        });
        let result = GitTool::build_args("checkout", &args).unwrap();
        assert_eq!(result[0], "checkout");
        assert_eq!(result[1], "-b");
        assert_eq!(result[2], "feature-new");
    }

    #[test]
    fn test_add_requires_files_or_all() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("add", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_all() {
        let args = serde_json::json!({ "all": true });
        let result = GitTool::build_args("add", &args).unwrap();
        assert_eq!(result[0], "add");
        assert_eq!(result[1], "-A");
    }

    #[test]
    fn test_add_files() {
        let args = serde_json::json!({ "files": ["a.rs", "b.rs"] });
        let result = GitTool::build_args("add", &args).unwrap();
        assert_eq!(result[0], "add");
        assert_eq!(result[1], "a.rs");
        assert_eq!(result[2], "b.rs");
    }

    #[test]
    fn test_branch_list() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("branch", &args).unwrap();
        assert_eq!(result, vec!["branch"]);
    }

    #[test]
    fn test_branch_create() {
        let args = serde_json::json!({ "name": "feat-x" });
        let result = GitTool::build_args("branch", &args).unwrap();
        assert_eq!(result, vec!["branch", "feat-x"]);
    }

    #[test]
    fn test_branch_delete() {
        let args = serde_json::json!({ "name": "old-branch", "delete": true });
        let result = GitTool::build_args("branch", &args).unwrap();
        assert_eq!(result, vec!["branch", "-d", "old-branch"]);
    }

    #[test]
    fn test_status_with_extra_args() {
        let args = serde_json::json!({ "args": ["--short"] });
        let result = GitTool::build_args("status", &args).unwrap();
        assert_eq!(result, vec!["status", "--short"]);
    }

    #[test]
    fn test_unknown_action_rejected() {
        let args = serde_json::json!({});
        let result = GitTool::build_args("rebase", &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_reset_hard_blocked_via_safety() {
        let args = serde_json::json!({ "args": ["--hard"] });
        let result = GitTool::validate_safety("status", &args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("reset --hard"));
    }

    #[test]
    fn test_push_without_force_allowed() {
        let args = serde_json::json!({
            "remote": "origin",
            "branch": "feature-x"
        });
        let result = GitTool::build_args("push", &args).unwrap();
        assert_eq!(result, vec!["push", "origin", "feature-x"]);
        assert!(!result.contains(&"--force".to_string()));
    }

    #[test]
    fn test_pull_with_remote_and_branch() {
        let args = serde_json::json!({
            "remote": "upstream",
            "branch": "develop"
        });
        let result = GitTool::build_args("pull", &args).unwrap();
        assert_eq!(result, vec!["pull", "upstream", "develop"]);
    }

    #[tokio::test]
    async fn test_execute_missing_action() {
        let tool = GitTool::new();
        let input = ToolInput {
            name: "git".to_string(),
            arguments: serde_json::json!({}),
        };
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_invalid_action() {
        let tool = GitTool::new();
        let input = ToolInput {
            name: "git".to_string(),
            arguments: serde_json::json!({ "action": "rebase" }),
        };
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }
}
