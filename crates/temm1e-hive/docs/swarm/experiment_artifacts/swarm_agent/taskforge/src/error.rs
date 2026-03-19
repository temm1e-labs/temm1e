use thiserror::Error;

#[derive(Error, Debug)]
pub enum TaskForgeError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Task not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<sqlx::Error> for TaskForgeError {
    fn from(err: sqlx::Error) -> Self {
        TaskForgeError::Database(err.to_string())
    }
}

impl From<serde_json::Error> for TaskForgeError {
    fn from(err: serde_json::Error) -> Self {
        TaskForgeError::Serialization(err.to_string())
    }
}