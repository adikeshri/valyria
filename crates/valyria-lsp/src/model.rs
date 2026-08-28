//! Language-server results, translated into the runtime's own vocabulary.
//!
//! Nothing above this crate sees an LSP type. The reason is §4.13's
//! `SymbolResolver`: index-derived and LSP-derived results are merged, and
//! merging requires them to be the same shape — with each result marked by
//! where it came from, so ranking can prefer the higher-fidelity one.

use serde::{Deserialize, Serialize};
use valyria_lang::SymbolKind;

/// A position in a file. Both are 0-based here because LSP is 0-based,
/// and converting at the boundary in exactly one place is how off-by-one
/// bugs are avoided; [`Location::start_line_1based`] is the display form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Workspace-relative when the server's URI was inside the workspace,
    /// and an absolute path otherwise (a definition in a dependency's
    /// source, which is a legitimate and useful answer).
    pub path: String,
    pub start: Position,
    pub end: Position,
}

impl Location {
    pub fn start_line_1based(&self) -> u32 {
        self.start.line + 1
    }
}

/// Where a result came from. §4.13: "marks each result's source so ranking
/// can prefer the higher-fidelity one".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSource {
    /// Derived from the repository index — always available, name-based.
    Index,
    /// Derived from a language server — type-aware, and therefore
    /// preferred when both agree on a location and disagree on which one
    /// is right.
    LanguageServer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    pub location: Location,
    pub source: ResultSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

impl Severity {
    /// LSP numbers severities 1-4. An unknown number becomes a warning
    /// rather than being dropped: a diagnostic the runtime cannot classify
    /// is still a diagnostic.
    pub fn from_lsp(value: i64) -> Self {
        match value {
            1 => Severity::Error,
            2 => Severity::Warning,
            3 => Severity::Information,
            4 => Severity::Hint,
            _ => Severity::Warning,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub location: Location,
    pub severity: Severity,
    /// The server's own rule identifier (`E0308`, `no-unused-vars`) when
    /// it sends one — the handle a repair loop needs to look a failure up.
    pub code: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

/// What the server said it can do, from the `initialize` handshake.
///
/// Every field is used as a gate: asking a server for something it did not
/// advertise wastes a round trip and, with some servers, hangs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub definition: bool,
    pub references: bool,
    pub document_symbols: bool,
    pub hover: bool,
    pub rename: bool,
    /// 1 = full document sync, 2 = incremental. The client sends full
    /// documents either way; this records what the server asked for.
    pub text_document_sync: u8,
}

/// LSP's `SymbolKind` numbering, mapped onto the runtime's own kinds.
///
/// The mapping is lossy in one direction on purpose: LSP distinguishes
/// kinds the runtime has no use for (`Key`, `Null`, `Event`), and folding
/// them into the nearest kind it does model beats inventing enum variants
/// nothing consumes.
pub fn symbol_kind_from_lsp(value: i64) -> SymbolKind {
    match value {
        2 => SymbolKind::Module,
        4 => SymbolKind::Module, // Package
        5 => SymbolKind::Class,
        6 => SymbolKind::Method,
        7 => SymbolKind::Field, // Property
        8 => SymbolKind::Field,
        9 => SymbolKind::Method, // Constructor
        10 => SymbolKind::Enum,
        11 => SymbolKind::Interface,
        12 => SymbolKind::Function,
        13 => SymbolKind::Variable,
        14 => SymbolKind::Constant,
        23 => SymbolKind::Struct,
        26 => SymbolKind::TypeAlias, // TypeParameter
        _ => SymbolKind::Variable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_the_four_lsp_levels() {
        assert_eq!(Severity::from_lsp(1), Severity::Error);
        assert_eq!(Severity::from_lsp(4), Severity::Hint);
    }

    #[test]
    fn an_unknown_severity_becomes_a_warning_rather_than_being_dropped() {
        assert_eq!(Severity::from_lsp(99), Severity::Warning);
        assert_eq!(Severity::from_lsp(0), Severity::Warning);
    }

    #[test]
    fn severity_orders_most_serious_first() {
        assert!(Severity::Error < Severity::Warning);
        assert!(Severity::Warning < Severity::Hint);
    }

    #[test]
    fn lsp_symbol_kinds_map_onto_the_runtimes_own() {
        assert_eq!(symbol_kind_from_lsp(12), SymbolKind::Function);
        assert_eq!(symbol_kind_from_lsp(6), SymbolKind::Method);
        assert_eq!(symbol_kind_from_lsp(23), SymbolKind::Struct);
    }

    #[test]
    fn an_unmodelled_lsp_kind_folds_into_the_nearest_one() {
        assert_eq!(symbol_kind_from_lsp(20), SymbolKind::Variable); // Null
        assert_eq!(symbol_kind_from_lsp(999), SymbolKind::Variable);
    }

    #[test]
    fn positions_convert_to_the_1_based_form_humans_and_compilers_use() {
        let location = Location {
            path: "src/lib.rs".into(),
            start: Position {
                line: 0,
                character: 4,
            },
            end: Position {
                line: 0,
                character: 8,
            },
        };
        assert_eq!(location.start_line_1based(), 1);
    }
}
