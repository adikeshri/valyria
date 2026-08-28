use valyria_types::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ModelStoreError {
    #[error("model {id:?} is not installed")]
    NotInstalled { id: String },
    #[error("model {id:?} is already installed")]
    AlreadyInstalled { id: String },
    #[error("install plan for {id:?} was not confirmed — the caller must acknowledge size and license first")]
    Unconfirmed { id: String },
    #[error("download of {id:?} failed: {detail}")]
    Download { id: String, detail: String },
    #[error(
        "integrity check failed for {id:?}: expected blake3 {expected}, got {actual} — the file was deleted"
    )]
    IntegrityMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("probe of {id:?} failed: {detail}")]
    Probe { id: String, detail: String },
    #[error("download of {id:?} was cancelled")]
    Cancelled { id: String },
    #[error("model store i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("model store serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("model store database error: {0}")]
    Store(#[from] valyria_store::StoreError),
}

impl ErrorCode for ModelStoreError {
    fn code(&self) -> &'static str {
        match self {
            ModelStoreError::NotInstalled { .. } => "model_store.not_installed",
            ModelStoreError::AlreadyInstalled { .. } => "model_store.already_installed",
            ModelStoreError::Unconfirmed { .. } => "model_store.unconfirmed",
            ModelStoreError::Download { .. } => "model_store.download",
            ModelStoreError::IntegrityMismatch { .. } => "model_store.integrity_mismatch",
            ModelStoreError::Probe { .. } => "model_store.probe",
            ModelStoreError::Cancelled { .. } => "model_store.cancelled",
            ModelStoreError::Io(_) => "model_store.io",
            ModelStoreError::Serde(_) => "model_store.serde",
            ModelStoreError::Store(_) => "model_store.store",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            ModelStoreError::Download { .. }
                | ModelStoreError::Io(_)
                | ModelStoreError::Cancelled { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, ModelStoreError>;
