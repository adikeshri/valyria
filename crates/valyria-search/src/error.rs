//! Search errors.

use valyria_types::ErrorCode;

pub type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("index: {0}")]
    Index(#[from] valyria_index::IndexError),

    #[error("knowledge graph: {0}")]
    Graph(#[from] valyria_graph::GraphError),

    #[error("embeddings: {0}")]
    Embed(#[from] valyria_embed::EmbedError),

    #[error("git: {0}")]
    Git(#[from] valyria_git::GitError),

    #[error("search storage: {0}")]
    Store(#[from] valyria_store::StoreError),

    /// The regex or AST pattern a caller supplied did not compile. Only
    /// raised when that mode was asked for *explicitly* — when search is
    /// choosing modes itself, a bad pattern just means that mode
    /// contributes nothing (see `SearchResults::degraded`).
    #[error("invalid {kind} pattern: {message}")]
    BadPattern { kind: &'static str, message: String },

    /// No index has been built for the workspace, so there is nothing to
    /// search. Distinct from "the search matched nothing".
    #[error("the workspace has not been indexed yet")]
    NotIndexed,
}

impl ErrorCode for SearchError {
    fn code(&self) -> &'static str {
        match self {
            SearchError::Index(_) => "search.index",
            SearchError::Graph(_) => "search.graph",
            SearchError::Embed(_) => "search.embed",
            SearchError::Git(_) => "search.git",
            SearchError::Store(_) => "search.store",
            SearchError::BadPattern { .. } => "search.bad_pattern",
            SearchError::NotIndexed => "search.not_indexed",
        }
    }

    fn retryable(&self) -> bool {
        false
    }
}
