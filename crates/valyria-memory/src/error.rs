use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Store(#[from] valyria_store::StoreError),
    #[error("memory row `{0}` has an unrecognized {1}")]
    Corrupt(String, &'static str),
    #[error("confidence must be in [0, 1], got {0}")]
    BadConfidence(f64),
}

impl ErrorCode for MemoryError {
    fn code(&self) -> &'static str {
        match self {
            MemoryError::Store(_) => "memory.store",
            MemoryError::Corrupt(..) => "memory.corrupt",
            MemoryError::BadConfidence(_) => "memory.bad_confidence",
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, MemoryError::Store(e) if e.retryable())
    }
}

pub type Result<T> = std::result::Result<T, MemoryError>;
