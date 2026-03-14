//! Persistent Task Queue with Checkpointing — persists tasks to SQLite
//! so they survive process restarts. Each task captures the user's goal,
//! current status, and a checkpoint of the session state (serialized history).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, info};
use uuid::Uuid;

/// Status of a task in the queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Result<Self, Temm1eError> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(Temm1eError::Internal(format!(
                "Unknown task status: {other}"
            ))),
        }
    }
}

/// A single task entry in the persistent queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub task_id: String,
    pub chat_id: String,
    pub goal: String,
    pub status: TaskStatus,
    /// Serialized session history as JSON — the checkpoint.
    pub checkpoint_data: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// SQLite-backed persistent task queue.
pub struct TaskQueue {
    pool: SqlitePool,
}

impl TaskQueue {
    /// Create a new TaskQueue and initialise the schema.
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect(database_url)
            .await
            .map_err(|e| Temm1eError::Internal(format!("TaskQueue connect failed: {e}")))?;

        let tq = Self { pool };
        tq.init_tables().await?;
        info!("Task queue initialised");
        Ok(tq)
    }

    async fn init_tables(&self) -> Result<(), Temm1eError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS temm1e_tasks (
                task_id         TEXT PRIMARY KEY,
                chat_id         TEXT NOT NULL,
                goal            TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'pending',
                checkpoint_data TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("TaskQueue create table failed: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_status ON temm1e_tasks(status)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Internal(format!("TaskQueue create index failed: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tasks_chat ON temm1e_tasks(chat_id)")
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Internal(format!("TaskQueue create index failed: {e}")))?;

        Ok(())
    }

    /// Create a new task and return its ID.
    pub async fn create_task(&self, chat_id: &str, goal: &str) -> Result<String, Temm1eError> {
        let task_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO temm1e_tasks (task_id, chat_id, goal, status, created_at, updated_at)
            VALUES (?, ?, ?, 'pending', ?, ?)
            "#,
        )
        .bind(&task_id)
        .bind(chat_id)
        .bind(goal)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("TaskQueue create failed: {e}")))?;

        debug!(task_id = %task_id, chat_id = %chat_id, "Created task");
        Ok(task_id)
    }

    /// Update the status of a task.
    pub async fn update_status(
        &self,
        task_id: &str,
        status: TaskStatus,
    ) -> Result<(), Temm1eError> {
        let now = Utc::now().to_rfc3339();

        sqlx::query("UPDATE temm1e_tasks SET status = ?, updated_at = ? WHERE task_id = ?")
            .bind(status.as_str())
            .bind(&now)
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(|e| Temm1eError::Internal(format!("TaskQueue update_status failed: {e}")))?;

        debug!(task_id = %task_id, status = %status.as_str(), "Updated task status");
        Ok(())
    }

    /// Save a checkpoint of the current session state for a task.
    pub async fn checkpoint(&self, task_id: &str, session_data: &str) -> Result<(), Temm1eError> {
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE temm1e_tasks SET checkpoint_data = ?, updated_at = ? WHERE task_id = ?",
        )
        .bind(session_data)
        .bind(&now)
        .bind(task_id)
        .execute(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("TaskQueue checkpoint failed: {e}")))?;

        debug!(task_id = %task_id, "Checkpointed task");
        Ok(())
    }

    /// Load all incomplete tasks (Pending or Running).
    pub async fn load_incomplete(&self) -> Result<Vec<TaskEntry>, Temm1eError> {
        let rows: Vec<TaskRow> = sqlx::query_as(
            r#"
            SELECT task_id, chat_id, goal, status, checkpoint_data, created_at, updated_at
            FROM temm1e_tasks
            WHERE status IN ('pending', 'running')
            ORDER BY created_at ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Temm1eError::Internal(format!("TaskQueue load_incomplete failed: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }

    /// List all tasks, optionally filtered by chat_id.
    pub async fn list_tasks(&self, chat_id: Option<&str>) -> Result<Vec<TaskEntry>, Temm1eError> {
        let rows: Vec<TaskRow> = if let Some(cid) = chat_id {
            sqlx::query_as(
                r#"
                SELECT task_id, chat_id, goal, status, checkpoint_data, created_at, updated_at
                FROM temm1e_tasks
                WHERE chat_id = ?
                ORDER BY created_at DESC
                "#,
            )
            .bind(cid)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                r#"
                SELECT task_id, chat_id, goal, status, checkpoint_data, created_at, updated_at
                FROM temm1e_tasks
                ORDER BY created_at DESC
                "#,
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|e| Temm1eError::Internal(format!("TaskQueue list_tasks failed: {e}")))?;

        rows.into_iter().map(row_to_entry).collect()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TaskRow {
    task_id: String,
    chat_id: String,
    goal: String,
    status: String,
    checkpoint_data: Option<String>,
    created_at: String,
    updated_at: String,
}

fn row_to_entry(row: TaskRow) -> Result<TaskEntry, Temm1eError> {
    let status = TaskStatus::from_str(&row.status)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&row.created_at)
        .map_err(|e| Temm1eError::Internal(format!("Invalid created_at: {e}")))?
        .with_timezone(&Utc);
    let updated_at = chrono::DateTime::parse_from_rfc3339(&row.updated_at)
        .map_err(|e| Temm1eError::Internal(format!("Invalid updated_at: {e}")))?
        .with_timezone(&Utc);

    Ok(TaskEntry {
        task_id: row.task_id,
        chat_id: row.chat_id,
        goal: row.goal,
        status,
        checkpoint_data: row.checkpoint_data,
        created_at,
        updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_queue() -> TaskQueue {
        TaskQueue::new("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn create_and_list_task() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "deploy the app").await.unwrap();
        assert!(!id.is_empty());

        let tasks = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].goal, "deploy the app");
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn update_status() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "test task").await.unwrap();

        tq.update_status(&id, TaskStatus::Running).await.unwrap();
        let tasks = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Running);

        tq.update_status(&id, TaskStatus::Completed).await.unwrap();
        let tasks = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn checkpoint_saves_data() {
        let tq = make_queue().await;
        let id = tq
            .create_task("chat-1", "task with checkpoint")
            .await
            .unwrap();

        let session_json = r#"{"history": [{"role": "user", "content": "hello"}]}"#;
        tq.checkpoint(&id, session_json).await.unwrap();

        let tasks = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(tasks[0].checkpoint_data.as_deref(), Some(session_json));
    }

    #[tokio::test]
    async fn load_incomplete_filters_correctly() {
        let tq = make_queue().await;
        let id1 = tq.create_task("chat-1", "pending task").await.unwrap();
        let id2 = tq.create_task("chat-1", "running task").await.unwrap();
        let id3 = tq.create_task("chat-1", "done task").await.unwrap();

        tq.update_status(&id2, TaskStatus::Running).await.unwrap();
        tq.update_status(&id3, TaskStatus::Completed).await.unwrap();

        let incomplete = tq.load_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 2);
        let ids: Vec<&str> = incomplete.iter().map(|t| t.task_id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
        assert!(!ids.contains(&id3.as_str()));
    }

    #[tokio::test]
    async fn list_tasks_no_filter() {
        let tq = make_queue().await;
        tq.create_task("chat-1", "task a").await.unwrap();
        tq.create_task("chat-2", "task b").await.unwrap();

        let all = tq.list_tasks(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn list_tasks_chat_filter() {
        let tq = make_queue().await;
        tq.create_task("chat-1", "task a").await.unwrap();
        tq.create_task("chat-2", "task b").await.unwrap();

        let filtered = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].chat_id, "chat-1");
    }

    #[tokio::test]
    async fn status_roundtrip() {
        assert_eq!(
            TaskStatus::from_str("pending").unwrap(),
            TaskStatus::Pending
        );
        assert_eq!(
            TaskStatus::from_str("running").unwrap(),
            TaskStatus::Running
        );
        assert_eq!(
            TaskStatus::from_str("completed").unwrap(),
            TaskStatus::Completed
        );
        assert_eq!(TaskStatus::from_str("failed").unwrap(), TaskStatus::Failed);
        assert!(TaskStatus::from_str("unknown").is_err());
    }

    #[tokio::test]
    async fn failed_task_not_in_incomplete() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "failing task").await.unwrap();
        tq.update_status(&id, TaskStatus::Failed).await.unwrap();

        let incomplete = tq.load_incomplete().await.unwrap();
        assert!(incomplete.is_empty());
    }

    #[tokio::test]
    async fn multiple_checkpoints_overwrite() {
        let tq = make_queue().await;
        let id = tq.create_task("chat-1", "iterating task").await.unwrap();

        tq.checkpoint(&id, "round-1").await.unwrap();
        tq.checkpoint(&id, "round-2").await.unwrap();
        tq.checkpoint(&id, "round-3").await.unwrap();

        let tasks = tq.list_tasks(Some("chat-1")).await.unwrap();
        assert_eq!(tasks[0].checkpoint_data.as_deref(), Some("round-3"));
    }
}
