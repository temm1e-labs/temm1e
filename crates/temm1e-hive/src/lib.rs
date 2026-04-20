//! TEMM1E Hive — Stigmergic Swarm Intelligence Runtime.
//!
//! The Hive coordinates multiple TEMM1E agent workers to execute complex
//! tasks in parallel, communicating through a shared SQLite blackboard
//! and a pheromone signal field — not through LLM-to-LLM chat.
//!
//! # Architecture
//!
//! ```text
//! User Message → Queen (decompose) → Task DAG → Workers (parallel) → Aggregated Result
//!                                        ↕
//!                                   Blackboard (SQLite)
//!                                        ↕
//!                                   Pheromone Field
//! ```
//!
//! # Usage
//!
//! ```toml
//! [hive]
//! enabled = true
//! max_workers = 3
//! ```
//!
//! When `enabled = false` (default), the Hive is completely inert.

pub mod blackboard;
pub mod config;
pub mod dag;
pub mod pheromone;
pub mod queen;
pub mod selection;
pub mod types;
pub mod worker;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use temm1e_core::types::error::Temm1eError;

use crate::blackboard::Blackboard;
pub use crate::config::HiveConfig;
use crate::pheromone::PheromoneField;
use crate::queen::Queen;
use crate::selection::TaskSelector;
use crate::types::{
    DecomposedTask, HiveOrder, HiveOrderStatus, HiveTask, HiveTaskStatus, SwarmResult,
};
use crate::worker::{HiveWorker, TaskResult};

// ---------------------------------------------------------------------------
// Hive
// ---------------------------------------------------------------------------

/// The Hive orchestrator. Coordinates decomposition, worker spawning,
/// and result aggregation.
pub struct Hive {
    blackboard: Blackboard,
    pheromones: Arc<PheromoneField>,
    queen: Queen,
    config: HiveConfig,
    /// Runtime-overridable task duration limit (seconds). 0 = use config default.
    task_duration_override: AtomicU64,
}

impl Hive {
    /// Create a new Hive. Initializes SQLite tables and starts the
    /// pheromone GC loop.
    pub async fn new(config: &HiveConfig, database_url: &str) -> Result<Self, Temm1eError> {
        let blackboard = Blackboard::new(database_url).await?;
        let pheromones =
            Arc::new(PheromoneField::new(database_url, config.pheromone.clone()).await?);
        let queen = Queen::new(config);

        // Start pheromone GC
        pheromones.start_gc_loop(config.pheromone.gc_interval_secs);

        info!("Many Tems initialized");
        Ok(Self {
            blackboard,
            pheromones,
            queen,
            config: config.clone(),
            task_duration_override: AtomicU64::new(0),
        })
    }

    /// Set the maximum wall-clock seconds a single hive task may run.
    /// This override takes effect on the next task dispatch (lock-free).
    pub fn set_max_task_duration_secs(&self, secs: u64) {
        self.task_duration_override.store(secs, Ordering::Relaxed);
    }

    /// Get the effective maximum task duration in seconds.
    /// Returns the runtime override if set, otherwise the config default.
    pub fn max_task_duration_secs(&self) -> u64 {
        let v = self.task_duration_override.load(Ordering::Relaxed);
        if v > 0 {
            v
        } else {
            self.config.blocker.max_task_duration_secs
        }
    }

    /// Attempt to decompose a message into a swarm-executable order.
    ///
    /// Returns `None` if single-agent mode is better (simple message,
    /// low speedup, or high queen cost).
    ///
    /// The `provider_call` closure performs the actual LLM call for
    /// decomposition. This keeps the Hive decoupled from the Provider trait.
    pub async fn maybe_decompose<F, Fut>(
        &self,
        message: &str,
        chat_id: &str,
        provider_call: F,
    ) -> Result<Option<String>, Temm1eError>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<(String, u64), Temm1eError>>,
    {
        // Caller has already verified this message is a decomposition candidate
        // (via LLM classifier or LLM yes/no check). Proceed directly to decomposition.
        //
        // Retry loop: if the Queen's JSON response fails to parse, feed the error
        // back and ask the Queen to fix it (up to 3 attempts).
        const MAX_DECOMPOSE_ATTEMPTS: usize = 3;
        let mut last_error = String::new();
        let mut total_queen_tokens = 0_u64;
        let mut decomposition = None;

        for attempt in 1..=MAX_DECOMPOSE_ATTEMPTS {
            let prompt = if attempt == 1 {
                Queen::build_decomposition_prompt(message)
            } else {
                format!(
                    "{}\n\nYour previous response failed to parse: {}\n\nFix the JSON and try again.",
                    Queen::build_decomposition_prompt(message),
                    last_error,
                )
            };

            let (response, tokens) = provider_call(prompt).await?;
            total_queen_tokens += tokens;

            match Queen::parse_decomposition(&response) {
                Ok(d) => {
                    decomposition = Some(d);
                    break;
                }
                Err(e) => {
                    last_error = e.to_string();
                    if attempt < MAX_DECOMPOSE_ATTEMPTS {
                        info!(attempt = attempt, error = %e, "Queen decomposition parse failed, retrying");
                    } else {
                        warn!(error = %e, "Queen decomposition failed after {MAX_DECOMPOSE_ATTEMPTS} attempts");
                    }
                }
            }
        }

        let decomposition = match decomposition {
            Some(d) => d,
            None => return Ok(None),
        };
        let queen_tokens = total_queen_tokens;

        // Step 4: Check activation threshold
        let estimated_single: u64 = decomposition
            .tasks
            .iter()
            .map(|t| t.estimated_tokens as u64)
            .sum::<u64>()
            * 2; // rough estimate: each task's tokens × 2 for context growth

        if !self
            .queen
            .should_activate_swarm(&decomposition, queen_tokens, estimated_single)
        {
            return Ok(None);
        }

        // Step 5: Create order and tasks in Blackboard
        let order_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();

        let order = HiveOrder {
            id: order_id.clone(),
            chat_id: chat_id.into(),
            original_message: message.into(),
            task_count: decomposition.tasks.len(),
            completed_count: 0,
            status: HiveOrderStatus::Active,
            total_tokens: queen_tokens,
            queen_tokens,
            created_at: now,
            completed_at: None,
        };
        self.blackboard.create_order(&order).await?;

        // v5.5.0 fix: namespace task IDs by order_id to prevent UNIQUE
        // constraint violations when spawn_swarm is invoked multiple times
        // per session. The Queen's decomposition prompt always emits
        // "t1, t2, t3..." as task IDs — collide across orders. We prefix
        // with the order_id (UUID, guaranteed unique) here and transform
        // dependency references the same way so the DAG stays intact.
        let task_id_ns = |raw: &str| format!("{}:{}", order_id, raw);
        let hive_tasks: Vec<HiveTask> = decomposition
            .tasks
            .iter()
            .map(|dt| HiveTask {
                id: task_id_ns(&dt.id),
                order_id: order_id.clone(),
                description: dt.description.clone(),
                status: HiveTaskStatus::Pending,
                claimed_by: None,
                dependencies: dt.dependencies.iter().map(|d| task_id_ns(d)).collect(),
                context_tags: dt.context_tags.clone(),
                estimated_tokens: dt.estimated_tokens,
                actual_tokens: 0,
                result_summary: None,
                artifacts: vec![],
                retry_count: 0,
                max_retries: self.config.blocker.max_retries,
                error_log: None,
                created_at: now,
                started_at: None,
                completed_at: None,
            })
            .collect();

        self.blackboard.create_tasks(&hive_tasks).await?;

        info!(
            order_id = %order_id,
            tasks = decomposition.tasks.len(),
            queen_tokens = queen_tokens,
            "Order created, pack activating"
        );

        Ok(Some(order_id))
    }

    /// Execute an order using parallel workers.
    ///
    /// Spawns up to `max_workers` tokio tasks, each competing for READY tasks
    /// via atomic SQLite claims. Workers that lose a claim simply re-select.
    /// All workers share the same Blackboard (cheap clone — `SqlitePool` is `Arc`)
    /// and PheromoneField.
    ///
    /// The `execute_fn` closure runs a single task against the AI provider.
    /// It receives the task and dependency results, returns the output.
    pub async fn execute_order<F, Fut>(
        &self,
        order_id: &str,
        cancel: CancellationToken,
        execute_fn: F,
    ) -> Result<SwarmResult, Temm1eError>
    where
        F: Fn(HiveTask, Vec<(String, String)>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<TaskResult, Temm1eError>> + Send,
    {
        let start = Instant::now();

        // Get order info
        let order = self
            .blackboard
            .get_order(order_id)
            .await?
            .ok_or_else(|| Temm1eError::Internal(format!("Order not found: {order_id}")))?;

        // Compute dependent counts for selection
        let all_tasks = self.blackboard.get_order_results(order_id).await?;
        let decomposed: Vec<DecomposedTask> = all_tasks
            .iter()
            .map(|t| DecomposedTask {
                id: t.id.clone(),
                description: t.description.clone(),
                dependencies: t.dependencies.clone(),
                context_tags: t.context_tags.clone(),
                estimated_tokens: t.estimated_tokens,
            })
            .collect();
        let dep_counts = Arc::new(dag::dependent_counts(&decomposed));

        // Determine worker count: min(max_workers, task_count), floor at min_workers
        let worker_count = self
            .config
            .max_workers
            .min(order.task_count)
            .max(self.config.min_workers);

        let execute_fn = Arc::new(execute_fn);

        info!(
            order_id = order_id,
            workers = worker_count,
            tasks = order.task_count,
            "Spawning pack — Tems working in parallel"
        );

        // Spawn N workers as real parallel tokio tasks.
        // Each gets a clone of the shared Blackboard (cheap: SqlitePool is Arc),
        // a clone of the Arc<PheromoneField>, and its own TaskSelector.
        let mut handles = Vec::with_capacity(worker_count);

        for i in 0..worker_count {
            let worker_id = format!("w{}", i + 1);
            let mut config = self.config.clone();
            // Apply runtime override if set
            let duration_override = self.task_duration_override.load(Ordering::Relaxed);
            if duration_override > 0 {
                config.blocker.max_task_duration_secs = duration_override;
            }
            let blackboard = self.blackboard.clone();
            let pheromones = Arc::clone(&self.pheromones);
            let selector = TaskSelector::new(&self.config.selection);
            let dep_counts = Arc::clone(&dep_counts);
            let total = order.task_count;
            let cancel = cancel.clone();
            let exec = Arc::clone(&execute_fn);
            let oid = order_id.to_string();

            handles.push(tokio::spawn(async move {
                let mut worker = HiveWorker::new(worker_id, config);
                worker
                    .run_loop(
                        &oid,
                        &blackboard,
                        &pheromones,
                        &selector,
                        total,
                        &dep_counts,
                        cancel,
                        |task, deps| {
                            let exec = Arc::clone(&exec);
                            async move { exec(task, deps).await }
                        },
                    )
                    .await
            }));
        }

        // Collect results from all workers
        let mut total_completed = 0_usize;
        let mut total_tokens = order.queen_tokens;
        // Queen's tokens are counted in total_tokens above as legacy. For the
        // split-accounting fields, Queen input/output split is not tracked by
        // the blackboard — we conservatively attribute all queen_tokens to
        // input (decomposition prompts are input-heavy; a tiny amount of
        // output goes into subtask descriptions). Workers carry full split.
        let mut total_input_tokens: u64 = order.queen_tokens;
        let mut total_output_tokens: u64 = 0;
        let mut total_cost_usd: f64 = 0.0;
        let mut workers_used = 0_usize;

        for handle in handles {
            match handle.await {
                Ok(Ok(stats)) => {
                    total_completed += stats.tasks_completed;
                    total_tokens += stats.total_tokens;
                    total_input_tokens += stats.total_input_tokens;
                    total_output_tokens += stats.total_output_tokens;
                    total_cost_usd += stats.total_cost_usd;
                    if stats.tasks_completed > 0 || stats.tasks_failed > 0 {
                        workers_used += 1;
                    }
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "Worker returned error");
                }
                Err(e) => {
                    warn!(error = %e, "Worker task panicked");
                }
            }
        }

        // Count escalated tasks
        let mut total_escalated = 0_usize;
        let final_tasks = self.blackboard.get_order_results(order_id).await?;
        for task in &final_tasks {
            if task.status == HiveTaskStatus::Escalate {
                total_escalated += 1;
            }
        }

        // Aggregate results
        let text = aggregate_results(&final_tasks);

        let result = SwarmResult {
            text,
            total_tokens,
            total_input_tokens,
            total_output_tokens,
            total_cost_usd,
            tasks_completed: total_completed,
            tasks_escalated: total_escalated,
            wall_clock_ms: start.elapsed().as_millis() as u64,
            workers_used,
        };

        info!(
            order_id = order_id,
            completed = result.tasks_completed,
            escalated = result.tasks_escalated,
            tokens = result.total_tokens,
            input_tokens = result.total_input_tokens,
            output_tokens = result.total_output_tokens,
            cost_usd = format!("{:.6}", result.total_cost_usd),
            wall_ms = result.wall_clock_ms,
            workers = result.workers_used,
            "Pack execution complete"
        );

        Ok(result)
    }
}

/// Aggregate task results into a final response text.
fn aggregate_results(tasks: &[HiveTask]) -> String {
    let mut parts = Vec::new();

    for task in tasks {
        match task.status {
            HiveTaskStatus::Complete => {
                if let Some(ref summary) = task.result_summary {
                    parts.push(summary.clone());
                }
            }
            HiveTaskStatus::Escalate => {
                parts.push(format!(
                    "[Task '{}' was escalated: {}]",
                    task.description,
                    task.error_log.as_deref().unwrap_or("unknown error")
                ));
            }
            _ => {
                // Task didn't finish — shouldn't happen if order is complete
                parts.push(format!(
                    "[Task '{}': status {}]",
                    task.description, task.status
                ));
            }
        }
    }

    if parts.is_empty() {
        "No results produced.".to_string()
    } else {
        parts.join("\n\n")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_complete_tasks() {
        let tasks = vec![
            HiveTask {
                id: "t1".into(),
                order_id: "o1".into(),
                description: "Create schema".into(),
                status: HiveTaskStatus::Complete,
                claimed_by: None,
                dependencies: vec![],
                context_tags: vec![],
                estimated_tokens: 0,
                actual_tokens: 0,
                result_summary: Some("Schema created".into()),
                artifacts: vec![],
                retry_count: 0,
                max_retries: 3,
                error_log: None,
                created_at: 0,
                started_at: None,
                completed_at: None,
            },
            HiveTask {
                id: "t2".into(),
                order_id: "o1".into(),
                description: "Build API".into(),
                status: HiveTaskStatus::Complete,
                claimed_by: None,
                dependencies: vec![],
                context_tags: vec![],
                estimated_tokens: 0,
                actual_tokens: 0,
                result_summary: Some("API built".into()),
                artifacts: vec![],
                retry_count: 0,
                max_retries: 3,
                error_log: None,
                created_at: 0,
                started_at: None,
                completed_at: None,
            },
        ];

        let text = aggregate_results(&tasks);
        assert!(text.contains("Schema created"));
        assert!(text.contains("API built"));
    }

    #[test]
    fn aggregate_with_escalation() {
        let tasks = vec![HiveTask {
            id: "t1".into(),
            order_id: "o1".into(),
            description: "Deploy".into(),
            status: HiveTaskStatus::Escalate,
            claimed_by: None,
            dependencies: vec![],
            context_tags: vec![],
            estimated_tokens: 0,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 3,
            max_retries: 3,
            error_log: Some("Permission denied".into()),
            created_at: 0,
            started_at: None,
            completed_at: None,
        }];

        let text = aggregate_results(&tasks);
        assert!(text.contains("escalated"));
        assert!(text.contains("Permission denied"));
    }

    #[test]
    fn aggregate_empty() {
        assert_eq!(aggregate_results(&[]), "No results produced.");
    }

    /// Proves workers run in parallel: 4 independent tasks each taking 200ms
    /// should complete in ~200ms with 4 workers, not ~800ms sequentially.
    #[tokio::test]
    async fn parallel_workers_actually_parallel() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = HiveConfig {
            enabled: true,
            max_workers: 4,
            min_workers: 4,
            ..Default::default()
        };

        let hive = Hive::new(&config, "sqlite::memory:").await.unwrap();

        // Create order with 4 independent tasks (no dependencies)
        let now = chrono::Utc::now().timestamp_millis();
        let order = HiveOrder {
            id: "parallel-test".into(),
            chat_id: "c1".into(),
            original_message: "parallel test".into(),
            task_count: 4,
            completed_count: 0,
            status: HiveOrderStatus::Active,
            total_tokens: 0,
            queen_tokens: 0,
            created_at: now,
            completed_at: None,
        };
        hive.blackboard.create_order(&order).await.unwrap();

        let tasks: Vec<HiveTask> = (1..=4)
            .map(|i| HiveTask {
                id: format!("t{i}"),
                order_id: "parallel-test".into(),
                description: format!("Independent task {i}"),
                status: HiveTaskStatus::Pending,
                claimed_by: None,
                dependencies: vec![],
                context_tags: vec!["test".into()],
                estimated_tokens: 100,
                actual_tokens: 0,
                result_summary: None,
                artifacts: vec![],
                retry_count: 0,
                max_retries: 3,
                error_log: None,
                created_at: now,
                started_at: None,
                completed_at: None,
            })
            .collect();
        hive.blackboard.create_tasks(&tasks).await.unwrap();

        // Track peak concurrency: how many execute_fn calls are active at once
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let active_c = Arc::clone(&active);
        let peak_c = Arc::clone(&peak);

        let cancel = CancellationToken::new();
        let start = Instant::now();

        let result = hive
            .execute_order("parallel-test", cancel, move |task, _deps| {
                let active = Arc::clone(&active_c);
                let peak = Arc::clone(&peak_c);
                async move {
                    // Increment active count
                    let cur = active.fetch_add(1, Ordering::SeqCst) + 1;
                    // Track peak
                    peak.fetch_max(cur, Ordering::SeqCst);

                    // Simulate 200ms of work
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

                    // Decrement active count
                    active.fetch_sub(1, Ordering::SeqCst);

                    Ok(TaskResult {
                        summary: format!("Done: {}", task.description),
                        tokens_used: 50,
                        input_tokens: 30,
                        output_tokens: 20,
                        cost_usd: 0.0,
                        artifacts: vec![],
                        success: true,
                        error: None,
                    })
                }
            })
            .await
            .unwrap();

        let elapsed = start.elapsed();

        // All 4 tasks should complete
        assert_eq!(result.tasks_completed, 4, "all tasks should complete");
        assert_eq!(result.tasks_escalated, 0, "no escalations");

        // Peak concurrency should be > 1 (proves parallelism)
        let peak_val = peak.load(Ordering::SeqCst);
        assert!(
            peak_val >= 2,
            "peak concurrency was {peak_val} — workers aren't running in parallel!"
        );

        // Wall clock should be closer to 200ms than 800ms.
        // Use very generous margin (2000ms) for CI runners under load —
        // the peak concurrency assertion above already proves parallelism.
        assert!(
            elapsed.as_millis() < 2000,
            "took {}ms — should be ~200ms with 4 parallel workers",
            elapsed.as_millis()
        );

        // Result text should contain all task outputs
        assert!(result.text.contains("Done: Independent task 1"));
        assert!(result.text.contains("Done: Independent task 4"));

        // Multiple workers should have participated
        assert!(
            result.workers_used >= 2,
            "only {} workers used — swarm isn't distributing work",
            result.workers_used
        );
    }

    /// DAG ordering: tasks with dependencies wait for prerequisites.
    /// t1, t2 run in parallel. t3 depends on both and runs after.
    #[tokio::test]
    async fn parallel_respects_dag_dependencies() {
        let config = HiveConfig {
            enabled: true,
            max_workers: 3,
            ..Default::default()
        };

        let hive = Hive::new(&config, "sqlite::memory:").await.unwrap();

        let now = chrono::Utc::now().timestamp_millis();
        let order = HiveOrder {
            id: "dag-test".into(),
            chat_id: "c1".into(),
            original_message: "dag test".into(),
            task_count: 3,
            completed_count: 0,
            status: HiveOrderStatus::Active,
            total_tokens: 0,
            queen_tokens: 0,
            created_at: now,
            completed_at: None,
        };
        hive.blackboard.create_order(&order).await.unwrap();

        let tasks = vec![
            HiveTask {
                id: "t1".into(),
                order_id: "dag-test".into(),
                description: "First (parallel)".into(),
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
                created_at: now,
                started_at: None,
                completed_at: None,
            },
            HiveTask {
                id: "t2".into(),
                order_id: "dag-test".into(),
                description: "Second (parallel)".into(),
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
                created_at: now,
                started_at: None,
                completed_at: None,
            },
            HiveTask {
                id: "t3".into(),
                order_id: "dag-test".into(),
                description: "Third (depends on t1+t2)".into(),
                status: HiveTaskStatus::Pending,
                claimed_by: None,
                dependencies: vec!["t1".into(), "t2".into()],
                context_tags: vec![],
                estimated_tokens: 100,
                actual_tokens: 0,
                result_summary: None,
                artifacts: vec![],
                retry_count: 0,
                max_retries: 3,
                error_log: None,
                created_at: now,
                started_at: None,
                completed_at: None,
            },
        ];
        hive.blackboard.create_tasks(&tasks).await.unwrap();

        // Track completion order
        let completion_order = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        let co = Arc::clone(&completion_order);
        let cancel = CancellationToken::new();

        let result = hive
            .execute_order("dag-test", cancel, move |task, _deps| {
                let co = Arc::clone(&co);
                async move {
                    // Simulate 100ms of work
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    co.lock().unwrap().push(task.id.clone());

                    Ok(TaskResult {
                        summary: format!("Result of {}", task.id),
                        tokens_used: 30,
                        input_tokens: 20,
                        output_tokens: 10,
                        cost_usd: 0.0,
                        artifacts: vec![],
                        success: true,
                        error: None,
                    })
                }
            })
            .await
            .unwrap();

        assert_eq!(result.tasks_completed, 3);
        assert_eq!(result.tasks_escalated, 0);

        // t3 must come after both t1 and t2
        let order = completion_order.lock().unwrap();
        let t1_pos = order.iter().position(|s| s == "t1").unwrap();
        let t2_pos = order.iter().position(|s| s == "t2").unwrap();
        let t3_pos = order.iter().position(|s| s == "t3").unwrap();

        assert!(
            t3_pos > t1_pos && t3_pos > t2_pos,
            "t3 ran at position {t3_pos} but t1={t1_pos}, t2={t2_pos} — DAG violated"
        );
    }
}
