//! The trust lattice (D3) and provenance tracking.
//!
//! Every piece of context assembled for a model carries a [`Trust`] level
//! and a [`Provenance`] record. This is what makes prompt-injection defense
//! a property of one function (context assembly, in `valyria-context`)
//! rather than a hope distributed across the codebase, and what answers
//! "why was this file included in model context?" (§14) directly from
//! stored data instead of after-the-fact reconstruction.

use serde::{Deserialize, Serialize};

use crate::id::{MemoryId, ToolInvocationId};

/// How much authority a piece of context is allowed to carry.
///
/// Ordered from highest authority to lowest. Prompt assembly enforces that
/// nothing below [`Trust::Instruction`] may occupy a system/policy position
/// in the assembled prompt, and everything at [`Trust::Evidence`] or below
/// is delimited with a nonce-fenced envelope before being shown to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Trust {
    /// Runtime-owned system prompt, compiled in. Highest authority.
    Policy,
    /// An authorized instruction source, per the authority order in §33
    /// (e.g. a `VALYRIA.md` or `AGENTS.md` the runtime has validated).
    Instruction,
    /// Tool output, git state, compiler/test results — factual but
    /// untrusted *as instructions*. May contain adversarial text if the
    /// repository or a command's output was crafted to inject.
    Evidence,
    /// Raw file contents from the repository.
    RepoData,
    /// A prior model generation, replayed back into context.
    ModelOutput,
}

impl Trust {
    /// Lower rank = higher authority. Explicit rather than relying on enum
    /// declaration order, so the meaning survives reordering variants.
    pub fn authority_rank(self) -> u8 {
        match self {
            Trust::Policy => 0,
            Trust::Instruction => 1,
            Trust::Evidence => 2,
            Trust::RepoData => 3,
            Trust::ModelOutput => 4,
        }
    }

    /// Whether content at this trust level may be placed in a system/policy
    /// position in an assembled prompt (as opposed to a fenced data block).
    pub fn may_occupy_system_position(self) -> bool {
        self.authority_rank() <= Trust::Instruction.authority_rank()
    }

    /// Whether content at this trust level must be nonce-fenced as untrusted
    /// data when assembled into a prompt.
    pub fn requires_fencing(self) -> bool {
        !self.may_occupy_system_position()
    }
}

impl PartialOrd for Trust {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Trust {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.authority_rank().cmp(&other.authority_rank())
    }
}

/// Where a context item came from and how it was found, so retrieval
/// decisions are auditable and explainable after the fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: ProvenanceSource,
    /// The sequence of retrieval/ranking stages that produced this item,
    /// e.g. `["search:lexical", "rank:fusion", "expand:definition"]`.
    /// This is the data `search --explain` and `context.explain` render.
    pub retrieval_path: Vec<String>,
    /// The final ranking score, if this item came from a ranked retrieval
    /// rather than being explicitly requested.
    pub score: Option<f64>,
}

impl Provenance {
    pub fn new(source: ProvenanceSource) -> Self {
        Self {
            source,
            retrieval_path: Vec::new(),
            score: None,
        }
    }

    pub fn with_step(mut self, step: impl Into<String>) -> Self {
        self.retrieval_path.push(step.into());
        self
    }

    pub fn with_score(mut self, score: f64) -> Self {
        self.score = Some(score);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProvenanceSource {
    File {
        path: String,
    },
    ToolOutput {
        invocation: ToolInvocationId,
    },
    Git {
        commit: String,
    },
    Instruction {
        path: String,
    },
    Memory {
        id: MemoryId,
    },
    /// A prior turn's model output, replayed into context.
    ModelTurn,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_outranks_everything() {
        assert!(Trust::Policy < Trust::Instruction);
        assert!(Trust::Instruction < Trust::Evidence);
        assert!(Trust::Evidence < Trust::RepoData);
        assert!(Trust::RepoData < Trust::ModelOutput);
    }

    #[test]
    fn only_policy_and_instruction_may_occupy_system_position() {
        assert!(Trust::Policy.may_occupy_system_position());
        assert!(Trust::Instruction.may_occupy_system_position());
        assert!(!Trust::Evidence.may_occupy_system_position());
        assert!(!Trust::RepoData.may_occupy_system_position());
        assert!(!Trust::ModelOutput.may_occupy_system_position());
    }

    #[test]
    fn fencing_is_the_complement_of_system_position() {
        for t in [
            Trust::Policy,
            Trust::Instruction,
            Trust::Evidence,
            Trust::RepoData,
            Trust::ModelOutput,
        ] {
            assert_eq!(t.requires_fencing(), !t.may_occupy_system_position());
        }
    }

    #[test]
    fn provenance_builder_records_path() {
        let p = Provenance::new(ProvenanceSource::File {
            path: "src/lib.rs".into(),
        })
        .with_step("search:lexical")
        .with_step("rank:fusion")
        .with_score(0.87);

        assert_eq!(p.retrieval_path, vec!["search:lexical", "rank:fusion"]);
        assert_eq!(p.score, Some(0.87));
    }
}
