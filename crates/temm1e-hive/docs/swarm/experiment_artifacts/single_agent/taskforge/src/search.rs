use crate::db::Database;
use crate::error::TaskForgeError;
use crate::models::{Task, TaskFilter};
use sqlx::Sqlite;

pub async fn search_tasks(db: &Database, filter: &TaskFilter) -> Result<Vec<Task>, TaskForgeError> {
    let mut sql = String::from("SELECT * FROM tasks WHERE 1=1");
    let mut binds: Vec<String> = Vec::new();

    if let Some(s) = &filter.status {
        sql.push_str(" AND status = ?");
        binds.push(format!("{:?}", s).to_lowercase());
    }

    if let Some(p) = &filter.priority {
        sql.push_str(" AND priority = ?");
        binds.push(format!("{:?}", p).to_lowercase());
    }

    if let Some(search) = &filter.search {
        sql.push_str(" AND (title LIKE ? OR description LIKE ?)");
        binds.push(format!("%{}%", search));
        binds.push(format!("%{}%", search));
    }

    sql.push_str(" ORDER BY created_at DESC");

    let mut query = sqlx::query_as::<Sqlite, Task>(&sql);

    for b in &binds {
        query = query.bind(b);
    }

    query
        .fetch_all(db.pool())
        .await
        .map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))
}