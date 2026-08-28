//! What the index stores and hands back.
//!
//! These mirror `valyria-lang`'s facts but add the two things only the
//! index knows: which file a fact came from, and which generations it was
//! true for.

use serde::{Deserialize, Serialize};
use valyria_lang::SymbolKind;
use valyria_types::Generation;
use valyria_util::ContentHash;

/// Workspace-relative path, always `/`-separated regardless of platform,
/// so an index built on Windows and one built on Linux agree — and so a
/// path can be compared without re-normalizing at every call site.
pub type RelPath = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: RelPath,
    /// `None` for a file no compiled-in grammar claims. Such files are
    /// still indexed — lexical search must find them — they just carry no
    /// symbols.
    pub language: Option<String>,
    pub content_hash: ContentHash,
    pub size_bytes: u64,
    pub line_count: u32,
    pub is_binary: bool,
    /// Whether tree-sitter's error recovery fired. `false` for files with
    /// no language, which is honest: nothing tried to parse them.
    pub has_parse_errors: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub path: RelPath,
    pub name: String,
    pub kind: SymbolKind,
    pub symbol_path: String,
    pub span: valyria_lang::Span,
    pub name_span: valyria_lang::Span,
    pub signature: String,
    pub doc: Option<String>,
}

impl SymbolRecord {
    /// The globally unique name of this symbol: `src/parser.rs#Parser::parse`.
    /// This is what the symbol-aware edit strategy and the knowledge graph
    /// address symbols by.
    pub fn qualified_name(&self) -> String {
        format!("{}#{}", self.path, self.symbol_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRecord {
    pub path: RelPath,
    pub raw_path: String,
    pub start_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    pub path: RelPath,
    pub name: String,
    pub enclosing_symbol_path: Option<String>,
    pub start_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRecord {
    pub path: RelPath,
    pub name: String,
    pub symbol_path: String,
    pub start_line: u32,
}

/// What stage of the bootstrap a generation represents.
///
/// Staging is what makes a large repository usable before indexing
/// finishes (§4.14: "a 100k-file repo must be usable before embeddings
/// finish"): the file list is published as its own generation, so lexical
/// search works while symbol extraction is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStage {
    /// Files, hashes and languages only — no symbols yet.
    FilesOnly,
    /// Everything this phase produces.
    Complete,
}

impl GenerationStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            GenerationStage::FilesOnly => "files_only",
            GenerationStage::Complete => "complete",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "files_only" => Some(GenerationStage::FilesOnly),
            "complete" => Some(GenerationStage::Complete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationInfo {
    pub generation: Generation,
    pub stage: GenerationStage,
    pub file_count: u64,
    pub symbol_count: u64,
    pub created_at_ms: i64,
}

/// Reported by a bootstrap or incremental update, and by
/// `storage.inspect` (§48).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStats {
    pub generation: Generation,
    pub stage: GenerationStage,
    pub files: u64,
    pub symbols: u64,
    pub imports: u64,
    pub calls: u64,
    pub tests: u64,
    /// Files whose language is known but which failed to parse cleanly.
    /// Surfaced rather than hidden: a spike here after a grammar upgrade
    /// is the signal that the upgrade broke something.
    pub files_with_parse_errors: u64,
    pub files_without_language: u64,
}

/// What one incremental update changed. Returned so a caller can decide
/// what to invalidate (embeddings, graph edges, cached context) without
/// diffing the whole index itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDelta {
    pub generation: Generation,
    pub added: Vec<RelPath>,
    pub modified: Vec<RelPath>,
    pub removed: Vec<RelPath>,
}

impl IndexDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    /// Every path this update touched, in a stable order.
    pub fn touched(&self) -> Vec<&str> {
        let mut all: Vec<&str> = self
            .added
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
            .map(|s| s.as_str())
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_stage_round_trips_through_its_stored_string() {
        for stage in [GenerationStage::FilesOnly, GenerationStage::Complete] {
            assert_eq!(GenerationStage::parse(stage.as_str()), Some(stage));
        }
        assert_eq!(GenerationStage::parse("embeddings"), None);
    }

    #[test]
    fn a_delta_reports_every_touched_path_once_and_in_order() {
        let delta = IndexDelta {
            generation: Generation(3),
            added: vec!["b.rs".into()],
            modified: vec!["a.rs".into()],
            removed: vec!["c.rs".into(), "a.rs".into()],
        };
        assert_eq!(delta.touched(), ["a.rs", "b.rs", "c.rs"]);
        assert!(!delta.is_empty());
    }

    #[test]
    fn an_empty_delta_is_empty() {
        assert!(IndexDelta::default().is_empty());
    }

    #[test]
    fn a_symbols_qualified_name_includes_its_file() {
        let record = SymbolRecord {
            path: "src/parser.rs".into(),
            name: "parse".into(),
            kind: SymbolKind::Method,
            symbol_path: "Parser::parse".into(),
            span: valyria_lang::Span {
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                end_line: 1,
            },
            name_span: valyria_lang::Span {
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                end_line: 1,
            },
            signature: String::new(),
            doc: None,
        };
        assert_eq!(record.qualified_name(), "src/parser.rs#Parser::parse");
    }
}
