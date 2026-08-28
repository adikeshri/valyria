//! Error types for `valyria-plan`.
//!
//! Note the split: [`PlanFormatError`] is a *parse-time* failure (the JSON
//! isn't a well-formed plan at all), while the [`crate::validate`] module's
//! `PlanError` is a *semantic* rejection of an otherwise well-formed plan,
//! carried as a `Vec` so the model gets every problem at once.

use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanFormatError {
    #[error("invalid plan step id `{got}`: {why}")]
    StepId { got: String, why: &'static str },
    #[error("plan payload is not valid JSON for a plan: {0}")]
    Json(String),
}

impl ErrorCode for PlanFormatError {
    fn code(&self) -> &'static str {
        match self {
            PlanFormatError::StepId { .. } => "plan.bad_step_id",
            PlanFormatError::Json(_) => "plan.bad_json",
        }
    }
    fn retryable(&self) -> bool {
        false
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("format error: {0}")]
    Format(#[from] PlanFormatError),
    #[error("store error: {0}")]
    Store(#[from] valyria_store::StoreError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("rollback error: {0}")]
    Rollback(#[from] crate::checkpoint::RollbackError),
}

impl ErrorCode for PlanError {
    fn code(&self) -> &'static str {
        match self {
            PlanError::Format(e) => e.code(),
            PlanError::Store(_) => "plan.store",
            PlanError::Json(_) => "plan.json",
            PlanError::Rollback(_) => "plan.rollback",
        }
    }
    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, PlanError>;
