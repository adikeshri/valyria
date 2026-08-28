//! Graph errors.

use valyria_types::{ErrorCode, Generation};

pub type Result<T> = std::result::Result<T, GraphError>;

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("graph storage: {0}")]
    Store(#[from] valyria_store::StoreError),

    #[error("index: {0}")]
    Index(#[from] valyria_index::IndexError),

    /// The graph has not been built for this index generation. Distinct
    /// from "the graph is empty": a caller must be able to tell "nothing
    /// relates to anything" from "nobody has computed the relationships
    /// yet", because only one of those is worth acting on.
    #[error("no graph has been built for index generation {0}")]
    NotBuilt(Generation),
}

impl ErrorCode for GraphError {
    fn code(&self) -> &'static str {
        match self {
            GraphError::Store(_) => "graph.store",
            GraphError::Index(_) => "graph.index",
            GraphError::NotBuilt(_) => "graph.not_built",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}
