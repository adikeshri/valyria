//! A single piece of assembled context (D3): every byte that reaches a
//! prompt carries a trust level and where it came from, not just its text.

use valyria_types::{Provenance, Trust};

#[derive(Debug, Clone, PartialEq)]
pub enum ContextBody {
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    pub trust: Trust,
    pub provenance: Provenance,
    pub tokens: usize,
    pub body: ContextBody,
}

impl ContextItem {
    /// A short human-readable label for this item's source, used by
    /// `AssembledContext::to_messages`'s minimal rendering. The full
    /// trust-ordered, nonce-fenced prompt assembly (D3) lands with the rest
    /// of the context pipeline in Phase 6 — this is just enough to make the
    /// walking skeleton's messages legible.
    pub fn label(&self) -> String {
        match &self.provenance.source {
            valyria_types::ProvenanceSource::File { path } => format!("file: {path}"),
            valyria_types::ProvenanceSource::ToolOutput { invocation } => {
                format!("tool output: {invocation}")
            }
            valyria_types::ProvenanceSource::Git { commit } => format!("git: {commit}"),
            valyria_types::ProvenanceSource::Instruction { path } => {
                format!("instruction: {path}")
            }
            valyria_types::ProvenanceSource::Memory { id } => format!("memory: {id}"),
            valyria_types::ProvenanceSource::ModelTurn => "model turn".to_string(),
        }
    }
}
