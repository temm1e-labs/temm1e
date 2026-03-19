use thiserror::Error;

#[derive(Debug, Error)]
pub enum NoteVaultError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

pub type Result<T, E = NoteVaultError> = std::result::Result<T, E>;