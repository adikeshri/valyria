//! Model roles (§38). The full set the orchestrator routes over —
//! `valyria-orchestrator` re-exports this as `Role` so callers keep a
//! stable path while the canonical definition lives at the model layer,
//! next to the catalog that scores models *for* each role.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A capability slot the runtime needs a model for. A single installed
/// model can serve several roles (a good coder model is usually a fine
/// summarizer); the catalog scores every `(model, role)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// The main implementation model — deepest reasoning, highest quality.
    PrimaryCoder,
    /// A cheaper, faster model tried first for routine edits; escalates to
    /// [`ModelRole::PrimaryCoder`] on low confidence or failure (§38).
    FastCoder,
    /// Plan authoring and revision.
    Planner,
    /// Review passes — reads a change and reports findings, never writes.
    Reviewer,
    /// Text/code embeddings for semantic search.
    Embedder,
    /// Cross-encoder reranking of retrieved candidates.
    Reranker,
    /// Inline single-line completion.
    Autocomplete,
    /// Conversation and task-log summarization.
    Summarizer,
}

impl ModelRole {
    pub const ALL: [ModelRole; 8] = [
        ModelRole::PrimaryCoder,
        ModelRole::FastCoder,
        ModelRole::Planner,
        ModelRole::Reviewer,
        ModelRole::Embedder,
        ModelRole::Reranker,
        ModelRole::Autocomplete,
        ModelRole::Summarizer,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ModelRole::PrimaryCoder => "primary_coder",
            ModelRole::FastCoder => "fast_coder",
            ModelRole::Planner => "planner",
            ModelRole::Reviewer => "reviewer",
            ModelRole::Embedder => "embedder",
            ModelRole::Reranker => "reranker",
            ModelRole::Autocomplete => "autocomplete",
            ModelRole::Summarizer => "summarizer",
        }
    }

    /// Eviction priority for the model pool (§4.22): when memory is tight
    /// and a model must be unloaded, the pool keeps higher-priority roles
    /// resident. The primary coder is the task's critical path and evicts
    /// last; autocomplete is the most disposable.
    pub fn priority(&self) -> u8 {
        match self {
            ModelRole::PrimaryCoder => 100,
            ModelRole::Planner => 80,
            ModelRole::FastCoder => 70,
            ModelRole::Reviewer => 60,
            ModelRole::Summarizer => 40,
            ModelRole::Embedder => 35,
            ModelRole::Reranker => 30,
            ModelRole::Autocomplete => 10,
        }
    }

    /// The role this one escalates to when its model is unavailable or
    /// returns a low-confidence / malformed result (§38's escalation rule).
    /// `None` means "no automatic escalation — surface the failure".
    pub fn escalates_to(&self) -> Option<ModelRole> {
        match self {
            ModelRole::FastCoder => Some(ModelRole::PrimaryCoder),
            ModelRole::Autocomplete => Some(ModelRole::FastCoder),
            _ => None,
        }
    }
}

impl fmt::Display for ModelRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown model role: {0}")]
pub struct RoleParseError(String);

impl FromStr for ModelRole {
    type Err = RoleParseError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        ModelRole::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| RoleParseError(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_round_trips_for_every_role() {
        for role in ModelRole::ALL {
            assert_eq!(role.as_str().parse::<ModelRole>().unwrap(), role);
        }
    }

    #[test]
    fn json_is_snake_case() {
        let json = serde_json::to_string(&ModelRole::PrimaryCoder).unwrap();
        assert_eq!(json, "\"primary_coder\"");
    }

    #[test]
    fn primary_coder_outranks_everything_for_eviction() {
        for role in ModelRole::ALL {
            if role != ModelRole::PrimaryCoder {
                assert!(ModelRole::PrimaryCoder.priority() > role.priority());
            }
        }
    }

    #[test]
    fn fast_coder_escalates_to_primary() {
        assert_eq!(
            ModelRole::FastCoder.escalates_to(),
            Some(ModelRole::PrimaryCoder)
        );
        assert_eq!(ModelRole::PrimaryCoder.escalates_to(), None);
    }
}
