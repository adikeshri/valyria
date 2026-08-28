//! Index errors.

use valyria_types::{ErrorCode, Generation};

pub type Result<T> = std::result::Result<T, IndexError>;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index storage: {0}")]
    Store(#[from] valyria_store::StoreError),

    #[error("workspace filesystem: {0}")]
    Vfs(#[from] valyria_vfs::VfsError),

    #[error("language extraction: {0}")]
    Lang(#[from] valyria_lang::LangError),

    /// A read was requested at a generation that has been pruned, or that
    /// was never published. Surfacing this rather than silently answering
    /// from the current generation is the point of D8: a step that planned
    /// against a vanished snapshot must be told, not quietly given newer
    /// data.
    #[error("index generation {0} is not available (never published, or pruned)")]
    UnknownGeneration(Generation),

    #[error("the index has not been built for this workspace yet")]
    NotIndexed,
}

impl ErrorCode for IndexError {
    fn code(&self) -> &'static str {
        match self {
            IndexError::Store(_) => "index.store",
            IndexError::Vfs(_) => "index.vfs",
            IndexError::Lang(_) => "index.extraction",
            IndexError::UnknownGeneration(_) => "index.unknown_generation",
            IndexError::NotIndexed => "index.not_indexed",
        }
    }

    fn retryable(&self) -> bool {
        // Rebuilding is a different operation, not a retry of the same
        // one: none of these succeed if simply re-issued unchanged.
        false
    }
}
