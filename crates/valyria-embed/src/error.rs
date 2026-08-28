//! Embedding errors.

use valyria_types::{ErrorCode, Generation};

pub type Result<T> = std::result::Result<T, EmbedError>;

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding storage: {0}")]
    Store(#[from] valyria_store::StoreError),

    #[error("index: {0}")]
    Index(#[from] valyria_index::IndexError),

    /// A search was asked for a generation whose vectors were never
    /// built. Distinct from "no vectors matched": a caller must be able
    /// to tell "nothing is close to this query" from "nobody has embedded
    /// this generation yet", because only the second is worth acting on
    /// (it means semantic search silently degrades to the other modes).
    #[error("no embeddings have been built for index generation {0}")]
    NotBuilt(Generation),

    /// A stored vector's dimensionality does not match the embedder in
    /// use. This can only happen if the configured embedder changed
    /// between builds without a rebuild; surfaced rather than silently
    /// comparing vectors of different lengths.
    #[error("embedding dimension mismatch: store holds {found}, embedder produces {expected}")]
    DimensionMismatch { expected: usize, found: usize },
}

impl ErrorCode for EmbedError {
    fn code(&self) -> &'static str {
        match self {
            EmbedError::Store(_) => "embed.store",
            EmbedError::Index(_) => "embed.index",
            EmbedError::NotBuilt(_) => "embed.not_built",
            EmbedError::DimensionMismatch { .. } => "embed.dimension_mismatch",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}
