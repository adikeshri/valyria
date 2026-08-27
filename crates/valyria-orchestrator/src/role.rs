//! Model roles (§38). Phase 3 only needs enough to route a request to the
//! single registered fake-model adapter; the full role set
//! (FAST_CODER, PLANNER, REVIEWER, EMBEDDER, RERANKER, AUTOCOMPLETE,
//! SUMMARIZER) and fallback-chain escalation land in Phase 9.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    PrimaryCoder,
}
