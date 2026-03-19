//! Hive Worker — executes individual tasks from the Blackboard.
//!
//! Each worker runs a loop: select task → claim → execute → complete/fail → repeat.
//! Workers carry task-scoped context (NOT full conversation history), which is
//! the source of the quadratic→linear cost savings.

use std::collections::HashMap;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use temm1e_core::types::error::Temm1eError;

use crate::blackboard::Blackboard;
use crate::config::HiveConfig;
use crate::pheromone::PheromoneField;
use crate::selection::TaskSelector;
use crate::types::{HiveTask, SignalType, WorkerState};

// ---------------------------------------------------------------------------
// Task Result
// ---------------------------------------------------------------------------

/// The outcome of executing a single task.
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub summary: String,
    pub tokens_used: u32,
    pub artifacts: Vec<String>,
    pub success: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// HiveWorker
// ---------------------------------------------------------------------------

/// A worker in the Hive swarm. Executes tasks from the Blackboard.
pub struct HiveWorker {
    pub state: WorkerState,
    config: HiveConfig,
}

impl HiveWorker {
    pub fn new(id: String, config: HiveConfig) -> Self {
        Self {
            state: WorkerState::new(id),
            config,
        }
    }

    /// Main worker loop: select → claim → execute → complete/fail → repeat.
    ///
    /// The `execute_fn` parameter is a closure that actually runs the task
    /// against an AI provider. This keeps the worker decoupled from the
    /// provider/tool system (testable with mocks).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_loop<F, Fut>(
        &mut self,
        order_id: &str,
        blackboard: &Blackboard,
        pheromones: &PheromoneField,
        selector: &TaskSelector,
        total_tasks: usize,
        dependent_counts: &HashMap<String, usize>,
        cancel: CancellationToken,
        execute_fn: F,
    ) -> Result<WorkerStats, Temm1eError>
    where
        F: Fn(HiveTask, Vec<(String, String)>) -> Fut,
        Fut: std::future::Future<Output = Result<TaskResult, Temm1eError>>,
    {
        let mut stats = WorkerStats {
            worker_id: self.state.id.clone(),
            tasks_completed: 0,
            tasks_failed: 0,
            total_tokens: 0,
        };

        loop {
            // Check cancellation
            if cancel.is_cancelled() {
                info!(worker = %self.state.id, "Worker cancelled");
                break;
            }

            // Get READY tasks
            let ready = blackboard.get_ready_tasks(order_id).await?;
            if ready.is_empty() {
                // Check if order is complete
                if blackboard.is_order_complete(order_id).await? {
                    info!(worker = %self.state.id, "Order complete, worker exiting");
                    break;
                }
                // Wait briefly then retry (other workers may complete tasks that unblock new ones)
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(500)) => {},
                    _ = cancel.cancelled() => break,
                }
                continue;
            }

            // Select best task
            let task_id = match selector
                .select_task(
                    &self.state,
                    &ready,
                    pheromones,
                    total_tasks,
                    dependent_counts,
                )
                .await
            {
                Some(id) => id,
                None => {
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(500)) => {},
                        _ = cancel.cancelled() => break,
                    }
                    continue;
                }
            };

            // Claim task (atomic)
            let claimed = blackboard.claim_task(&task_id, &self.state.id).await?;
            if !claimed {
                // Another worker got it first — try again
                debug!(worker = %self.state.id, task = %task_id, "Claim failed, retrying");
                continue;
            }

            self.state.current_task = Some(task_id.clone());

            // Emit progress pheromone
            let _ = pheromones
                .emit_default(SignalType::Progress, &task_id, Some(&self.state.id))
                .await;

            // Get task details and dependency results
            let task = blackboard.get_task(&task_id).await?.ok_or_else(|| {
                Temm1eError::Internal(format!("Claimed task vanished: {task_id}"))
            })?;

            let dep_results = blackboard.get_dependency_results(&task_id).await?;

            // Execute with timeout
            let timeout =
                tokio::time::Duration::from_secs(self.config.blocker.max_task_duration_secs);

            let execution_start = Instant::now();
            let result = tokio::select! {
                r = execute_fn(task.clone(), dep_results) => r,
                _ = tokio::time::sleep(timeout) => {
                    Err(Temm1eError::Internal(format!(
                        "Task {task_id} timed out after {}s",
                        self.config.blocker.max_task_duration_secs
                    )))
                },
                _ = cancel.cancelled() => {
                    info!(worker = %self.state.id, task = %task_id, "Cancelled during execution");
                    break;
                }
            };
            let elapsed_ms = execution_start.elapsed().as_millis() as u64;

            match result {
                Ok(task_result) if task_result.success => {
                    // Complete the task
                    let newly_ready = blackboard
                        .complete_task(&task_id, &task_result.summary, task_result.tokens_used)
                        .await?;

                    // Emit completion pheromone
                    let _ = pheromones
                        .emit_default(SignalType::Completion, &task_id, Some(&self.state.id))
                        .await;

                    // Update worker state
                    for tag in &task.context_tags {
                        self.state.recent_tags.insert(tag.clone());
                    }
                    self.state.tasks_completed += 1;
                    self.state.tokens_used += task_result.tokens_used as u64;
                    stats.tasks_completed += 1;
                    stats.total_tokens += task_result.tokens_used as u64;

                    info!(
                        worker = %self.state.id,
                        task = %task_id,
                        tokens = task_result.tokens_used,
                        elapsed_ms = elapsed_ms,
                        unblocked = newly_ready.len(),
                        "Task completed"
                    );
                }
                Ok(task_result) => {
                    // Task executed but reported failure
                    let err = task_result.error.unwrap_or_else(|| "Unknown error".into());
                    let new_status = blackboard.fail_task(&task_id, &err).await?;

                    let _ = pheromones
                        .emit_default(SignalType::Failure, &task_id, Some(&self.state.id))
                        .await;

                    stats.tasks_failed += 1;
                    stats.total_tokens += task_result.tokens_used as u64;

                    warn!(
                        worker = %self.state.id,
                        task = %task_id,
                        new_status = new_status.as_str(),
                        error = %err,
                        "Task failed"
                    );
                }
                Err(e) => {
                    // Execution error (panic, timeout, etc.)
                    let _new_status = blackboard.fail_task(&task_id, &e.to_string()).await?;

                    let _ = pheromones
                        .emit(
                            SignalType::Difficulty,
                            &task_id,
                            1.0,
                            0.006,
                            Some(&self.state.id),
                            None,
                        )
                        .await;

                    stats.tasks_failed += 1;

                    error!(
                        worker = %self.state.id,
                        task = %task_id,
                        error = %e,
                        "Task execution error"
                    );
                }
            }

            self.state.current_task = None;
        }

        Ok(stats)
    }
}

/// Build the task-scoped context messages for a worker.
///
/// This is where the quadratic→linear cost savings happen:
/// instead of full conversation history, the worker gets only
/// the task description + dependency results.
pub fn build_scoped_context(task: &HiveTask, dependency_results: &[(String, String)]) -> String {
    let mut context = String::new();

    if !dependency_results.is_empty() {
        context.push_str("## Context from completed prerequisite tasks:\n\n");
        for (dep_id, result) in dependency_results {
            context.push_str(&format!("### Result from task {dep_id}:\n{result}\n\n"));
        }
        context.push_str("---\n\n");
    }

    context.push_str("## Your task:\n\n");
    context.push_str(&task.description);

    context
}

// ---------------------------------------------------------------------------
// Worker Stats
// ---------------------------------------------------------------------------

/// Statistics from a worker's execution.
#[derive(Debug, Clone)]
pub struct WorkerStats {
    pub worker_id: String,
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_tokens: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HiveTaskStatus;

    #[test]
    fn scoped_context_no_deps() {
        let task = HiveTask {
            id: "t1".into(),
            order_id: "o1".into(),
            description: "Build the API".into(),
            status: HiveTaskStatus::Active,
            claimed_by: Some("w1".into()),
            dependencies: vec![],
            context_tags: vec!["rust".into()],
            estimated_tokens: 1000,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 3,
            error_log: None,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let ctx = build_scoped_context(&task, &[]);
        assert!(ctx.contains("Build the API"));
        assert!(!ctx.contains("prerequisite"));
    }

    #[test]
    fn scoped_context_with_deps() {
        let task = HiveTask {
            id: "t2".into(),
            order_id: "o1".into(),
            description: "Implement CRUD endpoints".into(),
            status: HiveTaskStatus::Active,
            claimed_by: Some("w1".into()),
            dependencies: vec!["t1".into()],
            context_tags: vec!["api".into()],
            estimated_tokens: 2000,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 3,
            error_log: None,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let deps = vec![(
            "t1".to_string(),
            "Schema created with users table".to_string(),
        )];
        let ctx = build_scoped_context(&task, &deps);
        assert!(ctx.contains("Schema created with users table"));
        assert!(ctx.contains("Implement CRUD endpoints"));
        assert!(ctx.contains("prerequisite"));
    }

    #[tokio::test]
    async fn worker_completes_single_task() {
        let config = HiveConfig::default();
        let bb = Blackboard::new("sqlite::memory:").await.unwrap();
        let pheromones =
            PheromoneField::new("sqlite::memory:", crate::config::PheromoneConfig::default())
                .await
                .unwrap();
        let selector = TaskSelector::new(&config.selection);
        let cancel = CancellationToken::new();

        // Create order with one task
        let order = crate::types::HiveOrder {
            id: "o1".into(),
            chat_id: "c1".into(),
            original_message: "test".into(),
            task_count: 1,
            completed_count: 0,
            status: crate::types::HiveOrderStatus::Active,
            total_tokens: 0,
            queen_tokens: 0,
            created_at: chrono::Utc::now().timestamp_millis(),
            completed_at: None,
        };
        bb.create_order(&order).await.unwrap();

        let task = HiveTask {
            id: "t1".into(),
            order_id: "o1".into(),
            description: "Test task".into(),
            status: HiveTaskStatus::Pending,
            claimed_by: None,
            dependencies: vec![],
            context_tags: vec![],
            estimated_tokens: 100,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 3,
            error_log: None,
            created_at: chrono::Utc::now().timestamp_millis(),
            started_at: None,
            completed_at: None,
        };
        bb.create_tasks(&[task]).await.unwrap();

        let dep_counts = HashMap::new();
        let mut worker = HiveWorker::new("w1".into(), config);

        // Execute with a mock function that always succeeds
        let stats = worker
            .run_loop(
                "o1",
                &bb,
                &pheromones,
                &selector,
                1,
                &dep_counts,
                cancel,
                |_task, _deps| async {
                    Ok(TaskResult {
                        summary: "Done!".into(),
                        tokens_used: 50,
                        artifacts: vec![],
                        success: true,
                        error: None,
                    })
                },
            )
            .await
            .unwrap();

        assert_eq!(stats.tasks_completed, 1);
        assert_eq!(stats.tasks_failed, 0);
        assert_eq!(stats.total_tokens, 50);

        // Verify task is complete on blackboard
        assert!(bb.is_order_complete("o1").await.unwrap());
    }

    #[tokio::test]
    async fn worker_handles_failure_and_retry() {
        let config = HiveConfig::default();
        let bb = Blackboard::new("sqlite::memory:").await.unwrap();
        let pheromones =
            PheromoneField::new("sqlite::memory:", crate::config::PheromoneConfig::default())
                .await
                .unwrap();
        let selector = TaskSelector::new(&config.selection);
        let cancel = CancellationToken::new();

        let order = crate::types::HiveOrder {
            id: "o1".into(),
            chat_id: "c1".into(),
            original_message: "test".into(),
            task_count: 1,
            completed_count: 0,
            status: crate::types::HiveOrderStatus::Active,
            total_tokens: 0,
            queen_tokens: 0,
            created_at: chrono::Utc::now().timestamp_millis(),
            completed_at: None,
        };
        bb.create_order(&order).await.unwrap();

        let task = HiveTask {
            id: "t1".into(),
            order_id: "o1".into(),
            description: "Failing task".into(),
            status: HiveTaskStatus::Pending,
            claimed_by: None,
            dependencies: vec![],
            context_tags: vec![],
            estimated_tokens: 100,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 2,
            error_log: None,
            created_at: chrono::Utc::now().timestamp_millis(),
            started_at: None,
            completed_at: None,
        };
        bb.create_tasks(&[task]).await.unwrap();

        let dep_counts = HashMap::new();
        let mut worker = HiveWorker::new("w1".into(), config);

        // Execute with a mock that always fails
        let attempt_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let ac = attempt_count.clone();

        let stats = worker
            .run_loop(
                "o1",
                &bb,
                &pheromones,
                &selector,
                1,
                &dep_counts,
                cancel,
                move |_task, _deps| {
                    let ac = ac.clone();
                    async move {
                        ac.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(TaskResult {
                            summary: String::new(),
                            tokens_used: 10,
                            artifacts: vec![],
                            success: false,
                            error: Some("Tool error".into()),
                        })
                    }
                },
            )
            .await
            .unwrap();

        // Worker should have attempted the task twice (retry_count reaches max_retries=2)
        // then the task escalates and worker exits
        assert!(stats.tasks_failed >= 1);

        // Task should be escalated
        let task = bb.get_task("t1").await.unwrap().unwrap();
        assert_eq!(task.status, HiveTaskStatus::Escalate);
    }
}
