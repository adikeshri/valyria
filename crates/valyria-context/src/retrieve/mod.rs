//! The retrieval seam.
//!
//! The context pipeline does not care *how* candidates are found — it
//! ranks, compresses, budgets and assembles whatever a [`Retriever`]
//! hands it. Phase 6 ships two:
//!
//! * [`StaticRetriever`] — returns a fixed candidate list. What the
//!   embedded runtime uses today (it drives with explicitly-named files),
//!   and what most pipeline tests use.
//! * [`SearchRetriever`] — runs [`valyria_search::SearchEngine`] and turns
//!   its ranked, explained hits into candidates, pulling symbol structure
//!   from the index so the compressor can degrade a file
//!   symbol-by-symbol. Behind the `intelligence` feature (on by default).
//!
//! [`LiveRetriever`] is the concrete enum `AgentDriver` actually holds
//! (`docs/COMPLETION-PLAN.md` M2): `Static` until an index bootstrap has
//! run for the workspace, `Search` once one has. `valyria-app::Runtime`
//! decides which by whether `index_status` reports a generation yet.

use async_trait::async_trait;
use valyria_types::Generation;

use crate::candidate::RetrievalCandidate;
use crate::error::Result;

#[cfg(feature = "intelligence")]
pub mod search;
#[cfg(feature = "intelligence")]
pub use search::SearchRetriever;

/// What a retriever is asked for.
#[derive(Debug, Clone, Default)]
pub struct RetrievalQuery {
    /// The task intent / topic, in natural language.
    pub intent: String,
    /// Files the task is anchored on — they seed dependency traversal and
    /// pull nearby files up the ranking.
    pub anchors: Vec<String>,
    /// Symbol names of interest (from the intent, an error, a diff).
    pub symbols: Vec<String>,
    /// Error/stack-trace signatures to match against.
    pub error_signatures: Vec<String>,
    /// Files the caller already knows must be included.
    pub explicit_paths: Vec<String>,
    /// Rough cap on how many candidates to return.
    pub limit: usize,
    /// The index generation to read against, if the caller is pinning one.
    pub generation: Option<Generation>,
}

impl RetrievalQuery {
    pub fn new(intent: impl Into<String>) -> Self {
        Self {
            intent: intent.into(),
            limit: 12,
            ..Default::default()
        }
    }

    pub fn anchor(mut self, path: impl Into<String>) -> Self {
        self.anchors.push(path.into());
        self
    }

    pub fn explicit(mut self, path: impl Into<String>) -> Self {
        self.explicit_paths.push(path.into());
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// A source of [`RetrievalCandidate`]s for the context pipeline.
#[async_trait]
pub trait Retriever: Send + Sync {
    async fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalCandidate>>;
}

/// A retriever that returns a fixed list, ignoring the query. The default
/// for a runtime that has not wired real retrieval in yet.
#[derive(Debug, Default, Clone)]
pub struct StaticRetriever {
    candidates: Vec<RetrievalCandidate>,
}

impl StaticRetriever {
    pub fn new(candidates: Vec<RetrievalCandidate>) -> Self {
        Self { candidates }
    }

    pub fn empty() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Retriever for StaticRetriever {
    async fn retrieve(&self, _query: &RetrievalQuery) -> Result<Vec<RetrievalCandidate>> {
        Ok(self.candidates.clone())
    }
}

/// The retriever the live agent loop actually drives with (M1's
/// `RoleRouter` for the model-binding side; this is the retrieval-binding
/// side, `docs/COMPLETION-PLAN.md` M2). `ContextEngine<R>` is generic over
/// a concrete `R: Retriever`, so a runtime that sometimes has a real index
/// and sometimes doesn't (no bootstrap yet, or the `intelligence` feature
/// disabled) needs one concrete type to hand it either way — a trait
/// object would need `Retriever` implemented for `Arc<dyn Retriever>`,
/// which doesn't exist, so this small enum is the simpler seam. Cheap to
/// clone: `StaticRetriever` is a `Vec` clone, and `SearchRetriever` holds
/// only handles (a `SearchEngine` over `Arc<Store>`-backed stores) — never
/// a materialized index in memory.
#[derive(Debug, Clone)]
pub enum LiveRetriever {
    Static(StaticRetriever),
    #[cfg(feature = "intelligence")]
    Search(SearchRetriever),
}

impl Default for LiveRetriever {
    fn default() -> Self {
        Self::Static(StaticRetriever::empty())
    }
}

impl LiveRetriever {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Retriever for LiveRetriever {
    async fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalCandidate>> {
        match self {
            LiveRetriever::Static(r) => r.retrieve(query).await,
            #[cfg(feature = "intelligence")]
            LiveRetriever::Search(r) => r.retrieve(query).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::{Provenance, ProvenanceSource, Trust};

    use crate::budget::SectionKind;
    use crate::candidate::CandidateContent;

    #[tokio::test]
    async fn static_retriever_echoes_its_candidates() {
        let c = RetrievalCandidate::new(
            Trust::RepoData,
            Provenance::new(ProvenanceSource::File { path: "a".into() }),
            SectionKind::Repository,
            0.5,
            CandidateContent::text("x"),
        );
        let r = StaticRetriever::new(vec![c.clone()]);
        let got = r.retrieve(&RetrievalQuery::new("anything")).await.unwrap();
        assert_eq!(got, vec![c]);
    }

    #[tokio::test]
    async fn empty_static_retriever_returns_nothing() {
        let got = StaticRetriever::empty()
            .retrieve(&RetrievalQuery::new("x"))
            .await
            .unwrap();
        assert!(got.is_empty());
    }
}
