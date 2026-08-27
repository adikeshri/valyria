use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("generation was cancelled")]
    Cancelled,
    #[error("generation timed out")]
    Timeout,
    #[error("model produced malformed output: {detail}")]
    MalformedOutput { detail: String },
    #[error("model runtime unavailable: {reason}")]
    Unavailable { reason: String },
}

impl ErrorCode for ModelError {
    fn code(&self) -> &'static str {
        match self {
            ModelError::Cancelled => "model.cancelled",
            ModelError::Timeout => "model.timeout",
            ModelError::MalformedOutput { .. } => "model.malformed_output",
            ModelError::Unavailable { .. } => "model.unavailable",
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, ModelError::Timeout | ModelError::Unavailable { .. })
    }
}

pub type Result<T> = std::result::Result<T, ModelError>;
