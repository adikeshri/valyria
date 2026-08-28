//! What retrieval hands to assembly: a trust-tagged, provenance-carrying
//! candidate for a slice of the prompt, with enough structure that
//! compression can lower its fidelity *without ever cutting through a
//! symbol* (§4.17: "no truncated-mid-symbol artifacts").

use serde::{Deserialize, Serialize};
use valyria_types::{Provenance, Trust};

use crate::budget::SectionKind;

/// How much of a candidate is rendered into the prompt. Ordered from most
/// detail to least; the assembler lowers this one step at a time to make
/// an item fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLevel {
    /// Full text / full symbol bodies.
    Full,
    /// Signatures plus the first line of each doc comment.
    Outline,
    /// Signatures only.
    Signature,
    /// A one-line "this exists, ask to see it" pointer.
    Reference,
}

impl CompressionLevel {
    /// The next step down in fidelity, or `None` at [`CompressionLevel::Reference`].
    pub fn next(self) -> Option<CompressionLevel> {
        match self {
            CompressionLevel::Full => Some(CompressionLevel::Outline),
            CompressionLevel::Outline => Some(CompressionLevel::Signature),
            CompressionLevel::Signature => Some(CompressionLevel::Reference),
            CompressionLevel::Reference => None,
        }
    }

    /// Every level from this one down to `Reference`, inclusive.
    pub fn ladder(self) -> Vec<CompressionLevel> {
        let mut out = vec![self];
        let mut cur = self;
        while let Some(next) = cur.next() {
            out.push(next);
            cur = next;
        }
        out
    }
}

/// One symbol within a [`CandidateContent::Source`]. Every string here is
/// an exact slice of the file (or of the index's stored metadata) — the
/// compressor emits these verbatim or drops the whole symbol, so a symbol
/// body is never sliced mid-way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolSpan {
    pub symbol_path: String,
    pub kind: String,
    /// The signature line(s), exactly as extracted.
    pub signature: String,
    pub doc: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    /// The exact source of the symbol, including its trailing newline.
    pub body: String,
    /// 0..1 relevance to the task — the least relevant symbols are dropped
    /// first when a source candidate must shrink.
    pub relevance: f64,
}

/// The payload of a candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum CandidateContent {
    /// Unstructured prose — an instruction file, a memory entry, a git
    /// summary, a prior model turn. Shrinks by dropping whole trailing
    /// lines, then to a one-line reference; never cut mid-line.
    Text { text: String },
    /// Source code with known symbol boundaries.
    Source {
        path: String,
        /// A file-level header (module doc, first comment block), if any.
        header: Option<String>,
        symbols: Vec<SymbolSpan>,
    },
}

impl CandidateContent {
    pub fn text(text: impl Into<String>) -> Self {
        CandidateContent::Text { text: text.into() }
    }
}

/// A candidate slice of context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalCandidate {
    pub trust: Trust,
    pub provenance: Provenance,
    pub section: SectionKind,
    /// 0..1 relevance from retrieval/ranking. Orders items within a
    /// section and decides which degrade first.
    pub relevance: f64,
    pub content: CandidateContent,
}

impl RetrievalCandidate {
    pub fn new(
        trust: Trust,
        provenance: Provenance,
        section: SectionKind,
        relevance: f64,
        content: CandidateContent,
    ) -> Self {
        Self {
            trust,
            provenance,
            section,
            relevance: relevance.clamp(0.0, 1.0),
            content,
        }
    }

    /// A short label for the block header in the assembled prompt, derived
    /// from the provenance source.
    pub fn label(&self) -> String {
        use valyria_types::ProvenanceSource::*;
        match &self.provenance.source {
            File { path } => format!("file: {path}"),
            ToolOutput { invocation } => format!("tool output: {invocation}"),
            Git { commit } => format!("git: {commit}"),
            Instruction { path } => format!("instruction: {path}"),
            Memory { id } => format!("memory: {id}"),
            ModelTurn => "prior model turn".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_descends_to_reference() {
        assert_eq!(
            CompressionLevel::Full.ladder(),
            vec![
                CompressionLevel::Full,
                CompressionLevel::Outline,
                CompressionLevel::Signature,
                CompressionLevel::Reference,
            ]
        );
        assert_eq!(
            CompressionLevel::Signature.ladder(),
            vec![CompressionLevel::Signature, CompressionLevel::Reference]
        );
    }

    #[test]
    fn relevance_is_clamped() {
        let c = RetrievalCandidate::new(
            Trust::RepoData,
            Provenance::new(valyria_types::ProvenanceSource::File { path: "a".into() }),
            SectionKind::Repository,
            2.5,
            CandidateContent::text("x"),
        );
        assert_eq!(c.relevance, 1.0);
    }
}
