//! Witness error type.
//!
//! All Witness operations return `Result<T, WitnessError>`. The error type
//! is designed to flow upward into `temm1e_core::Temm1eError::Witness`
//! (added as a new variant).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WitnessError {
    #[error("invalid predicate: {0}")]
    InvalidPredicate(String),

    #[error("lenient oath: {0}")]
    LenientOath(String),

    #[error("no sealed oath for subtask {0}")]
    NoSealedOath(String),

    #[error("ledger error: {0}")]
    Ledger(String),

    #[error("ledger tamper detected: expected root {expected}, got {actual}")]
    TamperDetected { expected: String, actual: String },

    #[error("predicate check failed: {0}")]
    PredicateCheck(String),

    #[error("template variable missing: {0}")]
    MissingTemplateVar(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("utf8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("cost budget exceeded for task {0}")]
    CostBudgetExceeded(String),

    #[error("oath already sealed for subtask {0}")]
    AlreadySealed(String),

    #[error("verifier timeout after {0}ms")]
    Timeout(u64),

    #[error("witness internal error: {0}")]
    Internal(String),
}

impl WitnessError {
    /// Returns true if this error indicates a tamper event.
    pub fn is_tamper(&self) -> bool {
        matches!(self, WitnessError::TamperDetected { .. })
    }

    /// Returns true if this error indicates a lenient oath that should be retried.
    pub fn is_lenient_oath(&self) -> bool {
        matches!(self, WitnessError::LenientOath(_))
    }
}
