//! Task Decomposition — Compound Orders to Task Graphs
//!
//! Parses compound user orders into sub-tasks with dependencies and executes
//! them in topological order. For example, "Deploy the app, run migrations,
//! verify health, and send me the logs" becomes 4 sub-tasks executed in order.
//!
//! The module provides:
//! - `SubTask` / `SubTaskStatus` — individual work units with dependency tracking
//! - `TaskGraph` — a DAG of sub-tasks with ready-task resolution and progress
//! - `decompose_prompt()` — generates a prompt asking the LLM to break down a goal
//! - `parse_decomposition()` — parses the LLM's numbered-list response into sub-tasks

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Status of a sub-task within a task graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for SubTaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// A single sub-task within a task graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    /// Unique identifier for this sub-task (e.g. "1", "step-1", UUID).
    pub id: String,
    /// Human-readable description of what this sub-task does.
    pub description: String,
    /// IDs of sub-tasks that must complete before this one can start.
    pub dependencies: Vec<String>,
    /// Current execution status.
    pub status: SubTaskStatus,
    /// Result or output from execution (set on completion or failure).
    pub result: Option<String>,
}

impl SubTask {
    /// Create a new sub-task in Pending status.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            dependencies: Vec::new(),
            status: SubTaskStatus::Pending,
            result: None,
        }
    }

    /// Add a dependency on another sub-task.
    pub fn with_dependency(mut self, dep_id: impl Into<String>) -> Self {
        self.dependencies.push(dep_id.into());
        self
    }

    /// Add multiple dependencies.
    pub fn with_dependencies(mut self, dep_ids: Vec<String>) -> Self {
        self.dependencies.extend(dep_ids);
        self
    }
}

// ---------------------------------------------------------------------------
// TaskGraph
// ---------------------------------------------------------------------------

/// A directed acyclic graph (DAG) of sub-tasks representing a decomposed
/// compound goal. Tracks execution state and provides ready-task resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
    /// The original user goal that was decomposed.
    pub goal: String,
    /// All sub-tasks in the graph, keyed by ID.
    tasks: HashMap<String, SubTask>,
    /// Insertion-order list of task IDs (for deterministic iteration).
    order: Vec<String>,
}

impl TaskGraph {
    /// Create a new task graph from a goal and a list of sub-tasks.
    ///
    /// Validates that:
    /// - All dependency references point to existing sub-tasks
    /// - The dependency graph is acyclic (a valid DAG)
    ///
    /// Returns an error if validation fails.
    pub fn new(goal: &str, subtasks: Vec<SubTask>) -> Result<Self, Temm1eError> {
        let order: Vec<String> = subtasks.iter().map(|t| t.id.clone()).collect();
        let tasks: HashMap<String, SubTask> =
            subtasks.into_iter().map(|t| (t.id.clone(), t)).collect();

        // Validate all dependency references exist
        for task in tasks.values() {
            for dep_id in &task.dependencies {
                if !tasks.contains_key(dep_id) {
                    return Err(Temm1eError::Internal(format!(
                        "Sub-task '{}' depends on unknown sub-task '{}'",
                        task.id, dep_id
                    )));
                }
            }
        }

        let graph = Self {
            goal: goal.to_string(),
            tasks,
            order,
        };

        // Validate no cycles
        graph.detect_cycle()?;

        debug!(
            goal = %goal,
            subtask_count = graph.order.len(),
            "Created task graph"
        );

        Ok(graph)
    }

    /// Return sub-tasks whose dependencies are all Completed and that are
    /// themselves still Pending. These are ready to execute.
    pub fn ready_tasks(&self) -> Vec<&SubTask> {
        self.order
            .iter()
            .filter_map(|id| {
                let task = self.tasks.get(id)?;
                if task.status != SubTaskStatus::Pending {
                    return None;
                }
                let deps_met = task.dependencies.iter().all(|dep_id| {
                    self.tasks
                        .get(dep_id)
                        .is_some_and(|d| d.status == SubTaskStatus::Completed)
                });
                if deps_met {
                    Some(task)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Mark a sub-task as Running.
    pub fn mark_running(&mut self, id: &str) -> Result<(), Temm1eError> {
        let task = self.tasks.get_mut(id).ok_or_else(|| {
            Temm1eError::NotFound(format!("Sub-task '{}' not found in graph", id))
        })?;
        task.status = SubTaskStatus::Running;
        debug!(subtask_id = %id, "Sub-task marked running");
        Ok(())
    }

    /// Mark a sub-task as Completed with a result.
    pub fn mark_completed(&mut self, id: &str, result: String) -> Result<(), Temm1eError> {
        let task = self.tasks.get_mut(id).ok_or_else(|| {
            Temm1eError::NotFound(format!("Sub-task '{}' not found in graph", id))
        })?;
        task.status = SubTaskStatus::Completed;
        task.result = Some(result);
        debug!(subtask_id = %id, "Sub-task marked completed");
        Ok(())
    }

    /// Mark a sub-task as Failed with an error description.
    pub fn mark_failed(&mut self, id: &str, error: String) -> Result<(), Temm1eError> {
        let task = self.tasks.get_mut(id).ok_or_else(|| {
            Temm1eError::NotFound(format!("Sub-task '{}' not found in graph", id))
        })?;
        task.status = SubTaskStatus::Failed;
        task.result = Some(error);
        warn!(subtask_id = %id, "Sub-task marked failed");
        Ok(())
    }

    /// Check whether all sub-tasks have completed successfully.
    pub fn is_complete(&self) -> bool {
        !self.tasks.is_empty()
            && self
                .tasks
                .values()
                .all(|t| t.status == SubTaskStatus::Completed)
    }

    /// Check whether any sub-task has failed.
    pub fn is_failed(&self) -> bool {
        self.tasks
            .values()
            .any(|t| t.status == SubTaskStatus::Failed)
    }

    /// Return a human-readable progress summary (e.g. "3/5 subtasks complete").
    pub fn progress_summary(&self) -> String {
        let total = self.tasks.len();
        let completed = self
            .tasks
            .values()
            .filter(|t| t.status == SubTaskStatus::Completed)
            .count();
        let failed = self
            .tasks
            .values()
            .filter(|t| t.status == SubTaskStatus::Failed)
            .count();
        let running = self
            .tasks
            .values()
            .filter(|t| t.status == SubTaskStatus::Running)
            .count();

        if failed > 0 {
            format!(
                "{}/{} subtasks complete ({} failed, {} running)",
                completed, total, failed, running
            )
        } else if running > 0 {
            format!(
                "{}/{} subtasks complete ({} running)",
                completed, total, running
            )
        } else {
            format!("{}/{} subtasks complete", completed, total)
        }
    }

    /// Format the current graph state for injection into the agent context.
    /// Produces a prompt-friendly summary showing each sub-task, its status,
    /// dependencies, and any results so far.
    pub fn to_prompt(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "[TASK GRAPH] Goal: \"{}\"\n{}",
            self.goal,
            self.progress_summary()
        ));
        lines.push(String::new());

        for id in &self.order {
            if let Some(task) = self.tasks.get(id) {
                let status_icon = match task.status {
                    SubTaskStatus::Pending => "[ ]",
                    SubTaskStatus::Running => "[~]",
                    SubTaskStatus::Completed => "[x]",
                    SubTaskStatus::Failed => "[!]",
                };

                let deps_str = if task.dependencies.is_empty() {
                    String::new()
                } else {
                    format!(" (depends on: {})", task.dependencies.join(", "))
                };

                let result_str = match &task.result {
                    Some(r) if task.status == SubTaskStatus::Completed => {
                        // Truncate long results in the prompt
                        let truncated = if r.len() > 200 {
                            let end = r
                                .char_indices()
                                .map(|(i, _)| i)
                                .take_while(|&i| i <= 200)
                                .last()
                                .unwrap_or(0);
                            format!("{}...", &r[..end])
                        } else {
                            r.clone()
                        };
                        format!("\n     Result: {}", truncated)
                    }
                    Some(r) if task.status == SubTaskStatus::Failed => {
                        format!("\n     Error: {}", r)
                    }
                    _ => String::new(),
                };

                lines.push(format!(
                    "  {} {} — {}{}{}",
                    status_icon, task.id, task.description, deps_str, result_str
                ));
            }
        }

        lines.push(String::new());

        let ready = self.ready_tasks();
        if !ready.is_empty() {
            let ready_ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();
            lines.push(format!("Next to execute: {}", ready_ids.join(", ")));
        } else if self.is_complete() {
            lines.push("All subtasks complete.".to_string());
        } else if self.is_failed() {
            lines.push("Task graph has failures — cannot proceed.".to_string());
        }

        lines.join("\n")
    }

    /// Return all sub-tasks in topological order (respecting dependencies).
    pub fn topological_order(&self) -> Result<Vec<String>, Temm1eError> {
        topological_sort(&self.tasks)
    }

    /// Get a sub-task by ID.
    pub fn get(&self, id: &str) -> Option<&SubTask> {
        self.tasks.get(id)
    }

    /// Return the total number of sub-tasks.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Return whether the graph is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    // ── Private ───────────────────────────────────────────────────────────

    /// Detect cycles using Kahn's algorithm. Returns an error if a cycle exists.
    fn detect_cycle(&self) -> Result<(), Temm1eError> {
        // Build in-degree map
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

        for id in self.tasks.keys() {
            in_degree.entry(id.as_str()).or_insert(0);
            adj.entry(id.as_str()).or_default();
        }

        for task in self.tasks.values() {
            for dep_id in &task.dependencies {
                // dep_id -> task.id  (dep must come before task)
                adj.entry(dep_id.as_str()).or_default().push(&task.id);
                *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut visited = 0usize;

        while let Some(node) = queue.pop_front() {
            visited += 1;
            if let Some(neighbors) = adj.get(node) {
                for &neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(neighbor);
                        }
                    }
                }
            }
        }

        if visited != self.tasks.len() {
            Err(Temm1eError::Internal(
                "Task graph contains a dependency cycle — cannot create a valid execution order"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Topological sort
// ---------------------------------------------------------------------------

/// Perform a topological sort on the task map. Returns task IDs in an order
/// that respects all dependency constraints.
fn topological_sort(tasks: &HashMap<String, SubTask>) -> Result<Vec<String>, Temm1eError> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

    for id in tasks.keys() {
        in_degree.entry(id.as_str()).or_insert(0);
        adj.entry(id.as_str()).or_default();
    }

    for task in tasks.values() {
        for dep_id in &task.dependencies {
            adj.entry(dep_id.as_str()).or_default().push(&task.id);
            *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
        }
    }

    // Start with zero in-degree nodes, sorted for determinism
    let mut queue: VecDeque<&str> = {
        let mut zeros: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        zeros.sort();
        zeros.into_iter().collect()
    };

    let mut result = Vec::with_capacity(tasks.len());

    while let Some(node) = queue.pop_front() {
        result.push(node.to_string());
        if let Some(neighbors) = adj.get(node) {
            let mut next: Vec<&str> = Vec::new();
            for &neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        next.push(neighbor);
                    }
                }
            }
            next.sort();
            for n in next {
                queue.push_back(n);
            }
        }
    }

    if result.len() != tasks.len() {
        Err(Temm1eError::Internal(
            "Task graph contains a dependency cycle".to_string(),
        ))
    } else {
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Prompt generation
// ---------------------------------------------------------------------------

/// Generate a prompt asking the LLM to decompose a compound goal into
/// numbered sub-tasks with dependencies.
///
/// This returns the prompt text — it does NOT call the LLM. The caller is
/// responsible for sending this prompt to the provider and passing the
/// response to `parse_decomposition()`.
pub fn decompose_prompt(goal: &str) -> String {
    format!(
        "Break down the following compound task into sequential sub-tasks.\n\
         \n\
         Task: \"{goal}\"\n\
         \n\
         Respond with a numbered list of sub-tasks. Each sub-task should be a single, \
         concrete action. If a sub-task depends on a previous one, note it with \
         \"(after N)\" where N is the step number it depends on.\n\
         \n\
         Format:\n\
         1. First action\n\
         2. Second action (after 1)\n\
         3. Third action (after 2)\n\
         4. Fourth action (after 2)\n\
         \n\
         Keep it simple — most tasks are linear chains. Only use parallel branching \
         when steps are truly independent.\n\
         \n\
         Respond ONLY with the numbered list, no preamble or explanation."
    )
}

/// Parse the LLM's numbered-list response into a vector of `SubTask` structs.
///
/// Expected format:
/// ```text
/// 1. Deploy the application
/// 2. Run database migrations (after 1)
/// 3. Verify health check (after 2)
/// 4. Send deployment logs (after 3)
/// ```
///
/// If parsing fails or produces no tasks, falls back to a single sub-task
/// containing the original goal.
pub fn parse_decomposition(llm_response: &str, goal: &str) -> Result<Vec<SubTask>, Temm1eError> {
    let mut subtasks: Vec<SubTask> = Vec::new();

    for line in llm_response.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try to match lines like "1. Description" or "1) Description"
        // Also handle "- 1. Description" or "  1. Description"
        let parsed = parse_numbered_line(trimmed);
        if let Some((number, description, deps)) = parsed {
            let id = number.to_string();
            let mut task = SubTask::new(id, description);
            task.dependencies = deps.into_iter().map(|d| d.to_string()).collect();
            subtasks.push(task);
        }
    }

    // Fallback: if parsing produced nothing, treat the whole goal as one task
    if subtasks.is_empty() {
        warn!("Failed to parse LLM decomposition — falling back to single task");
        subtasks.push(SubTask::new("1", goal));
    }

    debug!(subtask_count = subtasks.len(), "Parsed task decomposition");

    Ok(subtasks)
}

/// Parse a single numbered line, extracting the step number, description,
/// and any dependency references.
///
/// Returns `(step_number, description, dependency_numbers)` or `None`.
fn parse_numbered_line(line: &str) -> Option<(u32, String, Vec<u32>)> {
    let trimmed = line.trim().trim_start_matches('-').trim();

    // Match "N. description" or "N) description"
    let (num_str, rest) = if let Some(dot_pos) = trimmed.find(". ") {
        let candidate = &trimmed[..dot_pos];
        if candidate.chars().all(|c| c.is_ascii_digit()) && !candidate.is_empty() {
            (candidate, &trimmed[dot_pos + 2..])
        } else {
            return None;
        }
    } else if let Some(paren_pos) = trimmed.find(") ") {
        let candidate = &trimmed[..paren_pos];
        if candidate.chars().all(|c| c.is_ascii_digit()) && !candidate.is_empty() {
            (candidate, &trimmed[paren_pos + 2..])
        } else {
            return None;
        }
    } else {
        return None;
    };

    let number: u32 = num_str.parse().ok()?;

    // Extract dependency references: "(after N)", "(after N, M)", "(depends on N)"
    let (description, deps) = extract_dependencies(rest);

    Some((number, description, deps))
}

/// Extract dependency annotations from a description string.
///
/// Supports formats:
/// - `(after 1)`
/// - `(after 1, 2)`
/// - `(after 1 and 2)`
/// - `(depends on 1)`
/// - `(depends on 1, 2)`
///
/// Returns `(clean_description, dependency_numbers)`.
fn extract_dependencies(text: &str) -> (String, Vec<u32>) {
    let mut deps = Vec::new();
    let mut description = text.to_string();

    // Find parenthesized dependency annotations
    // Look for patterns like "(after N)", "(after N, M)", "(depends on N)"
    let patterns = ["(after ", "(depends on "];

    for pattern in &patterns {
        let lower = description.to_lowercase();
        if let Some(start) = lower.find(pattern) {
            if let Some(end) = description[start..].find(')') {
                let annotation = &description[start..start + end + 1];
                let inner = &annotation[pattern.len()..annotation.len() - 1];

                // Parse numbers from the inner text
                // Handle "1", "1, 2", "1 and 2", "1, 2, and 3"
                for part in inner.split([',', ' ']) {
                    let part = part.trim();
                    if part == "and" || part == "step" || part == "steps" || part.is_empty() {
                        continue;
                    }
                    if let Ok(n) = part.parse::<u32>() {
                        deps.push(n);
                    }
                }

                // Remove the annotation from the description
                description = format!(
                    "{}{}",
                    description[..start].trim(),
                    description[start + end + 1..].trim()
                )
                .trim()
                .to_string();
            }
        }
    }

    // If no explicit dependencies and the step number > 1, we assume linear
    // chaining — but we do NOT add implicit dependencies here. The caller
    // can use `create_linear_chain()` for that pattern.

    (description, deps)
}

/// Convenience: create a linear chain of sub-tasks from a list of descriptions.
/// Each task depends on the previous one.
pub fn create_linear_chain(descriptions: Vec<String>) -> Vec<SubTask> {
    descriptions
        .into_iter()
        .enumerate()
        .map(|(i, desc)| {
            let id = (i + 1).to_string();
            let mut task = SubTask::new(id, desc);
            if i > 0 {
                task.dependencies.push(i.to_string());
            }
            task
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── SubTask creation ──────────────────────────────────────────────

    #[test]
    fn subtask_new_defaults() {
        let task = SubTask::new("1", "Deploy the app");
        assert_eq!(task.id, "1");
        assert_eq!(task.description, "Deploy the app");
        assert!(task.dependencies.is_empty());
        assert_eq!(task.status, SubTaskStatus::Pending);
        assert!(task.result.is_none());
    }

    #[test]
    fn subtask_with_dependency() {
        let task = SubTask::new("2", "Run migrations").with_dependency("1");
        assert_eq!(task.dependencies, vec!["1"]);
    }

    #[test]
    fn subtask_with_multiple_dependencies() {
        let task = SubTask::new("3", "Verify").with_dependencies(vec!["1".into(), "2".into()]);
        assert_eq!(task.dependencies, vec!["1", "2"]);
    }

    // ── TaskGraph construction ────────────────────────────────────────

    #[test]
    fn graph_creation_linear() {
        let tasks = vec![
            SubTask::new("1", "Deploy"),
            SubTask::new("2", "Migrate").with_dependency("1"),
            SubTask::new("3", "Verify").with_dependency("2"),
        ];
        let graph = TaskGraph::new("deploy and verify", tasks).unwrap();
        assert_eq!(graph.len(), 3);
        assert!(!graph.is_empty());
    }

    #[test]
    fn graph_creation_empty() {
        let graph = TaskGraph::new("nothing", vec![]).unwrap();
        assert!(graph.is_empty());
        assert_eq!(graph.len(), 0);
        // Empty graph is technically "complete" (vacuously true? no — is_complete
        // requires non-empty)
        assert!(!graph.is_complete());
    }

    #[test]
    fn graph_rejects_missing_dependency() {
        let tasks = vec![SubTask::new("1", "Deploy").with_dependency("999")];
        let result = TaskGraph::new("bad deps", tasks);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("999"));
    }

    #[test]
    fn graph_rejects_cycle() {
        let tasks = vec![
            SubTask::new("1", "A").with_dependency("2"),
            SubTask::new("2", "B").with_dependency("1"),
        ];
        let result = TaskGraph::new("cycle", tasks);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cycle"));
    }

    #[test]
    fn graph_rejects_self_cycle() {
        let tasks = vec![SubTask::new("1", "Self-loop").with_dependency("1")];
        let result = TaskGraph::new("self-cycle", tasks);
        assert!(result.is_err());
    }

    #[test]
    fn graph_rejects_indirect_cycle() {
        let tasks = vec![
            SubTask::new("1", "A").with_dependency("3"),
            SubTask::new("2", "B").with_dependency("1"),
            SubTask::new("3", "C").with_dependency("2"),
        ];
        let result = TaskGraph::new("indirect cycle", tasks);
        assert!(result.is_err());
    }

    // ── Ready tasks ──────────────────────────────────────────────────

    #[test]
    fn ready_tasks_initial_state() {
        let tasks = vec![
            SubTask::new("1", "Deploy"),
            SubTask::new("2", "Migrate").with_dependency("1"),
            SubTask::new("3", "Verify").with_dependency("2"),
        ];
        let graph = TaskGraph::new("goal", tasks).unwrap();
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "1");
    }

    #[test]
    fn ready_tasks_after_first_complete() {
        let tasks = vec![
            SubTask::new("1", "Deploy"),
            SubTask::new("2", "Migrate").with_dependency("1"),
            SubTask::new("3", "Verify").with_dependency("1"),
        ];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_completed("1", "deployed".into()).unwrap();

        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 2);
        let ready_ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();
        assert!(ready_ids.contains(&"2"));
        assert!(ready_ids.contains(&"3"));
    }

    #[test]
    fn ready_tasks_no_deps_all_ready() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B"),
            SubTask::new("3", "C"),
        ];
        let graph = TaskGraph::new("parallel", tasks).unwrap();
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn ready_tasks_running_not_included() {
        let tasks = vec![SubTask::new("1", "A"), SubTask::new("2", "B")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_running("1").unwrap();

        let ready = graph.ready_tasks();
        // Only task 2 should be ready (no deps), task 1 is Running not Pending
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "2");
    }

    // ── Mark operations ──────────────────────────────────────────────

    #[test]
    fn mark_running_valid() {
        let tasks = vec![SubTask::new("1", "Deploy")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_running("1").unwrap();
        assert_eq!(graph.get("1").unwrap().status, SubTaskStatus::Running);
    }

    #[test]
    fn mark_completed_valid() {
        let tasks = vec![SubTask::new("1", "Deploy")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph
            .mark_completed("1", "deployment successful".into())
            .unwrap();
        let task = graph.get("1").unwrap();
        assert_eq!(task.status, SubTaskStatus::Completed);
        assert_eq!(task.result.as_deref(), Some("deployment successful"));
    }

    #[test]
    fn mark_failed_valid() {
        let tasks = vec![SubTask::new("1", "Deploy")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_failed("1", "connection refused".into()).unwrap();
        let task = graph.get("1").unwrap();
        assert_eq!(task.status, SubTaskStatus::Failed);
        assert_eq!(task.result.as_deref(), Some("connection refused"));
    }

    #[test]
    fn mark_nonexistent_returns_error() {
        let tasks = vec![SubTask::new("1", "Deploy")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        assert!(graph.mark_running("999").is_err());
        assert!(graph.mark_completed("999", "ok".into()).is_err());
        assert!(graph.mark_failed("999", "err".into()).is_err());
    }

    // ── Completion / failure checks ──────────────────────────────────

    #[test]
    fn is_complete_all_done() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B").with_dependency("1"),
        ];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        assert!(!graph.is_complete());

        graph.mark_completed("1", "ok".into()).unwrap();
        assert!(!graph.is_complete());

        graph.mark_completed("2", "ok".into()).unwrap();
        assert!(graph.is_complete());
    }

    #[test]
    fn is_failed_any_failed() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B").with_dependency("1"),
        ];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        assert!(!graph.is_failed());

        graph.mark_failed("1", "boom".into()).unwrap();
        assert!(graph.is_failed());
    }

    // ── Progress summary ─────────────────────────────────────────────

    #[test]
    fn progress_summary_initial() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B"),
            SubTask::new("3", "C"),
        ];
        let graph = TaskGraph::new("goal", tasks).unwrap();
        assert_eq!(graph.progress_summary(), "0/3 subtasks complete");
    }

    #[test]
    fn progress_summary_partial() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B"),
            SubTask::new("3", "C"),
        ];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_completed("1", "ok".into()).unwrap();
        graph.mark_running("2").unwrap();
        assert_eq!(
            graph.progress_summary(),
            "1/3 subtasks complete (1 running)"
        );
    }

    #[test]
    fn progress_summary_with_failure() {
        let tasks = vec![SubTask::new("1", "A"), SubTask::new("2", "B")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_completed("1", "ok".into()).unwrap();
        graph.mark_failed("2", "err".into()).unwrap();
        assert_eq!(
            graph.progress_summary(),
            "1/2 subtasks complete (1 failed, 0 running)"
        );
    }

    #[test]
    fn progress_summary_all_complete() {
        let tasks = vec![SubTask::new("1", "A"), SubTask::new("2", "B")];
        let mut graph = TaskGraph::new("goal", tasks).unwrap();
        graph.mark_completed("1", "ok".into()).unwrap();
        graph.mark_completed("2", "ok".into()).unwrap();
        assert_eq!(graph.progress_summary(), "2/2 subtasks complete");
    }

    // ── to_prompt ────────────────────────────────────────────────────

    #[test]
    fn to_prompt_shows_structure() {
        let tasks = vec![
            SubTask::new("1", "Deploy the app"),
            SubTask::new("2", "Run migrations").with_dependency("1"),
            SubTask::new("3", "Verify health").with_dependency("2"),
        ];
        let mut graph = TaskGraph::new("deploy and verify", tasks).unwrap();
        graph
            .mark_completed("1", "deployed to prod".into())
            .unwrap();

        let prompt = graph.to_prompt();
        assert!(prompt.contains("[TASK GRAPH]"));
        assert!(prompt.contains("deploy and verify"));
        assert!(prompt.contains("[x] 1"));
        assert!(prompt.contains("[ ] 2"));
        assert!(prompt.contains("[ ] 3"));
        assert!(prompt.contains("depends on: 1"));
        assert!(prompt.contains("Next to execute: 2"));
    }

    #[test]
    fn to_prompt_complete_graph() {
        let tasks = vec![SubTask::new("1", "Do thing")];
        let mut graph = TaskGraph::new("simple", tasks).unwrap();
        graph.mark_completed("1", "done".into()).unwrap();

        let prompt = graph.to_prompt();
        assert!(prompt.contains("All subtasks complete"));
    }

    #[test]
    fn to_prompt_failed_graph() {
        let tasks = vec![
            SubTask::new("1", "First"),
            SubTask::new("2", "Second").with_dependency("1"),
        ];
        let mut graph = TaskGraph::new("failing", tasks).unwrap();
        graph.mark_failed("1", "connection refused".into()).unwrap();

        let prompt = graph.to_prompt();
        assert!(prompt.contains("[!] 1"));
        assert!(prompt.contains("connection refused"));
        assert!(prompt.contains("failures"));
    }

    // ── Topological sort ─────────────────────────────────────────────

    #[test]
    fn topological_order_linear() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B").with_dependency("1"),
            SubTask::new("3", "C").with_dependency("2"),
        ];
        let graph = TaskGraph::new("linear", tasks).unwrap();
        let order = graph.topological_order().unwrap();
        assert_eq!(order, vec!["1", "2", "3"]);
    }

    #[test]
    fn topological_order_diamond() {
        // 1 -> 2, 1 -> 3, 2 -> 4, 3 -> 4
        let tasks = vec![
            SubTask::new("1", "Root"),
            SubTask::new("2", "Left").with_dependency("1"),
            SubTask::new("3", "Right").with_dependency("1"),
            SubTask::new("4", "Join").with_dependencies(vec!["2".into(), "3".into()]),
        ];
        let graph = TaskGraph::new("diamond", tasks).unwrap();
        let order = graph.topological_order().unwrap();

        // 1 must come first, 4 must come last, 2 and 3 in between
        assert_eq!(order[0], "1");
        assert_eq!(order[3], "4");
        assert!(order.contains(&"2".to_string()));
        assert!(order.contains(&"3".to_string()));
    }

    #[test]
    fn topological_order_all_independent() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B"),
            SubTask::new("3", "C"),
        ];
        let graph = TaskGraph::new("parallel", tasks).unwrap();
        let order = graph.topological_order().unwrap();
        assert_eq!(order.len(), 3);
        // All should be present
        assert!(order.contains(&"1".to_string()));
        assert!(order.contains(&"2".to_string()));
        assert!(order.contains(&"3".to_string()));
    }

    // ── Prompt generation ────────────────────────────────────────────

    #[test]
    fn decompose_prompt_contains_goal() {
        let prompt = decompose_prompt("deploy the app and run tests");
        assert!(prompt.contains("deploy the app and run tests"));
        assert!(prompt.contains("numbered list"));
        assert!(prompt.contains("(after N)"));
    }

    // ── Response parsing ─────────────────────────────────────────────

    #[test]
    fn parse_simple_numbered_list() {
        let response = "\
1. Deploy the application
2. Run database migrations (after 1)
3. Verify health check (after 2)
4. Send deployment logs (after 3)";

        let tasks = parse_decomposition(response, "deploy and verify").unwrap();
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[0].description, "Deploy the application");
        assert!(tasks[0].dependencies.is_empty());

        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[1].description, "Run database migrations");
        assert_eq!(tasks[1].dependencies, vec!["1"]);

        assert_eq!(tasks[2].id, "3");
        assert_eq!(tasks[2].dependencies, vec!["2"]);

        assert_eq!(tasks[3].id, "4");
        assert_eq!(tasks[3].dependencies, vec!["3"]);
    }

    #[test]
    fn parse_with_multiple_deps() {
        let response = "\
1. Build frontend
2. Build backend
3. Run integration tests (after 1, 2)";

        let tasks = parse_decomposition(response, "build and test").unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[2].dependencies, vec!["1", "2"]);
    }

    #[test]
    fn parse_with_depends_on_syntax() {
        let response = "\
1. First step
2. Second step (depends on 1)";

        let tasks = parse_decomposition(response, "two steps").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].dependencies, vec!["1"]);
    }

    #[test]
    fn parse_with_paren_numbering() {
        let response = "\
1) Deploy the app
2) Run migrations (after 1)";

        let tasks = parse_decomposition(response, "deploy").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].description, "Deploy the app");
        assert_eq!(tasks[1].dependencies, vec!["1"]);
    }

    #[test]
    fn parse_fallback_on_garbage() {
        let response = "I can't decompose this into steps. Just do the whole thing.";
        let tasks = parse_decomposition(response, "original goal").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[0].description, "original goal");
    }

    #[test]
    fn parse_fallback_on_empty() {
        let tasks = parse_decomposition("", "original goal").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].description, "original goal");
    }

    #[test]
    fn parse_ignores_blank_lines() {
        let response = "\n\n1. Step one\n\n2. Step two\n\n";
        let tasks = parse_decomposition(response, "goal").unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn parse_with_and_syntax() {
        let response = "\
1. Build the project
2. Test the project
3. Deploy (after 1 and 2)";

        let tasks = parse_decomposition(response, "build test deploy").unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[2].dependencies, vec!["1", "2"]);
    }

    // ── Linear chain helper ──────────────────────────────────────────

    #[test]
    fn create_linear_chain_basic() {
        let descs = vec![
            "Deploy".to_string(),
            "Migrate".to_string(),
            "Verify".to_string(),
        ];
        let tasks = create_linear_chain(descs);
        assert_eq!(tasks.len(), 3);

        assert_eq!(tasks[0].id, "1");
        assert!(tasks[0].dependencies.is_empty());

        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[1].dependencies, vec!["1"]);

        assert_eq!(tasks[2].id, "3");
        assert_eq!(tasks[2].dependencies, vec!["2"]);

        // Verify the chain forms a valid graph
        let graph = TaskGraph::new("linear chain", tasks).unwrap();
        assert_eq!(graph.len(), 3);
    }

    #[test]
    fn create_linear_chain_single() {
        let tasks = create_linear_chain(vec!["Only step".to_string()]);
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].dependencies.is_empty());
    }

    #[test]
    fn create_linear_chain_empty() {
        let tasks = create_linear_chain(vec![]);
        assert!(tasks.is_empty());
    }

    // ── Serialization roundtrip ──────────────────────────────────────

    #[test]
    fn subtask_serde_roundtrip() {
        let task = SubTask::new("1", "Deploy").with_dependency("0");
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: SubTask = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "1");
        assert_eq!(deserialized.description, "Deploy");
        assert_eq!(deserialized.dependencies, vec!["0"]);
        assert_eq!(deserialized.status, SubTaskStatus::Pending);
    }

    #[test]
    fn task_graph_serde_roundtrip() {
        let tasks = vec![
            SubTask::new("1", "A"),
            SubTask::new("2", "B").with_dependency("1"),
        ];
        let mut graph = TaskGraph::new("test goal", tasks).unwrap();
        graph.mark_completed("1", "done".into()).unwrap();

        let json = serde_json::to_string(&graph).unwrap();
        let deserialized: TaskGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.goal, "test goal");
        assert_eq!(deserialized.len(), 2);
        assert_eq!(
            deserialized.get("1").unwrap().status,
            SubTaskStatus::Completed
        );
    }

    #[test]
    fn subtask_status_display() {
        assert_eq!(SubTaskStatus::Pending.to_string(), "pending");
        assert_eq!(SubTaskStatus::Running.to_string(), "running");
        assert_eq!(SubTaskStatus::Completed.to_string(), "completed");
        assert_eq!(SubTaskStatus::Failed.to_string(), "failed");
    }

    // ── Integration: parse -> graph -> execute ───────────────────────

    #[test]
    fn end_to_end_parse_and_execute() {
        let llm_response = "\
1. Deploy the application to production
2. Run database migrations (after 1)
3. Verify health endpoint returns 200 (after 2)
4. Send deployment logs to the user (after 3)";

        let subtasks =
            parse_decomposition(llm_response, "deploy, migrate, verify, send logs").unwrap();
        let mut graph = TaskGraph::new("deploy, migrate, verify, send logs", subtasks).unwrap();

        // Simulate execution
        assert_eq!(graph.progress_summary(), "0/4 subtasks complete");

        // Step 1: only task 1 is ready
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "1");

        graph.mark_running("1").unwrap();
        graph
            .mark_completed("1", "Deployed to prod successfully".into())
            .unwrap();
        assert_eq!(graph.progress_summary(), "1/4 subtasks complete");

        // Step 2: task 2 is now ready
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "2");

        graph.mark_running("2").unwrap();
        graph
            .mark_completed("2", "Migrations applied".into())
            .unwrap();

        // Step 3
        let ready = graph.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "3");

        graph.mark_running("3").unwrap();
        graph
            .mark_completed("3", "Health check 200 OK".into())
            .unwrap();

        // Step 4
        graph.mark_running("4").unwrap();
        graph
            .mark_completed("4", "Logs sent to user".into())
            .unwrap();

        assert!(graph.is_complete());
        assert!(!graph.is_failed());
        assert_eq!(graph.progress_summary(), "4/4 subtasks complete");
    }

    #[test]
    fn end_to_end_failure_blocks_dependents() {
        let subtasks = vec![
            SubTask::new("1", "Deploy"),
            SubTask::new("2", "Migrate").with_dependency("1"),
            SubTask::new("3", "Verify").with_dependency("2"),
        ];
        let mut graph = TaskGraph::new("deploy pipeline", subtasks).unwrap();

        graph.mark_running("1").unwrap();
        graph.mark_failed("1", "Connection refused".into()).unwrap();

        // Task 2 should NOT be ready (dep 1 failed, not completed)
        let ready = graph.ready_tasks();
        assert!(ready.is_empty());

        assert!(graph.is_failed());
        assert!(!graph.is_complete());

        let prompt = graph.to_prompt();
        assert!(prompt.contains("failures"));
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn graph_with_diamond_deps() {
        //   1
        //  / \
        // 2   3
        //  \ /
        //   4
        let tasks = vec![
            SubTask::new("1", "Root"),
            SubTask::new("2", "Left").with_dependency("1"),
            SubTask::new("3", "Right").with_dependency("1"),
            SubTask::new("4", "Join").with_dependencies(vec!["2".into(), "3".into()]),
        ];
        let mut graph = TaskGraph::new("diamond", tasks).unwrap();

        // Only 1 is ready initially
        assert_eq!(graph.ready_tasks().len(), 1);
        graph.mark_completed("1", "ok".into()).unwrap();

        // Now 2 and 3 are ready (parallel)
        assert_eq!(graph.ready_tasks().len(), 2);
        graph.mark_completed("2", "ok".into()).unwrap();

        // 4 not ready yet — 3 still pending
        assert_eq!(graph.ready_tasks().len(), 1);
        assert_eq!(graph.ready_tasks()[0].id, "3");

        graph.mark_completed("3", "ok".into()).unwrap();

        // Now 4 is ready
        assert_eq!(graph.ready_tasks().len(), 1);
        assert_eq!(graph.ready_tasks()[0].id, "4");

        graph.mark_completed("4", "ok".into()).unwrap();
        assert!(graph.is_complete());
    }

    #[test]
    fn get_nonexistent_task() {
        let graph = TaskGraph::new("empty", vec![]).unwrap();
        assert!(graph.get("nope").is_none());
    }

    #[test]
    fn to_prompt_truncates_long_results() {
        let tasks = vec![SubTask::new("1", "Generate report")];
        let mut graph = TaskGraph::new("report", tasks).unwrap();
        let long_result = "x".repeat(500);
        graph.mark_completed("1", long_result).unwrap();

        let prompt = graph.to_prompt();
        assert!(prompt.contains("..."));
        // Should be truncated to ~200 chars
        assert!(prompt.len() < 800);
    }
}
