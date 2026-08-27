use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("including `{path}` would exceed the context budget (needs ~{needed} tokens, {remaining} remaining)")]
    BudgetExceeded {
        path: String,
        needed: usize,
        remaining: usize,
    },
    #[error("failed to read `{path}` for context: {message}")]
    ReadFailed { path: String, message: String },
}

impl ErrorCode for ContextError {
    fn code(&self) -> &'static str {
        match self {
            ContextError::BudgetExceeded { .. } => "context.budget_exceeded",
            ContextError::ReadFailed { .. } => "context.read_failed",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, ContextError>;
