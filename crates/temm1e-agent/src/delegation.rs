//! Agent-to-Agent Delegation — Sub-agent spawning for complex tasks.
//!
//! For complex tasks, the primary agent can spawn sub-agents with scoped
//! objectives. Each sub-agent has its own context, tool subset, model
//! (potentially cheaper/faster), and verification loop. Sub-agents report
//! back with structured results that are aggregated for the primary agent.
//!
//! Safety constraints:
//! - Hard limit on total sub-agents per task (`max_total_agents`)
//! - Sub-agents cannot spawn their own sub-agents (no recursion)
//! - Each sub-agent has its own timeout
//! - Token usage is tracked across all sub-agents

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum number of concurrent sub-agents.
const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Default maximum total sub-agents per task (safety limit).
const DEFAULT_MAX_TOTAL_AGENTS: usize = 10;

/// Default maximum tool rounds for a sub-agent.
const DEFAULT_MAX_ROUNDS: usize = 10;

/// Default sub-agent timeout (5 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Default model for sub-agents (cheap/fast).
const DEFAULT_SUB_AGENT_MODEL: &str = "claude-haiku-4-5-20251001";

// ---------------------------------------------------------------------------
// Tool keyword mapping for heuristic tool assignment
// ---------------------------------------------------------------------------

/// Maps keywords in objectives to relevant tool names.
const TOOL_KEYWORDS: &[(&[&str], &[&str])] = &[
    (
        &[
            "file",
            "read",
            "write",
            "edit",
            "create",
            "delete",
            "directory",
            "folder",
        ],
        &[
            "file_read",
            "file_write",
            "file_list",
            "list_directory",
            "read_file",
        ],
    ),
    (
        &[
            "git", "commit", "branch", "merge", "push", "pull", "diff", "log",
        ],
        &["git_status", "git_log", "git_diff", "shell"],
    ),
    (
        &[
            "shell", "command", "run", "execute", "install", "build", "compile", "test", "deploy",
        ],
        &["shell"],
    ),
    (
        &[
            "browse", "web", "http", "url", "fetch", "download", "api", "request",
        ],
        &["http_get", "browser"],
    ),
    (
        &["search", "find", "grep", "look"],
        &["file_read", "file_list", "shell", "list_directory"],
    ),
    (&["database", "db", "sql", "query", "migrate"], &["shell"]),
];

// ---------------------------------------------------------------------------
// SubAgentStatus
// ---------------------------------------------------------------------------

/// Execution status of a sub-agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubAgentStatus {
    /// Waiting to be started.
    Pending,
    /// Currently executing.
    Running,
    /// Finished successfully.
    Completed,
    /// Finished with an error.
    Failed,
    /// Exceeded its timeout.
    TimedOut,
}

impl std::fmt::Display for SubAgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::TimedOut => write!(f, "timed_out"),
        }
    }
}

// ---------------------------------------------------------------------------
// SubAgent
// ---------------------------------------------------------------------------

/// A sub-agent spawned by the primary agent to handle a scoped objective.
///
/// Sub-agents have their own model, tool subset, round limit, and timeout.
/// They cannot spawn further sub-agents (no recursion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgent {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Scoped task description for this sub-agent.
    pub objective: String,
    /// Model to use — can be cheaper/faster than the primary agent's model.
    pub model: String,
    /// Names of tools this sub-agent is allowed to use.
    pub tools: Vec<String>,
    /// Maximum number of tool-use rounds before the sub-agent must finish.
    pub max_rounds: usize,
    /// Maximum wall-clock time for this sub-agent.
    #[serde(with = "duration_serde")]
    pub timeout: Duration,
    /// Current execution status.
    pub status: SubAgentStatus,
}

impl SubAgent {
    /// Create a new sub-agent with the given objective and default settings.
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            objective: objective.into(),
            model: DEFAULT_SUB_AGENT_MODEL.to_string(),
            tools: Vec::new(),
            max_rounds: DEFAULT_MAX_ROUNDS,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            status: SubAgentStatus::Pending,
        }
    }

    /// Set the model for this sub-agent.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set the tools for this sub-agent.
    pub fn with_tools(mut self, tools: Vec<String>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the maximum number of tool rounds.
    pub fn with_max_rounds(mut self, max_rounds: usize) -> Self {
        self.max_rounds = max_rounds;
        self
    }

    /// Set the timeout duration.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set a specific ID (useful for testing).
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Transition to Running status.
    pub fn start(&mut self) -> Result<(), Temm1eError> {
        if self.status != SubAgentStatus::Pending {
            return Err(Temm1eError::Internal(format!(
                "Cannot start sub-agent '{}': status is {} (expected pending)",
                self.id, self.status
            )));
        }
        self.status = SubAgentStatus::Running;
        debug!(agent_id = %self.id, objective = %self.objective, "Sub-agent started");
        Ok(())
    }

    /// Mark as completed.
    pub fn complete(&mut self) -> Result<(), Temm1eError> {
        if self.status != SubAgentStatus::Running {
            return Err(Temm1eError::Internal(format!(
                "Cannot complete sub-agent '{}': status is {} (expected running)",
                self.id, self.status
            )));
        }
        self.status = SubAgentStatus::Completed;
        info!(agent_id = %self.id, "Sub-agent completed");
        Ok(())
    }

    /// Mark as failed.
    pub fn fail(&mut self) -> Result<(), Temm1eError> {
        if self.status != SubAgentStatus::Running {
            return Err(Temm1eError::Internal(format!(
                "Cannot fail sub-agent '{}': status is {} (expected running)",
                self.id, self.status
            )));
        }
        self.status = SubAgentStatus::Failed;
        warn!(agent_id = %self.id, "Sub-agent failed");
        Ok(())
    }

    /// Mark as timed out.
    pub fn time_out(&mut self) -> Result<(), Temm1eError> {
        if self.status != SubAgentStatus::Running {
            return Err(Temm1eError::Internal(format!(
                "Cannot time out sub-agent '{}': status is {} (expected running)",
                self.id, self.status
            )));
        }
        self.status = SubAgentStatus::TimedOut;
        warn!(agent_id = %self.id, "Sub-agent timed out");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SubAgentResult
// ---------------------------------------------------------------------------

/// Structured result from a completed (or failed/timed-out) sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    /// ID of the sub-agent that produced this result.
    pub agent_id: String,
    /// The objective the sub-agent was working on.
    pub objective: String,
    /// Final status.
    pub status: SubAgentStatus,
    /// The sub-agent's final text reply (empty on failure/timeout).
    pub output: String,
    /// Names of tools actually invoked during execution.
    pub tools_used: Vec<String>,
    /// Number of tool-use rounds taken.
    pub rounds_taken: usize,
    /// Wall-clock execution duration.
    #[serde(with = "duration_serde")]
    pub duration: Duration,
}

impl SubAgentResult {
    /// Create a result indicating successful completion.
    pub fn success(
        agent: &SubAgent,
        output: String,
        tools_used: Vec<String>,
        rounds_taken: usize,
        duration: Duration,
    ) -> Self {
        Self {
            agent_id: agent.id.clone(),
            objective: agent.objective.clone(),
            status: SubAgentStatus::Completed,
            output,
            tools_used,
            rounds_taken,
            duration,
        }
    }

    /// Create a result indicating failure.
    pub fn failure(agent: &SubAgent, error: String, duration: Duration) -> Self {
        Self {
            agent_id: agent.id.clone(),
            objective: agent.objective.clone(),
            status: SubAgentStatus::Failed,
            output: error,
            tools_used: Vec::new(),
            rounds_taken: 0,
            duration,
        }
    }

    /// Create a result indicating timeout.
    pub fn timed_out(agent: &SubAgent, duration: Duration) -> Self {
        Self {
            agent_id: agent.id.clone(),
            objective: agent.objective.clone(),
            status: SubAgentStatus::TimedOut,
            output: String::new(),
            tools_used: Vec::new(),
            rounds_taken: 0,
            duration,
        }
    }
}

// ---------------------------------------------------------------------------
// DelegationManager
// ---------------------------------------------------------------------------

/// Manages sub-agent spawning, limits, and result aggregation.
///
/// The delegation manager is responsible for:
/// - Planning how to decompose a task into sub-agent assignments
/// - Enforcing concurrency and total agent limits
/// - Building scoped prompts for each sub-agent
/// - Aggregating results from completed sub-agents
pub struct DelegationManager {
    /// Maximum number of sub-agents running concurrently.
    max_concurrent: usize,
    /// Hard limit on total sub-agents spawned per task.
    max_total_agents: usize,
    /// Counter for total sub-agents spawned (atomic for thread safety).
    spawned_count: AtomicUsize,
}

impl std::fmt::Debug for DelegationManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationManager")
            .field("max_concurrent", &self.max_concurrent)
            .field("max_total_agents", &self.max_total_agents)
            .field("spawned_count", &self.spawned_count.load(Ordering::Relaxed))
            .finish()
    }
}

impl DelegationManager {
    /// Create a new delegation manager with the given limits.
    pub fn new(max_concurrent: usize, max_total_agents: usize) -> Self {
        info!(
            max_concurrent,
            max_total_agents, "DelegationManager created"
        );
        Self {
            max_concurrent,
            max_total_agents,
            spawned_count: AtomicUsize::new(0),
        }
    }

    /// Create a delegation manager with default limits.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_TOTAL_AGENTS)
    }

    /// Check whether a new sub-agent can be spawned under current limits.
    pub fn can_spawn(&self) -> bool {
        let current = self.spawned_count.load(Ordering::Relaxed);
        current < self.max_total_agents
    }

    /// Return the number of sub-agents spawned so far.
    pub fn spawned_count(&self) -> usize {
        self.spawned_count.load(Ordering::Relaxed)
    }

    /// Return the maximum concurrency level.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Return the maximum total agents limit.
    pub fn max_total_agents(&self) -> usize {
        self.max_total_agents
    }

    /// Increment the spawned counter. Returns an error if the limit is exceeded.
    pub fn register_spawn(&self) -> Result<usize, Temm1eError> {
        let previous = self.spawned_count.fetch_add(1, Ordering::Relaxed);
        if previous >= self.max_total_agents {
            // Roll back — we exceeded the limit.
            self.spawned_count.fetch_sub(1, Ordering::Relaxed);
            return Err(Temm1eError::Internal(format!(
                "Sub-agent spawn limit exceeded: max {} agents per task",
                self.max_total_agents
            )));
        }
        debug!(
            spawned = previous + 1,
            max = self.max_total_agents,
            "Sub-agent registered"
        );
        Ok(previous + 1)
    }

    /// Decompose a task description into sub-agent assignments.
    ///
    /// Uses heuristics to split compound tasks:
    /// - Numbered lists ("1. ..., 2. ..., 3. ...")
    /// - Conjunction splitting ("X and Y and Z")
    /// - Sequential splitting ("X then Y then Z")
    /// - Semicolon splitting ("X; Y; Z")
    ///
    /// Each sub-agent gets the subset of available tools relevant to its
    /// objective based on keyword matching.
    pub fn plan_delegation(
        &self,
        task: &str,
        available_tools: &[String],
    ) -> Result<Vec<SubAgent>, Temm1eError> {
        if task.trim().is_empty() {
            return Err(Temm1eError::Internal(
                "Cannot plan delegation for empty task".to_string(),
            ));
        }

        let parts = split_task(task);

        if parts.is_empty() {
            return Err(Temm1eError::Internal(
                "Task decomposition produced no sub-tasks".to_string(),
            ));
        }

        // Cap the number of sub-agents to the remaining budget.
        let remaining = self
            .max_total_agents
            .saturating_sub(self.spawned_count.load(Ordering::Relaxed));

        if remaining == 0 {
            return Err(Temm1eError::Internal(format!(
                "Sub-agent spawn limit reached: {} of {} already spawned",
                self.spawned_count.load(Ordering::Relaxed),
                self.max_total_agents
            )));
        }

        let parts_to_use = if parts.len() > remaining {
            warn!(
                total_parts = parts.len(),
                remaining, "Task has more parts than remaining sub-agent budget; truncating"
            );
            &parts[..remaining]
        } else {
            &parts
        };

        let agents: Vec<SubAgent> = parts_to_use
            .iter()
            .map(|part| {
                let tools = assign_tools(part, available_tools);
                SubAgent::new(part.clone()).with_tools(tools)
            })
            .collect();

        info!(
            task_parts = agents.len(),
            task = %task,
            "Planned delegation"
        );

        Ok(agents)
    }

    /// Build a focused system prompt for a sub-agent, scoping it to its
    /// objective and available tools.
    pub fn format_delegation_prompt(sub_agent: &SubAgent) -> String {
        let mut prompt = String::new();

        prompt.push_str("You are a focused sub-agent with a specific objective.\n\n");
        prompt.push_str("## Objective\n");
        prompt.push_str(&sub_agent.objective);
        prompt.push_str("\n\n");

        prompt.push_str("## Constraints\n");
        prompt.push_str(&format!(
            "- You have a maximum of {} tool-use rounds.\n",
            sub_agent.max_rounds
        ));
        prompt.push_str(&format!(
            "- You must complete within {} seconds.\n",
            sub_agent.timeout.as_secs()
        ));
        prompt.push_str("- You CANNOT delegate to other sub-agents. Complete the task yourself.\n");
        prompt.push_str("- Stay focused on your objective. Do not work on unrelated tasks.\n");
        prompt.push('\n');

        if !sub_agent.tools.is_empty() {
            prompt.push_str("## Available Tools\n");
            for tool in &sub_agent.tools {
                prompt.push_str(&format!("- {}\n", tool));
            }
            prompt.push_str("\nUse only the tools listed above.\n\n");
        } else {
            prompt.push_str("## Tools\n");
            prompt.push_str("No tools are available. Respond based on your knowledge.\n\n");
        }

        prompt.push_str("## Output Format\n");
        prompt.push_str(
            "Provide a clear, concise result of your work. \
             Include any relevant details, file paths, or outputs.\n",
        );

        prompt
    }

    /// Aggregate results from multiple sub-agents into a structured summary
    /// suitable for the primary agent's context.
    pub fn aggregate_results(results: &[SubAgentResult]) -> String {
        if results.is_empty() {
            return "No sub-agent results to aggregate.".to_string();
        }

        let mut summary = String::new();
        summary.push_str("## Sub-Agent Results Summary\n\n");

        let completed = results
            .iter()
            .filter(|r| r.status == SubAgentStatus::Completed)
            .count();
        let failed = results
            .iter()
            .filter(|r| r.status == SubAgentStatus::Failed)
            .count();
        let timed_out = results
            .iter()
            .filter(|r| r.status == SubAgentStatus::TimedOut)
            .count();

        summary.push_str(&format!(
            "**Overall:** {} completed, {} failed, {} timed out (out of {} total)\n\n",
            completed,
            failed,
            timed_out,
            results.len()
        ));

        for (i, result) in results.iter().enumerate() {
            summary.push_str(&format!(
                "### Sub-Agent {} — {} [{}]\n",
                i + 1,
                result.objective,
                result.status
            ));

            if !result.output.is_empty() {
                summary.push_str(&format!("**Output:** {}\n", result.output));
            }

            if !result.tools_used.is_empty() {
                summary.push_str(&format!(
                    "**Tools used:** {}\n",
                    result.tools_used.join(", ")
                ));
            }

            summary.push_str(&format!(
                "**Rounds:** {} | **Duration:** {:.1}s\n\n",
                result.rounds_taken,
                result.duration.as_secs_f64()
            ));
        }

        summary
    }

    /// Format the current status of all sub-agents for injection into the
    /// primary agent's context.
    pub fn format_status_update(agents: &[SubAgent]) -> String {
        if agents.is_empty() {
            return "No active sub-agents.".to_string();
        }

        let mut status = String::new();
        status.push_str("## Sub-Agent Status\n\n");
        status.push_str("| # | Objective | Model | Status |\n");
        status.push_str("|---|-----------|-------|--------|\n");

        for (i, agent) in agents.iter().enumerate() {
            // Truncate long objectives for the table.
            let obj_display = if agent.objective.len() > 60 {
                format!("{}...", &agent.objective[..57])
            } else {
                agent.objective.clone()
            };
            status.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                i + 1,
                obj_display,
                agent.model,
                agent.status
            ));
        }

        let pending = agents
            .iter()
            .filter(|a| a.status == SubAgentStatus::Pending)
            .count();
        let running = agents
            .iter()
            .filter(|a| a.status == SubAgentStatus::Running)
            .count();
        let done = agents
            .iter()
            .filter(|a| {
                matches!(
                    a.status,
                    SubAgentStatus::Completed | SubAgentStatus::Failed | SubAgentStatus::TimedOut
                )
            })
            .count();

        status.push_str(&format!(
            "\n**Progress:** {} pending, {} running, {} done\n",
            pending, running, done
        ));

        status
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Split a compound task description into individual parts using heuristics.
///
/// Tries these strategies in order:
/// 1. Numbered list (e.g., "1. Do X\n2. Do Y")
/// 2. Semicolons (e.g., "Do X; Do Y")
/// 3. "then" separator (e.g., "Do X then do Y")
/// 4. "and" separator (only for clearly compound sentences)
///
/// Returns at least one part (the original task if no splitting applies).
fn split_task(task: &str) -> Vec<String> {
    // Strategy 1: Numbered list — "1. ...", "2. ...", etc.
    let numbered = split_numbered_list(task);
    if numbered.len() > 1 {
        debug!(
            strategy = "numbered_list",
            parts = numbered.len(),
            "Split task"
        );
        return numbered;
    }

    // Strategy 2: Semicolons.
    let semicolons = split_by_delimiter(task, ";");
    if semicolons.len() > 1 {
        debug!(
            strategy = "semicolons",
            parts = semicolons.len(),
            "Split task"
        );
        return semicolons;
    }

    // Strategy 3: "then" — split on " then " (case-insensitive).
    let then_parts = split_by_word(task, "then");
    if then_parts.len() > 1 {
        debug!(strategy = "then", parts = then_parts.len(), "Split task");
        return then_parts;
    }

    // Strategy 4: "and" — only if we get 2-4 parts with meaningful content.
    let and_parts = split_by_word(task, "and");
    if and_parts.len() >= 2 && and_parts.len() <= 4 {
        // Only split on "and" if each part is long enough to be a real task.
        let all_meaningful = and_parts.iter().all(|p| p.split_whitespace().count() >= 2);
        if all_meaningful {
            debug!(strategy = "and", parts = and_parts.len(), "Split task");
            return and_parts;
        }
    }

    // Fallback: single task.
    vec![task.trim().to_string()]
}

/// Split on numbered list items: "1. X", "2. Y", etc.
fn split_numbered_list(task: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_list = false;

    for line in task.lines() {
        let trimmed = line.trim();

        // Check if line starts with a number followed by a period or parenthesis.
        let is_numbered = !trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .is_empty()
            && trimmed
                .chars()
                .find(|c| !c.is_ascii_digit())
                .is_some_and(|c| c == '.' || c == ')');

        if is_numbered {
            if in_list && !current.trim().is_empty() {
                parts.push(current.trim().to_string());
            }
            // Strip the number prefix.
            let content = trimmed
                .chars()
                .skip_while(|c| c.is_ascii_digit())
                .skip(1) // skip '.' or ')'
                .collect::<String>();
            current = content.trim().to_string();
            in_list = true;
        } else if in_list {
            // Continuation line.
            if !trimmed.is_empty() {
                current.push(' ');
                current.push_str(trimmed);
            }
        }
    }

    if in_list && !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }

    // If we found numbered items on a single line, try inline splitting.
    if parts.is_empty() {
        return split_inline_numbered(task);
    }

    parts
}

/// Split inline numbered lists like "1. Do X, 2. Do Y, 3. Do Z".
fn split_inline_numbered(task: &str) -> Vec<String> {
    // Match patterns like "1. ", "2. ", "3) " etc.
    let mut parts = Vec::new();
    let mut rest = task;

    loop {
        // Find next numbered marker.
        let next_pos = find_numbered_marker(rest, parts.len() + 1);

        match next_pos {
            Some(pos) => {
                // Skip the marker itself.
                let after_marker = &rest[pos..];
                let marker_end = after_marker
                    .find(|c: char| c != '.' && c != ')' && !c.is_ascii_digit())
                    .unwrap_or(after_marker.len());

                rest = &rest[pos + marker_end..];

                // Find where this item ends (at the next numbered marker or end).
                let next = find_numbered_marker(rest, parts.len() + 2);
                match next {
                    Some(end) => {
                        let part = rest[..end].trim().trim_end_matches(',').trim();
                        if !part.is_empty() {
                            parts.push(part.to_string());
                        }
                        rest = &rest[end..];
                    }
                    None => {
                        let part = rest.trim();
                        if !part.is_empty() {
                            parts.push(part.to_string());
                        }
                        break;
                    }
                }
            }
            None => break,
        }
    }

    parts
}

/// Find the position of a numbered marker like "1.", "2.", "3)" in text.
fn find_numbered_marker(text: &str, expected_num: usize) -> Option<usize> {
    let num_str = expected_num.to_string();
    let dot_pattern = format!("{}.", num_str);
    let paren_pattern = format!("{})", num_str);

    let dot_pos = text.find(&dot_pattern);
    let paren_pos = text.find(&paren_pattern);

    match (dot_pos, paren_pos) {
        (Some(d), Some(p)) => Some(d.min(p)),
        (Some(d), None) => Some(d),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    }
}

/// Split on a delimiter character, trimming and filtering empties.
fn split_by_delimiter(task: &str, delimiter: &str) -> Vec<String> {
    task.split(delimiter)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split on a word boundary, case-insensitive.
fn split_by_word(task: &str, word: &str) -> Vec<String> {
    let lower = task.to_lowercase();
    let pattern = format!(" {} ", word);
    let mut parts = Vec::new();
    let mut start = 0;

    while let Some(pos) = lower[start..].find(&pattern) {
        let absolute_pos = start + pos;
        let part = task[start..absolute_pos].trim().to_string();
        if !part.is_empty() {
            parts.push(part);
        }
        start = absolute_pos + pattern.len();
    }

    // Add the remaining text.
    let remainder = task[start..].trim().to_string();
    if !remainder.is_empty() {
        parts.push(remainder);
    }

    parts
}

/// Assign relevant tools to a sub-agent based on keyword matching.
fn assign_tools(objective: &str, available_tools: &[String]) -> Vec<String> {
    let lower = objective.to_lowercase();
    let mut assigned: Vec<String> = Vec::new();

    for (keywords, tool_names) in TOOL_KEYWORDS {
        let matches = keywords.iter().any(|kw| lower.contains(kw));
        if matches {
            for tool_name in *tool_names {
                let name = tool_name.to_string();
                if available_tools.contains(&name) && !assigned.contains(&name) {
                    assigned.push(name);
                }
            }
        }
    }

    // If no tools matched by keyword, give the sub-agent all available tools
    // (it may need them and the heuristic just didn't match).
    if assigned.is_empty() && !available_tools.is_empty() {
        debug!(
            objective = %objective,
            "No keyword-matched tools; assigning all available tools"
        );
        assigned = available_tools.to_vec();
    }

    assigned
}

// ---------------------------------------------------------------------------
// Serde helper for Duration
// ---------------------------------------------------------------------------

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    #[derive(Serialize, Deserialize)]
    struct DurationRepr {
        secs: u64,
        nanos: u32,
    }

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let repr = DurationRepr {
            secs: duration.as_secs(),
            nanos: duration.subsec_nanos(),
        };
        repr.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let repr = DurationRepr::deserialize(deserializer)?;
        Ok(Duration::new(repr.secs, repr.nanos))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── SubAgent creation ─────────────────────────────────────────────

    #[test]
    fn sub_agent_creation_defaults() {
        let agent = SubAgent::new("Read the config file");
        assert_eq!(agent.objective, "Read the config file");
        assert_eq!(agent.model, DEFAULT_SUB_AGENT_MODEL);
        assert_eq!(agent.max_rounds, DEFAULT_MAX_ROUNDS);
        assert_eq!(agent.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(agent.status, SubAgentStatus::Pending);
        assert!(agent.tools.is_empty());
        // ID should be a valid UUID.
        assert!(Uuid::parse_str(&agent.id).is_ok());
    }

    #[test]
    fn sub_agent_builder_methods() {
        let agent = SubAgent::new("Deploy the app")
            .with_model("gpt-4o-mini")
            .with_tools(vec!["shell".to_string(), "file_read".to_string()])
            .with_max_rounds(5)
            .with_timeout(Duration::from_secs(60))
            .with_id("test-agent-1");

        assert_eq!(agent.id, "test-agent-1");
        assert_eq!(agent.model, "gpt-4o-mini");
        assert_eq!(agent.tools, vec!["shell", "file_read"]);
        assert_eq!(agent.max_rounds, 5);
        assert_eq!(agent.timeout, Duration::from_secs(60));
    }

    // ── SubAgent status transitions ───────────────────────────────────

    #[test]
    fn sub_agent_valid_status_transitions() {
        let mut agent = SubAgent::new("task").with_id("a1");
        assert_eq!(agent.status, SubAgentStatus::Pending);

        agent.start().unwrap();
        assert_eq!(agent.status, SubAgentStatus::Running);

        agent.complete().unwrap();
        assert_eq!(agent.status, SubAgentStatus::Completed);
    }

    #[test]
    fn sub_agent_transition_to_failed() {
        let mut agent = SubAgent::new("task").with_id("a2");
        agent.start().unwrap();
        agent.fail().unwrap();
        assert_eq!(agent.status, SubAgentStatus::Failed);
    }

    #[test]
    fn sub_agent_transition_to_timed_out() {
        let mut agent = SubAgent::new("task").with_id("a3");
        agent.start().unwrap();
        agent.time_out().unwrap();
        assert_eq!(agent.status, SubAgentStatus::TimedOut);
    }

    #[test]
    fn sub_agent_invalid_start_from_running() {
        let mut agent = SubAgent::new("task").with_id("a4");
        agent.start().unwrap();
        let err = agent.start().unwrap_err();
        assert!(err.to_string().contains("running"));
    }

    #[test]
    fn sub_agent_invalid_complete_from_pending() {
        let mut agent = SubAgent::new("task").with_id("a5");
        let err = agent.complete().unwrap_err();
        assert!(err.to_string().contains("pending"));
    }

    #[test]
    fn sub_agent_invalid_fail_from_pending() {
        let mut agent = SubAgent::new("task").with_id("a6");
        let err = agent.fail().unwrap_err();
        assert!(err.to_string().contains("pending"));
    }

    // ── DelegationManager limits ──────────────────────────────────────

    #[test]
    fn delegation_manager_can_spawn_under_limit() {
        let mgr = DelegationManager::new(3, 5);
        assert!(mgr.can_spawn());
        assert_eq!(mgr.spawned_count(), 0);
    }

    #[test]
    fn delegation_manager_register_spawn_increments() {
        let mgr = DelegationManager::new(3, 5);
        let count = mgr.register_spawn().unwrap();
        assert_eq!(count, 1);
        assert_eq!(mgr.spawned_count(), 1);
        assert!(mgr.can_spawn());
    }

    #[test]
    fn delegation_manager_spawn_limit_enforced() {
        let mgr = DelegationManager::new(2, 3);
        mgr.register_spawn().unwrap(); // 1
        mgr.register_spawn().unwrap(); // 2
        mgr.register_spawn().unwrap(); // 3
        assert!(!mgr.can_spawn());

        let err = mgr.register_spawn().unwrap_err();
        assert!(err.to_string().contains("limit exceeded"));
        // Count should not have increased past the limit.
        assert_eq!(mgr.spawned_count(), 3);
    }

    // ── plan_delegation ───────────────────────────────────────────────

    #[test]
    fn plan_delegation_splits_on_and() {
        let mgr = DelegationManager::new(3, 10);
        let tools = vec!["shell".to_string(), "file_read".to_string()];
        let agents = mgr
            .plan_delegation("read the logs and deploy the app", &tools)
            .unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].objective, "read the logs");
        assert_eq!(agents[1].objective, "deploy the app");
    }

    #[test]
    fn plan_delegation_splits_on_then() {
        let mgr = DelegationManager::new(3, 10);
        let tools = vec!["shell".to_string()];
        let agents = mgr
            .plan_delegation("build the project then run tests", &tools)
            .unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].objective, "build the project");
        assert_eq!(agents[1].objective, "run tests");
    }

    #[test]
    fn plan_delegation_splits_on_semicolons() {
        let mgr = DelegationManager::new(3, 10);
        let tools = vec!["shell".to_string()];
        let agents = mgr
            .plan_delegation("compile the code; run lints; deploy", &tools)
            .unwrap();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0].objective, "compile the code");
        assert_eq!(agents[1].objective, "run lints");
        assert_eq!(agents[2].objective, "deploy");
    }

    #[test]
    fn plan_delegation_empty_task_returns_error() {
        let mgr = DelegationManager::new(3, 10);
        let err = mgr.plan_delegation("", &[]).unwrap_err();
        assert!(err.to_string().contains("empty task"));
    }

    #[test]
    fn plan_delegation_single_task_no_split() {
        let mgr = DelegationManager::new(3, 10);
        let tools = vec!["shell".to_string()];
        let agents = mgr
            .plan_delegation("deploy the application", &tools)
            .unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].objective, "deploy the application");
    }

    #[test]
    fn plan_delegation_respects_remaining_budget() {
        let mgr = DelegationManager::new(3, 2);
        // Exhaust 1 of 2 slots.
        mgr.register_spawn().unwrap();
        let tools = vec!["shell".to_string()];
        let agents = mgr
            .plan_delegation("task A; task B; task C", &tools)
            .unwrap();
        // Only 1 remaining slot, so should be truncated.
        assert_eq!(agents.len(), 1);
    }

    // ── Tool assignment ───────────────────────────────────────────────

    #[test]
    fn assign_tools_matches_file_keywords() {
        let available = vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "shell".to_string(),
        ];
        let tools = assign_tools("read the configuration file", &available);
        assert!(tools.contains(&"file_read".to_string()));
        assert!(tools.contains(&"file_write".to_string()));
        // "shell" should NOT be included since no shell keywords match.
        assert!(!tools.contains(&"shell".to_string()));
    }

    #[test]
    fn assign_tools_fallback_to_all_when_no_match() {
        let available = vec!["custom_tool".to_string(), "another_tool".to_string()];
        let tools = assign_tools("do something unusual", &available);
        // No keywords match, so all tools should be assigned.
        assert_eq!(tools, available);
    }

    // ── format_delegation_prompt ──────────────────────────────────────

    #[test]
    fn format_delegation_prompt_includes_objective() {
        let agent = SubAgent::new("Read the error logs")
            .with_tools(vec!["file_read".to_string(), "shell".to_string()])
            .with_max_rounds(5)
            .with_timeout(Duration::from_secs(120));

        let prompt = DelegationManager::format_delegation_prompt(&agent);
        assert!(prompt.contains("Read the error logs"));
        assert!(prompt.contains("5 tool-use rounds"));
        assert!(prompt.contains("120 seconds"));
        assert!(prompt.contains("file_read"));
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("CANNOT delegate"));
    }

    #[test]
    fn format_delegation_prompt_no_tools() {
        let agent = SubAgent::new("Summarize the situation");
        let prompt = DelegationManager::format_delegation_prompt(&agent);
        assert!(prompt.contains("No tools are available"));
    }

    // ── aggregate_results ─────────────────────────────────────────────

    #[test]
    fn aggregate_results_empty() {
        let summary = DelegationManager::aggregate_results(&[]);
        assert_eq!(summary, "No sub-agent results to aggregate.");
    }

    #[test]
    fn aggregate_results_mixed_statuses() {
        let agent1 = SubAgent::new("task 1").with_id("a1");
        let agent2 = SubAgent::new("task 2").with_id("a2");
        let agent3 = SubAgent::new("task 3").with_id("a3");

        let results = vec![
            SubAgentResult::success(
                &agent1,
                "Found 3 errors".to_string(),
                vec!["shell".to_string()],
                2,
                Duration::from_secs(10),
            ),
            SubAgentResult::failure(
                &agent2,
                "Permission denied".to_string(),
                Duration::from_secs(5),
            ),
            SubAgentResult::timed_out(&agent3, Duration::from_secs(300)),
        ];

        let summary = DelegationManager::aggregate_results(&results);
        assert!(summary.contains("1 completed"));
        assert!(summary.contains("1 failed"));
        assert!(summary.contains("1 timed out"));
        assert!(summary.contains("Found 3 errors"));
        assert!(summary.contains("Permission denied"));
        assert!(summary.contains("task 1"));
        assert!(summary.contains("task 2"));
        assert!(summary.contains("task 3"));
    }

    // ── format_status_update ──────────────────────────────────────────

    #[test]
    fn format_status_update_empty() {
        let status = DelegationManager::format_status_update(&[]);
        assert_eq!(status, "No active sub-agents.");
    }

    #[test]
    fn format_status_update_mixed() {
        let agents = vec![
            SubAgent::new("read logs").with_id("a1"),
            {
                let mut a = SubAgent::new("deploy app").with_id("a2");
                a.status = SubAgentStatus::Running;
                a
            },
            {
                let mut a = SubAgent::new("send report").with_id("a3");
                a.status = SubAgentStatus::Completed;
                a
            },
        ];

        let status = DelegationManager::format_status_update(&agents);
        assert!(status.contains("read logs"));
        assert!(status.contains("deploy app"));
        assert!(status.contains("send report"));
        assert!(status.contains("1 pending"));
        assert!(status.contains("1 running"));
        assert!(status.contains("1 done"));
    }

    #[test]
    fn format_status_update_truncates_long_objectives() {
        let long_objective = "a".repeat(100);
        let agents = vec![SubAgent::new(long_objective).with_id("a1")];
        let status = DelegationManager::format_status_update(&agents);
        assert!(status.contains("..."));
    }

    // ── Safety: no recursion ──────────────────────────────────────────

    #[test]
    fn delegation_prompt_forbids_sub_delegation() {
        let agent = SubAgent::new("complex task");
        let prompt = DelegationManager::format_delegation_prompt(&agent);
        assert!(prompt.contains("CANNOT delegate"));
    }

    // ── SubAgentResult constructors ───────────────────────────────────

    #[test]
    fn sub_agent_result_success_fields() {
        let agent = SubAgent::new("objective").with_id("r1");
        let result = SubAgentResult::success(
            &agent,
            "done".to_string(),
            vec!["shell".to_string()],
            3,
            Duration::from_secs(42),
        );
        assert_eq!(result.agent_id, "r1");
        assert_eq!(result.objective, "objective");
        assert_eq!(result.status, SubAgentStatus::Completed);
        assert_eq!(result.output, "done");
        assert_eq!(result.tools_used, vec!["shell"]);
        assert_eq!(result.rounds_taken, 3);
        assert_eq!(result.duration, Duration::from_secs(42));
    }

    #[test]
    fn sub_agent_result_failure_fields() {
        let agent = SubAgent::new("failing task").with_id("r2");
        let result =
            SubAgentResult::failure(&agent, "crashed".to_string(), Duration::from_millis(500));
        assert_eq!(result.status, SubAgentStatus::Failed);
        assert_eq!(result.output, "crashed");
        assert!(result.tools_used.is_empty());
    }

    #[test]
    fn sub_agent_result_timed_out_fields() {
        let agent = SubAgent::new("slow task").with_id("r3");
        let result = SubAgentResult::timed_out(&agent, Duration::from_secs(300));
        assert_eq!(result.status, SubAgentStatus::TimedOut);
        assert!(result.output.is_empty());
    }

    // ── Serde roundtrip ───────────────────────────────────────────────

    #[test]
    fn sub_agent_serde_roundtrip() {
        let agent = SubAgent::new("test task")
            .with_id("serde-1")
            .with_model("claude-haiku-4-5-20251001")
            .with_tools(vec!["shell".to_string()])
            .with_max_rounds(7)
            .with_timeout(Duration::from_secs(120));

        let json = serde_json::to_string(&agent).unwrap();
        let restored: SubAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, "serde-1");
        assert_eq!(restored.objective, "test task");
        assert_eq!(restored.model, "claude-haiku-4-5-20251001");
        assert_eq!(restored.tools, vec!["shell"]);
        assert_eq!(restored.max_rounds, 7);
        assert_eq!(restored.timeout, Duration::from_secs(120));
        assert_eq!(restored.status, SubAgentStatus::Pending);
    }

    #[test]
    fn sub_agent_result_serde_roundtrip() {
        let agent = SubAgent::new("task").with_id("sr1");
        let result = SubAgentResult::success(
            &agent,
            "output text".to_string(),
            vec!["file_read".to_string()],
            2,
            Duration::from_millis(1500),
        );

        let json = serde_json::to_string(&result).unwrap();
        let restored: SubAgentResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.agent_id, "sr1");
        assert_eq!(restored.output, "output text");
        assert_eq!(restored.duration, Duration::from_millis(1500));
    }

    // ── SubAgentStatus display ────────────────────────────────────────

    #[test]
    fn sub_agent_status_display() {
        assert_eq!(SubAgentStatus::Pending.to_string(), "pending");
        assert_eq!(SubAgentStatus::Running.to_string(), "running");
        assert_eq!(SubAgentStatus::Completed.to_string(), "completed");
        assert_eq!(SubAgentStatus::Failed.to_string(), "failed");
        assert_eq!(SubAgentStatus::TimedOut.to_string(), "timed_out");
    }

    // ── split_task internals ──────────────────────────────────────────

    #[test]
    fn split_task_numbered_multiline() {
        let task = "1. Read the file\n2. Parse the data\n3. Write the report";
        let parts = split_task(task);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "Read the file");
        assert_eq!(parts[1], "Parse the data");
        assert_eq!(parts[2], "Write the report");
    }

    #[test]
    fn split_task_no_split_for_short_and() {
        // "and" joining short fragments should NOT split.
        let task = "read and write";
        let parts = split_task(task);
        // "read" alone is only 1 word, so "and" split is rejected.
        assert_eq!(parts.len(), 1);
    }

    // ── DelegationManager defaults ────────────────────────────────────

    #[test]
    fn delegation_manager_defaults() {
        let mgr = DelegationManager::with_defaults();
        assert_eq!(mgr.max_concurrent(), DEFAULT_MAX_CONCURRENT);
        assert_eq!(mgr.max_total_agents(), DEFAULT_MAX_TOTAL_AGENTS);
        assert_eq!(mgr.spawned_count(), 0);
        assert!(mgr.can_spawn());
    }

    #[test]
    fn delegation_manager_debug_output() {
        let mgr = DelegationManager::new(2, 5);
        let debug_str = format!("{:?}", mgr);
        assert!(debug_str.contains("DelegationManager"));
        assert!(debug_str.contains("max_concurrent: 2"));
        assert!(debug_str.contains("max_total_agents: 5"));
    }
}
