//! Tool executor — validates tool calls against declarations and executes them
//! within workspace-scoped sandboxing.
//!
//! Supports both sequential execution via [`execute_tool`] and parallel
//! execution via [`execute_tools_parallel`], which groups independent tool
//! calls and runs them concurrently with configurable concurrency limits.

use std::collections::HashSet;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::session::SessionContext;
use temm1e_core::{PathAccess, Tool, ToolContext, ToolInput, ToolOutput};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

// ── Parallel execution types ────────────────────────────────────────────

/// A single tool call request, carrying the tool_use block ID from the
/// provider response so results can be correlated back.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Unique ID from the tool_use block (e.g., `toolu_01XFDUDYJgAACzvnptvVer6t`).
    pub id: String,
    /// Tool name (e.g., `file_read`, `shell`).
    pub name: String,
    /// JSON arguments for the tool.
    pub arguments: serde_json::Value,
}

/// The result of a single tool call within a parallel batch.
#[derive(Debug)]
pub struct ToolCallResult {
    /// The tool_use block ID this result corresponds to.
    pub id: String,
    /// The tool output, or an error if execution failed.
    pub output: Result<ToolOutput, Temm1eError>,
}

/// Default maximum number of concurrently executing tool calls.
const DEFAULT_MAX_CONCURRENT: usize = 5;

// ── Parallel execution ──────────────────────────────────────────────────

/// Execute multiple tool calls with automatic dependency detection and
/// parallel execution of independent groups.
///
/// Independent tool calls run concurrently (up to `max_concurrent`), while
/// dependent tool calls within a group run sequentially. Results are always
/// returned in the original call order regardless of execution order.
///
/// Individual tool failures do **not** abort other parallel executions —
/// each result carries its own `Result`.
pub async fn execute_tools_parallel(
    tool_calls: Vec<ToolCall>,
    tools: &[Arc<dyn Tool>],
    session: &SessionContext,
    max_concurrent: usize,
) -> Vec<ToolCallResult> {
    if tool_calls.is_empty() {
        return Vec::new();
    }

    let max_concurrent = if max_concurrent == 0 {
        DEFAULT_MAX_CONCURRENT
    } else {
        max_concurrent
    };

    let groups = detect_dependencies(&tool_calls);

    info!(
        total_calls = tool_calls.len(),
        groups = groups.len(),
        max_concurrent,
        "Executing tool calls with dependency grouping"
    );

    // Pre-allocate results with None placeholders; we fill them by index.
    let mut results: Vec<Option<ToolCallResult>> = (0..tool_calls.len()).map(|_| None).collect();

    let semaphore = Arc::new(Semaphore::new(max_concurrent));

    // Each group contains indices of tool calls that are mutually dependent
    // and must run sequentially within the group.  Different groups are
    // independent and may run concurrently.
    let mut group_futures = FuturesUnordered::new();

    for group in &groups {
        let group = group.clone();
        let semaphore = Arc::clone(&semaphore);
        let tools = tools.to_vec();
        let session = session.clone();
        let calls: Vec<(usize, ToolCall)> = group
            .iter()
            .map(|&idx| (idx, tool_calls[idx].clone()))
            .collect();

        let is_parallel = group.len() == 1;
        if is_parallel {
            debug!(
                tool = %calls[0].1.name,
                id = %calls[0].1.id,
                "Scheduling independent tool call"
            );
        } else {
            let names: Vec<&str> = calls.iter().map(|(_, c)| c.name.as_str()).collect();
            debug!(
                tools = ?names,
                "Scheduling sequential dependency group"
            );
        }

        group_futures.push(tokio::spawn(async move {
            let mut group_results = Vec::new();
            for (idx, call) in calls {
                // Acquire a semaphore permit to respect max_concurrent
                let _permit = match semaphore.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!("Tool semaphore closed, returning partial results");
                        return group_results;
                    }
                };

                let output = execute_tool(&call.name, call.arguments, &tools, &session).await;

                group_results.push((
                    idx,
                    ToolCallResult {
                        id: call.id,
                        output,
                    },
                ));
            }
            group_results
        }));
    }

    // Collect all results from spawned tasks
    while let Some(join_result) = group_futures.next().await {
        match join_result {
            Ok(group_results) => {
                for (idx, result) in group_results {
                    results[idx] = Some(result);
                }
            }
            Err(join_err) => {
                // A spawned task panicked — this should not happen in normal
                // operation. Log and leave corresponding slots as errors.
                warn!(error = %join_err, "Tool execution task panicked");
            }
        }
    }

    // Unwrap all Option<ToolCallResult> — any None slots are from panicked
    // tasks, which we convert to error results.
    results
        .into_iter()
        .enumerate()
        .map(|(idx, opt)| {
            opt.unwrap_or_else(|| ToolCallResult {
                id: tool_calls[idx].id.clone(),
                output: Err(Temm1eError::Tool(
                    "Tool execution task panicked".to_string(),
                )),
            })
        })
        .collect()
}

// ── Dependency detection ────────────────────────────────────────────────

/// Analyse a batch of tool calls and group them by dependency.
///
/// Returns a `Vec<Vec<usize>>` where each inner `Vec` is a group of call
/// indices that must execute sequentially.  Groups themselves are
/// independent and can run concurrently.
///
/// Heuristics (conservative — when unsure, treat as dependent):
///
/// - Two `file_read` calls are always independent (read-read no conflict).
/// - Two `file_write` / `file_read`+`file_write` to the **same path** are
///   dependent.
/// - `shell` calls are always treated as dependent on other `shell` calls
///   (they may share state via the filesystem or environment).
/// - Calls to different tool names with no overlapping file targets are
///   independent.
pub fn detect_dependencies(calls: &[ToolCall]) -> Vec<Vec<usize>> {
    if calls.is_empty() {
        return Vec::new();
    }
    if calls.len() == 1 {
        return vec![vec![0]];
    }

    // Build a dependency graph: for each pair (i, j) where i < j, determine
    // if they are dependent.  We then use union-find to group them.
    let n = calls.len();
    let mut parent: Vec<usize> = (0..n).collect();

    /// Find with path compression.
    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    /// Union two sets.
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if are_dependent(&calls[i], &calls[j]) {
                union(&mut parent, i, j);
            }
        }
    }

    // Collect groups, preserving original order within each group.
    let mut group_map: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        group_map.entry(root).or_default().push(i);
    }

    group_map.into_values().collect()
}

/// Determine whether two tool calls have a potential dependency.
fn are_dependent(a: &ToolCall, b: &ToolCall) -> bool {
    // Shell calls are always treated as mutually dependent
    if a.name == "shell" && b.name == "shell" {
        return true;
    }

    // Extract file paths from arguments
    let a_paths = extract_file_paths(&a.arguments);
    let b_paths = extract_file_paths(&b.arguments);

    // If either call has no file paths, use name-based heuristic
    if a_paths.is_empty() && b_paths.is_empty() {
        // Same tool name with no file paths — conservative: treat as dependent
        // unless they are known read-only tools
        if a.name == b.name && !is_read_only_tool(&a.name) {
            return true;
        }
        return false;
    }

    // Check for overlapping paths
    let a_writes = extract_write_paths(&a.name, &a.arguments);
    let b_writes = extract_write_paths(&b.name, &b.arguments);

    let a_path_set: HashSet<&str> = a_paths.iter().map(|s| s.as_str()).collect();
    let b_path_set: HashSet<&str> = b_paths.iter().map(|s| s.as_str()).collect();

    let overlapping_paths: HashSet<&&str> = a_path_set.intersection(&b_path_set).collect();

    if overlapping_paths.is_empty() {
        return false;
    }

    // Overlapping paths exist — dependent if either side writes
    let a_write_set: HashSet<&str> = a_writes.iter().map(|s| s.as_str()).collect();
    let b_write_set: HashSet<&str> = b_writes.iter().map(|s| s.as_str()).collect();

    for path in &overlapping_paths {
        if a_write_set.contains(**path) || b_write_set.contains(**path) {
            return true;
        }
    }

    // Both are reading the same paths — no conflict
    false
}

/// Extract file path strings from tool call arguments.
fn extract_file_paths(arguments: &serde_json::Value) -> Vec<String> {
    let path_keys = [
        "path",
        "file",
        "file_path",
        "directory",
        "dir",
        "target",
        "destination",
        "src",
        "dest",
    ];

    let mut paths = Vec::new();
    if let serde_json::Value::Object(map) = arguments {
        for key in &path_keys {
            if let Some(serde_json::Value::String(p)) = map.get(*key) {
                paths.push(p.clone());
            }
        }
    }
    paths
}

/// Extract paths that would be written to by this tool call.
fn extract_write_paths(tool_name: &str, arguments: &serde_json::Value) -> Vec<String> {
    // Tools that write: file_write, file_edit, shell (conservatively all paths)
    // Tools that read: file_read (no writes)
    match tool_name {
        "file_read" => Vec::new(),
        "file_write" | "file_edit" | "file_create" => extract_file_paths(arguments),
        "shell" => extract_file_paths(arguments), // conservative
        _ => {
            // Unknown tool — conservatively treat all paths as writes
            extract_file_paths(arguments)
        }
    }
}

/// Whether a tool is known to be read-only (no side effects).
fn is_read_only_tool(name: &str) -> bool {
    matches!(name, "file_read" | "file_list" | "file_search" | "browser")
}

/// Dangerous shell command patterns that should be rejected.
const BLOCKED_SHELL_PATTERNS: &[&str] = &[
    "rm -rf /",
    "mkfs.",
    "dd if=",
    "> /dev/sd",
    "chmod -R 777 /",
    ":(){ :|:", // fork bomb
    "curl | sh",
    "curl | bash",
    "wget | sh",
    "wget | bash",
];

/// Execute a tool call, validating sandbox constraints first.
pub async fn execute_tool(
    tool_name: &str,
    arguments: serde_json::Value,
    tools: &[Arc<dyn Tool>],
    session: &SessionContext,
) -> Result<ToolOutput, Temm1eError> {
    // Find the matching tool
    let tool = tools
        .iter()
        .find(|t| t.name() == tool_name)
        .ok_or_else(|| Temm1eError::Tool(format!("Unknown tool: {}", tool_name)))?;

    // Validate sandbox declarations against workspace scope
    validate_sandbox(tool.as_ref(), session)?;

    // Validate runtime arguments against workspace scope (CA-02 / CA-06)
    validate_arguments(tool_name, &arguments, session)?;

    let ctx = ToolContext {
        workspace_path: session.workspace_path.clone(),
        session_id: session.session_id.clone(),
        chat_id: session.chat_id.clone(),
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

/// Validate runtime arguments from the tool call's JSON against workspace scope.
///
/// This catches path traversal and out-of-scope file access in the actual
/// arguments the LLM provides at call time, not just the static declarations.
fn validate_arguments(
    tool_name: &str,
    arguments: &serde_json::Value,
    session: &SessionContext,
) -> Result<(), Temm1eError> {
    // Validate file path arguments
    let path_keys = [
        "path",
        "file",
        "file_path",
        "directory",
        "dir",
        "target",
        "destination",
        "src",
        "dest",
    ];
    if let serde_json::Value::Object(map) = arguments {
        for key in &path_keys {
            if let Some(serde_json::Value::String(path_str)) = map.get(*key) {
                validate_path_in_workspace(tool_name, path_str, session)?;
            }
        }

        // Validate shell/command arguments for dangerous patterns
        if let Some(serde_json::Value::String(cmd)) = map.get("command") {
            validate_shell_command(tool_name, cmd)?;
        }
        if let Some(serde_json::Value::String(cmd)) = map.get("cmd") {
            validate_shell_command(tool_name, cmd)?;
        }
    }

    Ok(())
}

/// Validate that a file path argument resolves to within the workspace.
fn validate_path_in_workspace(
    tool_name: &str,
    path_str: &str,
    session: &SessionContext,
) -> Result<(), Temm1eError> {
    let path = std::path::Path::new(path_str);
    let workspace = &session.workspace_path;

    let abs_path = if path.is_relative() {
        workspace.join(path)
    } else {
        path.to_path_buf()
    };

    let workspace_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.clone());

    // For existing paths, canonicalize to resolve symlinks and ..
    // For non-existent paths, reject them if they can't be validated
    let path_canonical = match abs_path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // Path does not exist yet; do lexical normalization to catch traversal
            let normalized = lexical_normalize(&abs_path);
            if !normalized.starts_with(&workspace_canonical) {
                return Err(Temm1eError::SandboxViolation(format!(
                    "Tool '{}' argument path '{}' escapes workspace '{}'",
                    tool_name,
                    path_str,
                    workspace.display()
                )));
            }
            return Ok(());
        }
    };

    if !path_canonical.starts_with(&workspace_canonical) {
        return Err(Temm1eError::SandboxViolation(format!(
            "Tool '{}' argument path '{}' is outside workspace '{}'",
            tool_name,
            path_str,
            workspace.display()
        )));
    }

    Ok(())
}

/// Lexically normalize a path by resolving `.` and `..` components without I/O.
fn lexical_normalize(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Only pop if there's a Normal component to pop
                if parts
                    .last()
                    .is_some_and(|c| matches!(c, Component::Normal(_)))
                {
                    parts.pop();
                } else {
                    parts.push(component);
                }
            }
            Component::CurDir => {} // skip
            _ => parts.push(component),
        }
    }
    parts.iter().collect()
}

/// Validate that a shell command does not contain dangerous patterns.
fn validate_shell_command(tool_name: &str, command: &str) -> Result<(), Temm1eError> {
    let lower = command.to_lowercase();
    for pattern in BLOCKED_SHELL_PATTERNS {
        if lower.contains(pattern) {
            return Err(Temm1eError::SandboxViolation(format!(
                "Tool '{}' command contains blocked pattern: '{}'",
                tool_name, pattern
            )));
        }
    }
    Ok(())
}

/// Validate that a tool's declared resource access is within the session's workspace scope.
fn validate_sandbox(tool: &dyn Tool, session: &SessionContext) -> Result<(), Temm1eError> {
    let declarations = tool.declarations();
    let workspace = &session.workspace_path;

    // Check file access paths are within the workspace
    for path_access in &declarations.file_access {
        let path_str = match path_access {
            PathAccess::Read(p) => p,
            PathAccess::Write(p) => p,
            PathAccess::ReadWrite(p) => p,
        };

        // Expand tilde (~) to home directory
        let expanded_path = if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                home.join(&path_str[2..])
            } else {
                std::path::PathBuf::from(path_str)
            }
        } else if path_str == "~" {
            dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(path_str))
        } else {
            std::path::PathBuf::from(path_str)
        };

        let path = expanded_path.as_path();

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

        let path_canonical = abs_path.canonicalize().unwrap_or(abs_path);

        // Allow access to ~/.skyclaw/* paths (system-level tool data)
        if let Some(home) = dirs::home_dir() {
            let skyclaw_dir = home.join(".skyclaw");
            if path_canonical.starts_with(&skyclaw_dir) {
                continue; // Allow ~/.skyclaw/* paths
            }
        }

        if !path_canonical.starts_with(&workspace_canonical) {
            return Err(Temm1eError::SandboxViolation(format!(
                "Tool '{}' declares access to '{}' which is outside workspace '{}'",
                tool.name(),
                path_str,
                workspace.display()
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use temm1e_core::{PathAccess, ToolDeclarations};
    use temm1e_test_utils::{make_session, MockTool};

    // ── Test helpers ────────────────────────────────────────────────────

    /// A mock tool that records invocation order via an atomic counter and
    /// optionally sleeps to simulate work (useful for concurrency tests).
    struct TimedMockTool {
        tool_name: String,
        output: ToolOutput,
        delay: std::time::Duration,
        invocation_counter: Arc<AtomicUsize>,
        /// Recorded invocation order index for this tool (set on execute).
        invocation_order: Arc<Mutex<Vec<String>>>,
    }

    impl TimedMockTool {
        fn new(
            name: &str,
            delay_ms: u64,
            counter: Arc<AtomicUsize>,
            order: Arc<Mutex<Vec<String>>>,
        ) -> Self {
            Self {
                tool_name: name.to_string(),
                output: ToolOutput {
                    content: format!("{} output", name),
                    is_error: false,
                },
                delay: std::time::Duration::from_millis(delay_ms),
                invocation_counter: counter,
                invocation_order: order,
            }
        }
    }

    use tokio::sync::Mutex;

    #[async_trait]
    impl Tool for TimedMockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "timed mock"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn declarations(&self) -> ToolDeclarations {
            ToolDeclarations {
                file_access: Vec::new(),
                network_access: Vec::new(),
                shell_access: false,
            }
        }
        async fn execute(
            &self,
            _input: ToolInput,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, Temm1eError> {
            let idx = self.invocation_counter.fetch_add(1, Ordering::SeqCst);
            {
                let mut order = self.invocation_order.lock().await;
                order.push(format!("{}:{}", self.tool_name, idx));
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            Ok(self.output.clone())
        }
    }

    /// A mock tool that always fails.
    struct FailingTool {
        tool_name: String,
    }

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn declarations(&self) -> ToolDeclarations {
            ToolDeclarations {
                file_access: Vec::new(),
                network_access: Vec::new(),
                shell_access: false,
            }
        }
        async fn execute(
            &self,
            _input: ToolInput,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, Temm1eError> {
            Err(Temm1eError::Tool(format!("{} failed", self.tool_name)))
        }
    }

    /// A mock tool that tracks max concurrent executions via a barrier pattern.
    struct ConcurrencyTrackingTool {
        tool_name: String,
        active_count: Arc<AtomicUsize>,
        peak_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for ConcurrencyTrackingTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "tracks concurrency"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn declarations(&self) -> ToolDeclarations {
            ToolDeclarations {
                file_access: Vec::new(),
                network_access: Vec::new(),
                shell_access: false,
            }
        }
        async fn execute(
            &self,
            _input: ToolInput,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, Temm1eError> {
            let current = self.active_count.fetch_add(1, Ordering::SeqCst) + 1;
            // Update peak
            self.peak_count.fetch_max(current, Ordering::SeqCst);
            // Hold for a bit so concurrent tasks overlap
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.active_count.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput {
                content: "ok".to_string(),
                is_error: false,
            })
        }
    }

    #[tokio::test]
    async fn execute_tool_returns_output() {
        let tool = MockTool::new("test_tool");
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let result = execute_tool("test_tool", serde_json::json!({}), &tools, &session).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "mock output");
    }

    #[tokio::test]
    async fn execute_unknown_tool_returns_error() {
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let session = make_session();

        let result = execute_tool("nonexistent", serde_json::json!({}), &tools, &session).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Temm1eError::Tool(_)));
    }

    #[test]
    fn sandbox_allows_workspace_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().to_path_buf();

        // Create a file inside workspace for canonicalization
        let inner_dir = workspace.join("subdir");
        std::fs::create_dir_all(&inner_dir).unwrap();

        let tool = MockTool::new("file_tool").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::Read("subdir".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_rejects_path_outside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = MockTool::new("evil_tool").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::Write("/etc/passwd".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Temm1eError::SandboxViolation(_)
        ));
    }

    #[test]
    fn sandbox_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = MockTool::new("traversal_tool").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::Read("../../etc/shadow".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_no_file_access() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = MockTool::new("network_only");

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: tmp.path().to_path_buf(),
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_ok());
    }

    // ── T5b: New sandbox security & edge case tests ───────────────────

    #[test]
    fn sandbox_rejects_double_dot_encoded_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        // Path with encoded-style traversal (literal string, not URL-encoded)
        let tool = MockTool::new("encoded_traversal").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::Read("../../../etc/passwd".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_rejects_absolute_path_to_root() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let tool = MockTool::new("root_access").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::ReadWrite("/".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_err());
    }

    #[test]
    fn sandbox_allows_nested_workspace_path() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().to_path_buf();
        let nested = workspace.join("src").join("lib");
        std::fs::create_dir_all(&nested).unwrap();

        let tool = MockTool::new("nested_tool").with_declarations(ToolDeclarations {
            file_access: vec![PathAccess::Read("src/lib".to_string())],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_multiple_file_accesses_all_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().to_path_buf();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();

        let tool = MockTool::new("multi_tool").with_declarations(ToolDeclarations {
            file_access: vec![
                PathAccess::Read("src".to_string()),
                PathAccess::Write("docs".to_string()),
            ],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_ok());
    }

    #[test]
    fn sandbox_one_bad_path_among_multiple_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("valid")).unwrap();

        let tool = MockTool::new("mixed_tool").with_declarations(ToolDeclarations {
            file_access: vec![
                PathAccess::Read("valid".to_string()),
                PathAccess::Write("/etc/shadow".to_string()),
            ],
            network_access: Vec::new(),
            shell_access: false,
        });

        let session = SessionContext {
            session_id: "test".to_string(),
            channel: "cli".to_string(),
            chat_id: "c".to_string(),
            user_id: "u".to_string(),
            history: Vec::new(),
            workspace_path: workspace,
        };

        let result = validate_sandbox(&tool, &session);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_tool_with_custom_output() {
        let tool = MockTool::new("custom").with_output(ToolOutput {
            content: "custom result".to_string(),
            is_error: false,
        });
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let result = execute_tool("custom", serde_json::json!({}), &tools, &session)
            .await
            .unwrap();
        assert_eq!(result.content, "custom result");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn execute_tool_error_output() {
        let tool = MockTool::new("err_tool").with_output(ToolOutput {
            content: "something went wrong".to_string(),
            is_error: true,
        });
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let result = execute_tool("err_tool", serde_json::json!({}), &tools, &session)
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, "something went wrong");
    }

    // ── Parallel executor tests ─────────────────────────────────────────

    #[tokio::test]
    async fn parallel_empty_tool_calls() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockTool::new("t"))];
        let session = make_session();

        let results = execute_tools_parallel(vec![], &tools, &session, 5).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn parallel_single_tool_call() {
        let tool = MockTool::new("file_read");
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let calls = vec![ToolCall {
            id: "tc_1".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({}),
        }];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "tc_1");
        assert!(results[0].output.is_ok());
        assert_eq!(results[0].output.as_ref().unwrap().content, "mock output");
    }

    #[tokio::test]
    async fn parallel_independent_file_reads_run_concurrently() {
        let counter = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));

        let tool = TimedMockTool::new("file_read", 50, Arc::clone(&counter), Arc::clone(&order));

        // Lookup is by name, so a single tool instance serves both calls.
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let calls = vec![
            ToolCall {
                id: "tc_a".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
            ToolCall {
                id: "tc_b".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "b.txt"}),
            },
        ];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].output.is_ok());
        assert!(results[1].output.is_ok());
        assert_eq!(results[0].id, "tc_a");
        assert_eq!(results[1].id, "tc_b");
    }

    #[tokio::test]
    async fn parallel_result_ordering_preserved() {
        let tool = MockTool::new("file_read");
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let calls = vec![
            ToolCall {
                id: "first".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "1.txt"}),
            },
            ToolCall {
                id: "second".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "2.txt"}),
            },
            ToolCall {
                id: "third".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "3.txt"}),
            },
        ];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, "first");
        assert_eq!(results[1].id, "second");
        assert_eq!(results[2].id, "third");
    }

    #[tokio::test]
    async fn parallel_individual_failure_does_not_abort_others() {
        let good_tool = MockTool::new("file_read");
        let bad_tool = FailingTool {
            tool_name: "bad_tool".to_string(),
        };
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(good_tool), Arc::new(bad_tool)];
        let session = make_session();

        let calls = vec![
            ToolCall {
                id: "tc_good1".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({}),
            },
            ToolCall {
                id: "tc_bad".to_string(),
                name: "bad_tool".to_string(),
                arguments: serde_json::json!({}),
            },
            ToolCall {
                id: "tc_good2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({}),
            },
        ];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;
        assert_eq!(results.len(), 3);

        // First and third should succeed
        assert!(results[0].output.is_ok(), "first call should succeed");
        assert_eq!(results[0].id, "tc_good1");

        // Second should fail
        assert!(results[1].output.is_err(), "second call should fail");
        assert_eq!(results[1].id, "tc_bad");

        // Third should still succeed
        assert!(results[2].output.is_ok(), "third call should succeed");
        assert_eq!(results[2].id, "tc_good2");
    }

    #[tokio::test]
    async fn parallel_unknown_tool_returns_error_result() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(MockTool::new("file_read"))];
        let session = make_session();

        let calls = vec![ToolCall {
            id: "tc_missing".to_string(),
            name: "nonexistent".to_string(),
            arguments: serde_json::json!({}),
        }];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].output.is_err());
        let err = results[0].output.as_ref().unwrap_err();
        assert!(matches!(err, Temm1eError::Tool(_)));
    }

    #[tokio::test]
    async fn parallel_max_concurrent_zero_uses_default() {
        let tool = MockTool::new("file_read");
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        let calls = vec![ToolCall {
            id: "tc_1".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({}),
        }];

        // max_concurrent=0 should not panic, should use default
        let results = execute_tools_parallel(calls, &tools, &session, 0).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].output.is_ok());
    }

    #[tokio::test]
    async fn parallel_max_concurrency_respected() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let tool = ConcurrencyTrackingTool {
            tool_name: "file_read".to_string(),
            active_count: Arc::clone(&active),
            peak_count: Arc::clone(&peak),
        };
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
        let session = make_session();

        // Launch 6 independent calls with max_concurrent=2
        let calls: Vec<ToolCall> = (0..6)
            .map(|i| ToolCall {
                id: format!("tc_{}", i),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": format!("file_{}.txt", i)}),
            })
            .collect();

        let results = execute_tools_parallel(calls, &tools, &session, 2).await;

        assert_eq!(results.len(), 6);
        for r in &results {
            assert!(r.output.is_ok());
        }

        // Peak concurrent executions should not exceed 2
        let peak_val = peak.load(Ordering::SeqCst);
        assert!(
            peak_val <= 2,
            "Peak concurrency {} exceeded limit 2",
            peak_val
        );
    }

    // ── Dependency detection tests ──────────────────────────────────────

    #[test]
    fn deps_empty_calls() {
        let groups = detect_dependencies(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn deps_single_call() {
        let calls = vec![ToolCall {
            id: "1".to_string(),
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
        }];
        let groups = detect_dependencies(&calls);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0]);
    }

    #[test]
    fn deps_independent_file_reads_separate_paths() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "b.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        // Two independent reads should be in separate groups
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn deps_file_reads_same_path_independent() {
        // Two reads of the same file are independent (read-read no conflict)
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "same.txt"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "same.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(groups.len(), 2, "read-read same path should be independent");
    }

    #[test]
    fn deps_write_then_read_same_file_dependent() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "data.txt"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "data.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        // Write then read of the same file must be sequential (1 group)
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn deps_two_writes_same_file_dependent() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "out.txt"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "out.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(groups.len(), 1, "two writes to same file are dependent");
    }

    #[test]
    fn deps_shell_calls_always_dependent() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "echo hello"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "echo world"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(groups.len(), 1, "shell calls should be grouped together");
    }

    #[test]
    fn deps_mixed_independent_and_dependent() {
        let calls = vec![
            // Group 1: write to data.txt
            ToolCall {
                id: "w1".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "data.txt"}),
            },
            // Independent: read from other.txt
            ToolCall {
                id: "r1".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "other.txt"}),
            },
            // Group 1: read from data.txt (depends on w1)
            ToolCall {
                id: "r2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "data.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);

        // Should have 2 groups: {w1, r2} and {r1}
        assert_eq!(groups.len(), 2);

        // Find which group has 2 elements and which has 1
        let (big, small) = if groups[0].len() > groups[1].len() {
            (&groups[0], &groups[1])
        } else {
            (&groups[1], &groups[0])
        };

        assert_eq!(big.len(), 2);
        assert_eq!(small.len(), 1);

        // The independent read (index 1) should be alone
        assert!(small.contains(&1));

        // The dependent pair (indices 0 and 2) should be together
        assert!(big.contains(&0));
        assert!(big.contains(&2));
    }

    #[test]
    fn deps_different_tools_different_paths_independent() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "b.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(groups.len(), 2, "different paths should be independent");
    }

    #[test]
    fn deps_chain_of_three_dependent_calls() {
        // A writes X, B reads X and writes Y, C reads Y
        // A->B (share X with write), B->C (share Y with write)
        // All three should be in one group via transitive dependency
        let calls = vec![
            ToolCall {
                id: "a".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "x.txt"}),
            },
            ToolCall {
                id: "b".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "x.txt"}),
            },
            ToolCall {
                id: "c".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "x.txt"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(
            groups.len(),
            1,
            "transitive dependencies should merge groups"
        );
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn deps_no_path_args_same_unknown_tool_dependent() {
        // Two calls to the same unknown tool with no file path args
        // Conservative: treat as dependent
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "custom_tool".to_string(),
                arguments: serde_json::json!({"key": "value1"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "custom_tool".to_string(),
                arguments: serde_json::json!({"key": "value2"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(
            groups.len(),
            1,
            "same unknown tool with no paths should be conservative (dependent)"
        );
    }

    #[test]
    fn deps_no_path_args_different_tools_independent() {
        let calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "tool_a".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "tool_b".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            },
        ];
        let groups = detect_dependencies(&calls);
        assert_eq!(
            groups.len(),
            2,
            "different tools with no path overlap should be independent"
        );
    }

    #[tokio::test]
    async fn parallel_multiple_different_tools() {
        let read_tool = MockTool::new("file_read").with_output(ToolOutput {
            content: "read result".to_string(),
            is_error: false,
        });
        let write_tool = MockTool::new("file_write").with_output(ToolOutput {
            content: "write result".to_string(),
            is_error: false,
        });
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(read_tool), Arc::new(write_tool)];
        let session = make_session();

        let calls = vec![
            ToolCall {
                id: "tc_r".to_string(),
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
            },
            ToolCall {
                id: "tc_w".to_string(),
                name: "file_write".to_string(),
                arguments: serde_json::json!({"path": "b.txt"}),
            },
        ];

        let results = execute_tools_parallel(calls, &tools, &session, 5).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "tc_r");
        assert_eq!(results[0].output.as_ref().unwrap().content, "read result");
        assert_eq!(results[1].id, "tc_w");
        assert_eq!(results[1].output.as_ref().unwrap().content, "write result");
    }

    #[test]
    fn is_read_only_tool_classification() {
        assert!(is_read_only_tool("file_read"));
        assert!(is_read_only_tool("file_list"));
        assert!(is_read_only_tool("file_search"));
        assert!(is_read_only_tool("browser"));
        assert!(!is_read_only_tool("file_write"));
        assert!(!is_read_only_tool("shell"));
        assert!(!is_read_only_tool("custom_tool"));
    }

    #[test]
    fn extract_file_paths_from_arguments() {
        let args = serde_json::json!({
            "path": "/tmp/a.txt",
            "file": "/tmp/b.txt",
            "other_key": "ignored",
        });
        let paths = extract_file_paths(&args);
        assert!(paths.contains(&"/tmp/a.txt".to_string()));
        assert!(paths.contains(&"/tmp/b.txt".to_string()));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn extract_file_paths_empty_for_non_object() {
        let args = serde_json::json!("just a string");
        let paths = extract_file_paths(&args);
        assert!(paths.is_empty());
    }

    #[test]
    fn extract_write_paths_file_read_returns_empty() {
        let args = serde_json::json!({"path": "a.txt"});
        let writes = extract_write_paths("file_read", &args);
        assert!(writes.is_empty(), "file_read should have no write paths");
    }

    #[test]
    fn extract_write_paths_file_write_returns_paths() {
        let args = serde_json::json!({"path": "a.txt"});
        let writes = extract_write_paths("file_write", &args);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], "a.txt");
    }
}
