use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use crate::error::TaskForgeError;

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn new(url: &str) -> Result<Self, TaskForgeError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(url)
            .await
            .map_err(|e| TaskForgeError::Database(e.to_string()))?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT,
                priority TEXT NOT NULL DEFAULT 'medium',
                status TEXT NOT NULL DEFAULT 'todo',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| TaskForgeError::Database(e.to_string()))?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}