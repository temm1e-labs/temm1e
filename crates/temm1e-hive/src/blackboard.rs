//! Blackboard — SQLite-backed task DAG with atomic claim semantics.
//!
//! The Blackboard is the shared state that all workers read from and write to.
//! All state lives here — no information exists solely in worker memory (Axiom A4).

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tracing::{debug, info};

use temm1e_core::types::error::Temm1eError;

use crate::types::{HiveOrder, HiveOrderStatus, HiveTask, HiveTaskStatus};

// ---------------------------------------------------------------------------
// Blackboard
// ---------------------------------------------------------------------------

/// SQLite-backed blackboard for hive task management.
///
/// `Clone` is cheap — `SqlitePool` is internally `Arc`-wrapped.
/// Multiple workers share the same underlying connection pool.
#[derive(Clone)]
pub struct Blackboard {
    pool: SqlitePool,
}

impl Blackboard {
    /// Create a new Blackboard with the given SQLite connection URL.
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Internal(format!("Blackboard connect: {e}")))?;

        let bb = Self { pool };
        bb.init_tables().await?;
        info!("Blackboard initialized");
        Ok(bb)
    }

    async fn init_tables(&self) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS hive_orders (
                id              TEXT PRIMARY KEY,
                chat_id         TEXT NOT NULL,
                original_message TEXT NOT NULL,
                task_count      INTEGER NOT NULL,
                completed_count INTEGER DEFAULT 0,
                status          TEXT NOT NULL DEFAULT 'active',
                total_tokens    INTEGER DEFAULT 0,
                queen_tokens    INTEGER DEFAULT 0,
                created_at      INTEGER NOT NULL,
                completed_at    INTEGER
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard orders table: {e}")))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS hive_tasks (
                id              TEXT PRIMARY KEY,
                order_id        TEXT NOT NULL,
                description     TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'pending',
                claimed_by      TEXT,
                dependencies    TEXT DEFAULT '[]',
                context_tags    TEXT DEFAULT '[]',
                estimated_tokens INTEGER DEFAULT 0,
                actual_tokens   INTEGER DEFAULT 0,
                result_summary  TEXT,
                artifacts       TEXT DEFAULT '[]',
                retry_count     INTEGER DEFAULT 0,
                max_retries     INTEGER DEFAULT 3,
                error_log       TEXT,
                created_at      INTEGER NOT NULL,
                started_at      INTEGER,
                completed_at    INTEGER,
                FOREIGN KEY (order_id) REFERENCES hive_orders(id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard tasks table: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_hive_tasks_status \
             ON hive_tasks(status, order_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard index: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_hive_tasks_order \
             ON hive_tasks(order_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard index: {e}")))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Order operations
    // -----------------------------------------------------------------------

    /// Create a new order entry.
    pub async fn create_order(&self, order: &HiveOrder) -> Result<(), Temm1eError> {
        sqlx::query(
            "INSERT INTO hive_orders (id, chat_id, original_message, task_count, completed_count, \
             status, total_tokens, queen_tokens, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(&order.id)
        .bind(&order.chat_id)
        .bind(&order.original_message)
        .bind(order.task_count as i64)
        .bind(order.completed_count as i64)
        .bind(order.status.as_str())
        .bind(order.total_tokens as i64)
        .bind(order.queen_tokens as i64)
        .bind(order.created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard create_order: {e}")))?;

        debug!(order_id = %order.id, "Created hive order");
        Ok(())
    }

    /// Get an order by ID.
    pub async fn get_order(&self, order_id: &str) -> Result<Option<HiveOrder>, Temm1eError> {
        let row: Option<OrderRow> = sqlx::query_as(
            "SELECT id, chat_id, original_message, task_count, completed_count, \
             status, total_tokens, queen_tokens, created_at, completed_at \
             FROM hive_orders WHERE id = ?1",
        )
        .bind(order_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard get_order: {e}")))?;

        Ok(row.map(|r| r.into_order()))
    }

    // -----------------------------------------------------------------------
    // Task operations
    // -----------------------------------------------------------------------

    /// Insert tasks for an order. Tasks with no dependencies start as READY.
    pub async fn create_tasks(&self, tasks: &[HiveTask]) -> Result<(), Temm1eError> {
        for task in tasks {
            let status = if task.dependencies.is_empty() {
                HiveTaskStatus::Ready
            } else {
                HiveTaskStatus::Pending
            };
            let deps_json = serde_json::to_string(&task.dependencies)
                .map_err(|e| Temm1eError::Internal(format!("serialize deps: {e}")))?;
            let tags_json = serde_json::to_string(&task.context_tags)
                .map_err(|e| Temm1eError::Internal(format!("serialize tags: {e}")))?;

            sqlx::query(
                "INSERT INTO hive_tasks (id, order_id, description, status, dependencies, \
                 context_tags, estimated_tokens, max_retries, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .bind(&task.id)
            .bind(&task.order_id)
            .bind(&task.description)
            .bind(status.as_str())
            .bind(&deps_json)
            .bind(&tags_json)
            .bind(task.estimated_tokens as i64)
            .bind(task.max_retries as i64)
            .bind(task.created_at)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Internal(format!("Blackboard create_task: {e}")))?;
        }

        debug!(count = tasks.len(), "Created hive tasks");
        Ok(())
    }

    /// Atomically claim a READY task. Returns true if claim succeeded.
    pub async fn claim_task(&self, task_id: &str, worker_id: &str) -> Result<bool, Temm1eError> {
        let now = chrono::Utc::now().timestamp_millis();

        let result = sqlx::query(
            "UPDATE hive_tasks SET status = 'active', claimed_by = ?1, started_at = ?2 \
             WHERE id = ?3 AND status = 'ready'",
        )
        .bind(worker_id)
        .bind(now)
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard claim_task: {e}")))?;

        let claimed = result.rows_affected() > 0;
        if claimed {
            debug!(task_id = task_id, worker = worker_id, "Claimed task");
        }
        Ok(claimed)
    }

    /// Complete a task. Returns the list of task IDs that became READY.
    pub async fn complete_task(
        &self,
        task_id: &str,
        result_summary: &str,
        actual_tokens: u32,
    ) -> Result<Vec<String>, Temm1eError> {
        let now = chrono::Utc::now().timestamp_millis();

        // Mark task complete
        sqlx::query(
            "UPDATE hive_tasks SET status = 'complete', result_summary = ?1, \
             actual_tokens = ?2, completed_at = ?3 WHERE id = ?4",
        )
        .bind(result_summary)
        .bind(actual_tokens as i64)
        .bind(now)
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard complete_task: {e}")))?;

        // Get the order_id for this task
        let (order_id,): (String,) =
            sqlx::query_as("SELECT order_id FROM hive_tasks WHERE id = ?1")
                .bind(task_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| Temm1eError::Internal(format!("Blackboard get order_id: {e}")))?;

        // Increment completed_count and add tokens
        sqlx::query(
            "UPDATE hive_orders SET completed_count = completed_count + 1, \
             total_tokens = total_tokens + ?1 WHERE id = ?2",
        )
        .bind(actual_tokens as i64)
        .bind(&order_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard update order: {e}")))?;

        // Find tasks that should transition from PENDING to READY
        let newly_ready = self.resolve_dependencies(&order_id).await?;

        debug!(
            task_id = task_id,
            newly_ready = newly_ready.len(),
            "Completed task"
        );
        Ok(newly_ready)
    }

    /// Fail a task. Increments retry_count. Returns the new status.
    pub async fn fail_task(
        &self,
        task_id: &str,
        error: &str,
    ) -> Result<HiveTaskStatus, Temm1eError> {
        // Get current retry count and max retries
        let row: (i64, i64) =
            sqlx::query_as("SELECT retry_count, max_retries FROM hive_tasks WHERE id = ?1")
                .bind(task_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| Temm1eError::Internal(format!("Blackboard fail_task fetch: {e}")))?;

        let retry_count = row.0 as u32;
        let max_retries = row.1 as u32;
        let new_retry_count = retry_count + 1;

        let new_status = if new_retry_count >= max_retries {
            HiveTaskStatus::Escalate
        } else {
            // Set back to Ready for retry by a different worker
            HiveTaskStatus::Ready
        };

        sqlx::query(
            "UPDATE hive_tasks SET status = ?1, retry_count = ?2, error_log = ?3, \
             claimed_by = NULL, started_at = NULL WHERE id = ?4",
        )
        .bind(new_status.as_str())
        .bind(new_retry_count as i64)
        .bind(error)
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard fail_task: {e}")))?;

        debug!(
            task_id = task_id,
            retry = new_retry_count,
            status = new_status.as_str(),
            "Failed task"
        );
        Ok(new_status)
    }

    /// Get all READY tasks for an order.
    pub async fn get_ready_tasks(&self, order_id: &str) -> Result<Vec<HiveTask>, Temm1eError> {
        let rows: Vec<TaskRow> = sqlx::query_as(
            "SELECT id, order_id, description, status, claimed_by, dependencies, \
             context_tags, estimated_tokens, actual_tokens, result_summary, artifacts, \
             retry_count, max_retries, error_log, created_at, started_at, completed_at \
             FROM hive_tasks WHERE order_id = ?1 AND status = 'ready'",
        )
        .bind(order_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard get_ready: {e}")))?;

        rows.into_iter().map(|r| r.into_task()).collect()
    }

    /// Get a task by ID.
    pub async fn get_task(&self, task_id: &str) -> Result<Option<HiveTask>, Temm1eError> {
        let row: Option<TaskRow> = sqlx::query_as(
            "SELECT id, order_id, description, status, claimed_by, dependencies, \
             context_tags, estimated_tokens, actual_tokens, result_summary, artifacts, \
             retry_count, max_retries, error_log, created_at, started_at, completed_at \
             FROM hive_tasks WHERE id = ?1",
        )
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard get_task: {e}")))?;

        match row {
            Some(r) => Ok(Some(r.into_task()?)),
            None => Ok(None),
        }
    }

    /// Get the result summaries for a task's dependencies.
    pub async fn get_dependency_results(
        &self,
        task_id: &str,
    ) -> Result<Vec<(String, String)>, Temm1eError> {
        // First get the task's dependencies
        let task = self
            .get_task(task_id)
            .await?
            .ok_or_else(|| Temm1eError::Internal(format!("Task not found: {task_id}")))?;

        let mut results = Vec::new();
        for dep_id in &task.dependencies {
            let dep = self
                .get_task(dep_id)
                .await?
                .ok_or_else(|| Temm1eError::Internal(format!("Dependency not found: {dep_id}")))?;
            if let Some(summary) = &dep.result_summary {
                results.push((dep_id.clone(), summary.clone()));
            }
        }

        Ok(results)
    }

    /// Check if all tasks in an order have reached a terminal state.
    pub async fn is_order_complete(&self, order_id: &str) -> Result<bool, Temm1eError> {
        let (non_terminal,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM hive_tasks \
             WHERE order_id = ?1 AND status NOT IN ('complete', 'escalate')",
        )
        .bind(order_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard is_complete: {e}")))?;

        Ok(non_terminal == 0)
    }

    /// Get all tasks for an order in topological order (completed tasks first).
    pub async fn get_order_results(&self, order_id: &str) -> Result<Vec<HiveTask>, Temm1eError> {
        let rows: Vec<TaskRow> = sqlx::query_as(
            "SELECT id, order_id, description, status, claimed_by, dependencies, \
             context_tags, estimated_tokens, actual_tokens, result_summary, artifacts, \
             retry_count, max_retries, error_log, created_at, started_at, completed_at \
             FROM hive_tasks WHERE order_id = ?1 ORDER BY completed_at ASC NULLS LAST",
        )
        .bind(order_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard get_results: {e}")))?;

        rows.into_iter().map(|r| r.into_task()).collect()
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Check PENDING tasks and transition to READY if all deps are COMPLETE.
    async fn resolve_dependencies(&self, order_id: &str) -> Result<Vec<String>, Temm1eError> {
        let pending: Vec<TaskRow> = sqlx::query_as(
            "SELECT id, order_id, description, status, claimed_by, dependencies, \
             context_tags, estimated_tokens, actual_tokens, result_summary, artifacts, \
             retry_count, max_retries, error_log, created_at, started_at, completed_at \
             FROM hive_tasks WHERE order_id = ?1 AND status = 'pending'",
        )
        .bind(order_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("Blackboard resolve: {e}")))?;

        let mut newly_ready = Vec::new();

        for row in pending {
            let task = row.into_task()?;
            let mut all_deps_met = true;

            for dep_id in &task.dependencies {
                let (dep_status,): (String,) =
                    sqlx::query_as("SELECT status FROM hive_tasks WHERE id = ?1")
                        .bind(dep_id)
                        .fetch_one(&self.pool)
                        .await
                        .map_err(|e| {
                            Temm1eError::Internal(format!("Blackboard resolve dep check: {e}"))
                        })?;

                if dep_status != "complete" {
                    all_deps_met = false;
                    break;
                }
            }

            if all_deps_met {
                sqlx::query("UPDATE hive_tasks SET status = 'ready' WHERE id = ?1")
                    .bind(&task.id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        Temm1eError::Internal(format!("Blackboard resolve update: {e}"))
                    })?;
                newly_ready.push(task.id.clone());
            }
        }

        Ok(newly_ready)
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct OrderRow {
    id: String,
    chat_id: String,
    original_message: String,
    task_count: i64,
    completed_count: i64,
    status: String,
    total_tokens: i64,
    queen_tokens: i64,
    created_at: i64,
    completed_at: Option<i64>,
}

impl OrderRow {
    fn into_order(self) -> HiveOrder {
        HiveOrder {
            id: self.id,
            chat_id: self.chat_id,
            original_message: self.original_message,
            task_count: self.task_count as usize,
            completed_count: self.completed_count as usize,
            status: HiveOrderStatus::parse_str(&self.status).unwrap_or(HiveOrderStatus::Active),
            total_tokens: self.total_tokens as u64,
            queen_tokens: self.queen_tokens as u64,
            created_at: self.created_at,
            completed_at: self.completed_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    id: String,
    order_id: String,
    description: String,
    status: String,
    claimed_by: Option<String>,
    dependencies: String,
    context_tags: String,
    estimated_tokens: i64,
    actual_tokens: i64,
    result_summary: Option<String>,
    artifacts: String,
    retry_count: i64,
    max_retries: i64,
    error_log: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    completed_at: Option<i64>,
}

impl TaskRow {
    fn into_task(self) -> Result<HiveTask, Temm1eError> {
        let dependencies: Vec<String> =
            serde_json::from_str(&self.dependencies).unwrap_or_default();
        let context_tags: Vec<String> =
            serde_json::from_str(&self.context_tags).unwrap_or_default();
        let artifacts: Vec<String> = serde_json::from_str(&self.artifacts).unwrap_or_default();

        Ok(HiveTask {
            id: self.id,
            order_id: self.order_id,
            description: self.description,
            status: HiveTaskStatus::parse_str(&self.status).ok_or_else(|| {
                Temm1eError::Internal(format!("Unknown task status: {}", self.status))
            })?,
            claimed_by: self.claimed_by,
            dependencies,
            context_tags,
            estimated_tokens: self.estimated_tokens as u32,
            actual_tokens: self.actual_tokens as u32,
            result_summary: self.result_summary,
            artifacts,
            retry_count: self.retry_count as u32,
            max_retries: self.max_retries as u32,
            error_log: self.error_log,
            created_at: self.created_at,
            started_at: self.started_at,
            completed_at: self.completed_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_bb() -> Blackboard {
        Blackboard::new("sqlite::memory:").await.unwrap()
    }

    fn make_order(id: &str) -> HiveOrder {
        HiveOrder {
            id: id.into(),
            chat_id: "chat1".into(),
            original_message: "Build something".into(),
            task_count: 3,
            completed_count: 0,
            status: HiveOrderStatus::Active,
            total_tokens: 0,
            queen_tokens: 500,
            created_at: chrono::Utc::now().timestamp_millis(),
            completed_at: None,
        }
    }

    fn make_task(id: &str, order_id: &str, deps: &[&str]) -> HiveTask {
        HiveTask {
            id: id.into(),
            order_id: order_id.into(),
            description: format!("Task {id}"),
            status: HiveTaskStatus::Pending,
            claimed_by: None,
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            context_tags: vec!["rust".into()],
            estimated_tokens: 1000,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 3,
            error_log: None,
            created_at: chrono::Utc::now().timestamp_millis(),
            started_at: None,
            completed_at: None,
        }
    }

    #[tokio::test]
    async fn create_order_and_tasks() {
        let bb = make_bb().await;
        let order = make_order("o1");
        bb.create_order(&order).await.unwrap();

        let tasks = vec![
            make_task("t1", "o1", &[]),
            make_task("t2", "o1", &["t1"]),
            make_task("t3", "o1", &["t1"]),
        ];
        bb.create_tasks(&tasks).await.unwrap();

        let fetched = bb.get_order("o1").await.unwrap().unwrap();
        assert_eq!(fetched.task_count, 3);
    }

    #[tokio::test]
    async fn tasks_without_deps_start_ready() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();

        let tasks = vec![
            make_task("t1", "o1", &[]),     // no deps → READY
            make_task("t2", "o1", &["t1"]), // has deps → PENDING
        ];
        bb.create_tasks(&tasks).await.unwrap();

        let ready = bb.get_ready_tasks("o1").await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "t1");
    }

    #[tokio::test]
    async fn dependency_resolution() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();

        let tasks = vec![
            make_task("t1", "o1", &[]),
            make_task("t2", "o1", &["t1"]),
            make_task("t3", "o1", &["t1"]),
        ];
        bb.create_tasks(&tasks).await.unwrap();

        // Claim and complete t1
        assert!(bb.claim_task("t1", "w1").await.unwrap());
        let newly_ready = bb.complete_task("t1", "result of t1", 800).await.unwrap();

        // t2 and t3 should now be READY
        assert_eq!(newly_ready.len(), 2);
        assert!(newly_ready.contains(&"t2".to_string()));
        assert!(newly_ready.contains(&"t3".to_string()));
    }

    #[tokio::test]
    async fn atomic_claim() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();
        bb.create_tasks(&[make_task("t1", "o1", &[])])
            .await
            .unwrap();

        // First claim succeeds
        assert!(bb.claim_task("t1", "w1").await.unwrap());
        // Second claim fails (already ACTIVE)
        assert!(!bb.claim_task("t1", "w2").await.unwrap());
    }

    #[tokio::test]
    async fn complete_updates_order() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();
        bb.create_tasks(&[make_task("t1", "o1", &[])])
            .await
            .unwrap();

        bb.claim_task("t1", "w1").await.unwrap();
        bb.complete_task("t1", "done", 500).await.unwrap();

        let order = bb.get_order("o1").await.unwrap().unwrap();
        assert_eq!(order.completed_count, 1);
        assert_eq!(order.total_tokens, 500);
    }

    #[tokio::test]
    async fn fail_increments_retry() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();
        bb.create_tasks(&[make_task("t1", "o1", &[])])
            .await
            .unwrap();

        bb.claim_task("t1", "w1").await.unwrap();
        let status = bb.fail_task("t1", "tool error").await.unwrap();
        // retry_count goes from 0 to 1, max_retries is 3 → back to Ready
        assert_eq!(status, HiveTaskStatus::Ready);

        let task = bb.get_task("t1").await.unwrap().unwrap();
        assert_eq!(task.retry_count, 1);
        assert_eq!(task.error_log.as_deref(), Some("tool error"));
        assert!(task.claimed_by.is_none()); // released
    }

    #[tokio::test]
    async fn fail_escalates_after_max_retries() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();

        let mut task = make_task("t1", "o1", &[]);
        task.max_retries = 2;
        bb.create_tasks(&[task]).await.unwrap();

        // First failure → retry
        bb.claim_task("t1", "w1").await.unwrap();
        let s1 = bb.fail_task("t1", "err1").await.unwrap();
        assert_eq!(s1, HiveTaskStatus::Ready);

        // Second failure → escalate (retry_count=2 >= max_retries=2)
        bb.claim_task("t1", "w2").await.unwrap();
        let s2 = bb.fail_task("t1", "err2").await.unwrap();
        assert_eq!(s2, HiveTaskStatus::Escalate);
    }

    #[tokio::test]
    async fn get_dependency_results() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();

        let tasks = vec![make_task("t1", "o1", &[]), make_task("t2", "o1", &["t1"])];
        bb.create_tasks(&tasks).await.unwrap();

        bb.claim_task("t1", "w1").await.unwrap();
        bb.complete_task("t1", "schema created", 500).await.unwrap();

        let results = bb.get_dependency_results("t2").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "t1");
        assert_eq!(results[0].1, "schema created");
    }

    #[tokio::test]
    async fn order_completion_detection() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();
        bb.create_tasks(&[make_task("t1", "o1", &[]), make_task("t2", "o1", &[])])
            .await
            .unwrap();

        assert!(!bb.is_order_complete("o1").await.unwrap());

        bb.claim_task("t1", "w1").await.unwrap();
        bb.complete_task("t1", "done1", 300).await.unwrap();
        assert!(!bb.is_order_complete("o1").await.unwrap());

        bb.claim_task("t2", "w2").await.unwrap();
        bb.complete_task("t2", "done2", 400).await.unwrap();
        assert!(bb.is_order_complete("o1").await.unwrap());
    }

    #[tokio::test]
    async fn order_results_in_order() {
        let bb = make_bb().await;
        bb.create_order(&make_order("o1")).await.unwrap();
        bb.create_tasks(&[make_task("t1", "o1", &[]), make_task("t2", "o1", &[])])
            .await
            .unwrap();

        bb.claim_task("t1", "w1").await.unwrap();
        bb.complete_task("t1", "first", 100).await.unwrap();
        bb.claim_task("t2", "w2").await.unwrap();
        bb.complete_task("t2", "second", 200).await.unwrap();

        let results = bb.get_order_results("o1").await.unwrap();
        assert_eq!(results.len(), 2);
        // Ordered by completed_at ASC
        assert_eq!(results[0].result_summary.as_deref(), Some("first"));
        assert_eq!(results[1].result_summary.as_deref(), Some("second"));
    }
}
