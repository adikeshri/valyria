use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration {version} failed: {source}")]
    Migration {
        version: i64,
        #[source]
        source: rusqlite::Error,
    },
    #[error("store actor is shut down")]
    ActorShutDown,
    #[error("blob {0} not found")]
    BlobNotFound(String),
    #[error("blob content hash mismatch: expected {expected}, wrote {actual}")]
    BlobHashMismatch { expected: String, actual: String },
}

impl ErrorCode for StoreError {
    fn code(&self) -> &'static str {
        match self {
            StoreError::Sqlite(_) => "store.sqlite",
            StoreError::Io(_) => "store.io",
            StoreError::Migration { .. } => "store.migration",
            StoreError::ActorShutDown => "store.actor_shutdown",
            StoreError::BlobNotFound(_) => "store.blob_not_found",
            StoreError::BlobHashMismatch { .. } => "store.blob_hash_mismatch",
        }
    }

    fn retryable(&self) -> bool {
        matches!(self, StoreError::ActorShutDown)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
