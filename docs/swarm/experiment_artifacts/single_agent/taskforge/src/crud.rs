use crate::db::Database;
use crate::error::TaskForgeError;
use crate::models::{CreateTaskRequest, Task, TaskStatus};
use chrono::Utc;
use uuid::Uuid;

pub async fn create_task(db: &Database, req: &CreateTaskRequest) -> Result<Task, TaskForgeError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let priority = format!("{:?}", req.priority).to_lowercase();
    let status = "todo";

    sqlx::query(
        "INSERT INTO tasks (id, title, description, priority, status, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&req.title)
    .bind(&req.description)
    .bind(priority)
    .bind(status)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await
    .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))?;

    get_task(db, &id).await
}

pub async fn get_task(db: &Database, id: &str) -> Result<Task, TaskForgeError> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))?
        .ok_or_else(|| TaskForgeError::NotFound(id.to_string()))
}

pub async fn list_tasks(db: &Database) -> Result<Vec<Task>, TaskForgeError> {
    sqlx::query_as::<_, Task>("SELECT * FROM tasks ORDER BY created_at DESC")
        .fetch_all(db.pool())
        .await
        .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))
}

pub async fn update_status(db: &Database, id: &str, status: TaskStatus) -> Result<Task, TaskForgeError> {
    let status_str = format!("{:?}", status).to_lowercase();
    let now = Utc::now().to_rfc3339();

    sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE id = ?")
        .bind(status_str)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await
        .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))?;

    get_task(db, id).await
}

pub async fn delete_task(db: &Database, id: &str) -> Result<(), TaskForgeError> {
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(id)
        .execute(db.pool())
        .await
        .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))?;
    Ok(())
}