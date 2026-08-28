//! `valyria-search` — layer 2 (Repository intelligence).
//!
//! One query, several ways of answering it, one ranked and explained
//! result.
//!
//! The modes (§4.16) are independent: lexical and regex scan file
//! contents, symbol goes through the index, semantic goes through
//! [`valyria_embed`], AST runs a tree-sitter pattern, dependency walks
//! [`valyria_graph`] from the task's anchor files, and git reads recent
//! history. [`SearchEngine`] runs the ones a query asked for, or a
//! sensible default set, and any mode with nothing to contribute —
//! no embeddings, not a git repo, no anchors — steps aside with a note
//! rather than failing the search. **Search works fully with embeddings
//! disabled**; it is simply less good.
//!
//! Ranking is [reciprocal-rank fusion](fusion) across the modes followed
//! by a small feature reranker (recency, churn, import-graph distance,
//! test proximity, a path prior). Every hit carries a
//! [`ScoreExplanation`](result::ScoreExplanation) whose features sum
//! *exactly* to the hit's score — "why this file?" (§14) is answered
//! from stored data, not reconstructed, and the number can never drift
//! from its own explanation.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod engine;
pub mod error;
pub mod fusion;
pub mod modes;
pub mod query;
pub mod result;

pub use engine::SearchEngine;
pub use error::{Result, SearchError};
pub use fusion::{FeatureWeights, RankContext};
pub use query::{SearchMode, SearchQuery};
pub use result::{Feature, ScoreExplanation, SearchHit, SearchResults, StageScore};
