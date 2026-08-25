use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error("store error: {0}")]
    Store(#[from] valyria_store::StoreError),
    #[error("event bus is shut down")]
    ShutDown,
    #[error("malformed persisted event: {0}")]
    Corrupt(String),
}

impl ErrorCode for EventsError {
    fn code(&self) -> &'static str {
        match self {
            EventsError::Store(_) => "events.store",
            EventsError::ShutDown => "events.shutdown",
            EventsError::Corrupt(_) => "events.corrupt",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}

pub type Result<T> = std::result::Result<T, EventsError>;
