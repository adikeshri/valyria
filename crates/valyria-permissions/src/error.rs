use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("no pending ask found for this exact request")]
    NoPendingAsk,
    #[error("request was denied: {0}")]
    Denied(String),
}

impl ErrorCode for PermissionError {
    fn code(&self) -> &'static str {
        match self {
            PermissionError::NoPendingAsk => "permissions.no_pending_ask",
            PermissionError::Denied(_) => "permissions.denied",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, PermissionError>;
