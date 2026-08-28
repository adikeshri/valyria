//! What extraction produces: the language-neutral facts about one source
//! file that the index (`valyria-index`) and knowledge graph
//! (`valyria-graph`) are built from.
//!
//! These types are deliberately *not* tree-sitter types. Nothing above
//! this crate should need to know that tree-sitter exists — that is what
//! makes D9 ("language support is data + a trait") hold: swapping or
//! adding a parser changes this crate only.

use serde::{Deserialize, Serialize};

/// A byte-and-line span within one file. Byte offsets are what edits and
/// chunking need; line numbers are what humans and compilers speak.
/// Lines are 1-based (matching every compiler and editor); byte offsets
/// are 0-based and half-open (`start..end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: u32,
    pub end_line: u32,
}

impl Span {
    pub fn contains(&self, other: &Span) -> bool {
        self.start_byte <= other.start_byte && other.end_byte <= self.end_byte
    }

    /// Strictly contains: `other` is inside `self` and they are not the
    /// same span. Used for nesting, where a node must not be its own
    /// parent.
    pub fn strictly_contains(&self, other: &Span) -> bool {
        self.contains(other)
            && (self.start_byte, self.end_byte) != (other.start_byte, other.end_byte)
    }

    pub fn len_bytes(&self) -> usize {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        let end = self.end_byte.min(source.len());
        let start = self.start_byte.min(end);
        &source[start..end]
    }
}

/// The kinds of definition the runtime distinguishes. Deliberately a
/// closed enum rather than a free string: ranking priors, graph edge
/// rules, and the symbol-aware edit strategy all match on this, and a
/// typo in a `.scm` capture name should fail loudly at query-compile time
/// rather than silently create a new "kind" nobody handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Module,
    Constant,
    Variable,
    Field,
    TypeAlias,
    Macro,
    Test,
}

impl SymbolKind {
    /// Maps the capture-name suffix used in `symbols.scm`
    /// (`@definition.function`, `@definition.struct`, …) to a kind.
    /// Returns `None` for an unrecognized suffix so
    /// [`crate::provider::LanguageProvider`] validation can reject a query
    /// file that invents a capture nobody consumes.
    pub fn from_capture_suffix(suffix: &str) -> Option<Self> {
        Some(match suffix {
            "function" => SymbolKind::Function,
            "method" => SymbolKind::Method,
            "class" => SymbolKind::Class,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "interface" => SymbolKind::Interface,
            "trait" => SymbolKind::Trait,
            "module" => SymbolKind::Module,
            "constant" => SymbolKind::Constant,
            "variable" => SymbolKind::Variable,
            "field" => SymbolKind::Field,
            "type_alias" => SymbolKind::TypeAlias,
            "macro" => SymbolKind::Macro,
            "test" => SymbolKind::Test,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Module => "module",
            SymbolKind::Constant => "constant",
            SymbolKind::Variable => "variable",
            SymbolKind::Field => "field",
            SymbolKind::TypeAlias => "type_alias",
            SymbolKind::Macro => "macro",
            SymbolKind::Test => "test",
        }
    }

    /// Whether this kind can lexically contain other definitions, and so
    /// should be considered when computing a nested symbol's path.
    /// A `Function` qualifies (closures and nested `fn`s exist) but is
    /// listed here mainly so a method inside an `impl` inside a `mod`
    /// gets the full path.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::Trait
                | SymbolKind::Module
        )
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::from_capture_suffix(s)
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One definition found in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Language-appropriate qualified path within the file, e.g.
    /// `Parser::parse` (Rust), `Parser.parse` (Python/Java/TS). Never
    /// includes the file path — the index owns that half of the identity.
    pub symbol_path: String,
    /// The whole definition, including its body: what a symbol-aware edit
    /// replaces and what the outline compressor summarizes.
    pub span: Span,
    /// The identifier alone — what "jump to definition" should land on.
    pub name_span: Span,
    /// First line(s) of the definition up to the body, used for
    /// outline-level context compression (`Full → Outline → Signature`).
    pub signature: String,
    /// Doc comment immediately preceding the definition, if the language's
    /// `symbols.scm` captures one.
    pub doc: Option<String>,
}

/// An import/use/require statement. `raw_path` is exactly what the source
/// said (`std::collections::HashMap`, `./util`, `github.com/x/y`);
/// resolving it to a file in the workspace is the index's job, since only
/// the index knows what files exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    pub raw_path: String,
    pub span: Span,
}

/// A call site. `name` is the callee identifier as written; binding it to
/// a definition happens in `valyria-graph`, which can see the whole
/// repository. `enclosing_symbol_path` is filled in during extraction by
/// containment, so the graph can build `caller -> callee` edges without
/// re-parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub name: String,
    pub span: Span,
    pub enclosing_symbol_path: Option<String>,
}

/// A test function/case, detected by the language's `tests.scm` (naming
/// convention, attribute, or decorator depending on the ecosystem).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub symbol_path: String,
    pub span: Span,
}

/// Everything one file yields in a single parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFacts {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub calls: Vec<Call>,
    pub tests: Vec<TestCase>,
    /// True when tree-sitter's error recovery had to fire. The file is
    /// still indexed (partial facts beat no facts), but the editing engine
    /// uses this to enforce "if the file parsed before, it must parse
    /// after" without penalizing a file that never parsed.
    pub has_parse_errors: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(a: usize, b: usize) -> Span {
        Span {
            start_byte: a,
            end_byte: b,
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn containment_is_inclusive_but_strict_containment_is_not() {
        let outer = span(0, 100);
        let same = span(0, 100);
        let inner = span(10, 20);

        assert!(outer.contains(&same));
        assert!(!outer.strictly_contains(&same));
        assert!(outer.strictly_contains(&inner));
        assert!(!inner.contains(&outer));
    }

    #[test]
    fn slice_is_clamped_to_the_source_length() {
        let s = "hello";
        assert_eq!(span(0, 5).slice(s), "hello");
        assert_eq!(span(0, 999).slice(s), "hello");
        assert_eq!(span(999, 1000).slice(s), "");
    }

    #[test]
    fn capture_suffixes_round_trip_through_kind() {
        for kind in [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Trait,
            SymbolKind::Module,
            SymbolKind::Constant,
            SymbolKind::Variable,
            SymbolKind::Field,
            SymbolKind::TypeAlias,
            SymbolKind::Macro,
            SymbolKind::Test,
        ] {
            assert_eq!(SymbolKind::from_capture_suffix(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn unknown_capture_suffix_is_rejected() {
        assert_eq!(SymbolKind::from_capture_suffix("widget"), None);
    }

    #[test]
    fn symbol_kind_serde_is_the_stable_snake_case_string() {
        let json = serde_json::to_string(&SymbolKind::TypeAlias).unwrap();
        assert_eq!(json, "\"type_alias\"");
    }
}
