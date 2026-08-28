//! The typed graph vocabulary (§13).
//!
//! Node ids are strings with a kind prefix (`file:src/parser.rs`,
//! `sym:src/parser.rs#Parser::parse`) rather than integers. That costs
//! some storage and buys three things worth more: an id is meaningful in a
//! log line or a protocol payload without a join, it is stable across
//! rebuilds (an integer id would not be), and it can be constructed by a
//! caller that knows a path and a symbol without querying first.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Symbol,
    /// A directory that groups files: the closest language-neutral
    /// equivalent of a module, and what "which area of the repository is
    /// this?" means in practice.
    Module,
    Test,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Symbol => "sym",
            NodeKind::Module => "mod",
            NodeKind::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(NodeKind::File),
            "sym" => Some(NodeKind::Symbol),
            "mod" => Some(NodeKind::Module),
            "test" => Some(NodeKind::Test),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn file(path: &str) -> Self {
        NodeId(format!("file:{path}"))
    }

    pub fn module(dir: &str) -> Self {
        NodeId(format!("mod:{dir}"))
    }

    pub fn symbol(path: &str, symbol_path: &str) -> Self {
        NodeId(format!("sym:{path}#{symbol_path}"))
    }

    pub fn test(path: &str, symbol_path: &str) -> Self {
        NodeId(format!("test:{path}#{symbol_path}"))
    }

    pub fn kind(&self) -> Option<NodeKind> {
        self.0.split_once(':').and_then(|(k, _)| NodeKind::parse(k))
    }

    /// The file a node belongs to, for every kind that belongs to one.
    /// `None` for a module, which is a directory rather than a file.
    pub fn file_path(&self) -> Option<&str> {
        let (kind, rest) = self.0.split_once(':')?;
        match NodeKind::parse(kind)? {
            NodeKind::File => Some(rest),
            NodeKind::Symbol | NodeKind::Test => Some(rest.split_once('#')?.0),
            NodeKind::Module => None,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Short display name: a file's base name, a symbol's identifier.
    pub name: String,
    /// The file (or directory, for a module) this node lives at.
    pub path: String,
    /// Set for symbols and tests.
    pub symbol_path: Option<String>,
    pub language: Option<String>,
    pub start_line: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Module contains file; file contains symbol; symbol contains nested
    /// symbol.
    Contains,
    /// File imports file.
    Imports,
    /// Symbol calls symbol.
    Calls,
    /// File defines symbol — the direct lookup behind "where is this
    /// defined?", kept separate from `Contains` so a traversal can ask for
    /// definitions without also walking nesting.
    Defines,
    /// Test exercises symbol. The edge §4.26's verification strategy walks
    /// to answer "which tests cover this change?".
    Tests,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Imports => "imports",
            EdgeKind::Calls => "calls",
            EdgeKind::Defines => "defines",
            EdgeKind::Tests => "tests",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "contains" => Some(EdgeKind::Contains),
            "imports" => Some(EdgeKind::Imports),
            "calls" => Some(EdgeKind::Calls),
            "defines" => Some(EdgeKind::Defines),
            "tests" => Some(EdgeKind::Tests),
            _ => None,
        }
    }
}

/// How much to trust an edge.
///
/// Without a type system the runtime can only resolve a call by name, and
/// names collide. Recording *how* an edge was derived, rather than
/// dropping the uncertain ones or presenting them as facts, is what lets
/// ranking prefer the certain ones and lets `--explain` say why a file was
/// pulled into context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Structural, derived from the syntax tree alone: containment,
    /// definitions. Cannot be wrong.
    Exact,
    /// One plausible target after narrowing — a unique name, or a unique
    /// match in an imported file.
    Likely,
    /// Several targets share the name and nothing distinguishes them. The
    /// edge is recorded to each, so a consumer sees the ambiguity instead
    /// of a coin flip.
    Ambiguous,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Likely => "likely",
            Confidence::Ambiguous => "ambiguous",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(Confidence::Exact),
            "likely" => Some(Confidence::Likely),
            "ambiguous" => Some(Confidence::Ambiguous),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub confidence: Confidence,
}

/// A reference the graph could not bind to anything in the repository —
/// an import of an external crate, a call into the standard library.
///
/// Kept rather than discarded: "this file depends on `serde`" is a real
/// fact about the repository even though `serde` is not in it, and
/// dependency-aware search needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedRef {
    pub from: NodeId,
    pub kind: EdgeKind,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow edges away from the node: what does X call?
    Outgoing,
    /// Follow edges into the node: who calls X?
    Incoming,
    Both,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subgraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// What a change to one file could plausibly affect (§4.14's
/// `impact_of(path)`), and the input to §4.26's "which verification is
/// worth running?" decision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactSet {
    /// The file the change is in.
    pub origin: String,
    /// Files that reach the origin through imports or calls, nearest
    /// first.
    pub affected_files: Vec<String>,
    /// Tests with a path to the origin — what to run.
    pub covering_tests: Vec<NodeId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphStats {
    pub nodes: u64,
    pub edges: u64,
    pub unresolved: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_expose_their_kind_and_file() {
        let file = NodeId::file("src/parser.rs");
        assert_eq!(file.kind(), Some(NodeKind::File));
        assert_eq!(file.file_path(), Some("src/parser.rs"));

        let symbol = NodeId::symbol("src/parser.rs", "Parser::parse");
        assert_eq!(symbol.kind(), Some(NodeKind::Symbol));
        assert_eq!(symbol.file_path(), Some("src/parser.rs"));
        assert_eq!(symbol.as_str(), "sym:src/parser.rs#Parser::parse");

        let module = NodeId::module("src");
        assert_eq!(module.kind(), Some(NodeKind::Module));
        assert_eq!(module.file_path(), None);
    }

    #[test]
    fn a_symbol_path_containing_a_hash_still_resolves_its_file() {
        // Splitting on the *first* `#` is what makes this work; a symbol
        // path may legitimately contain one (a Rust macro, say).
        let id = NodeId::symbol("src/m.rs", "make#tag");
        assert_eq!(id.file_path(), Some("src/m.rs"));
    }

    #[test]
    fn a_malformed_id_has_no_kind_rather_than_a_wrong_one() {
        assert_eq!(NodeId("not-an-id".into()).kind(), None);
        assert_eq!(NodeId("widget:x".into()).kind(), None);
    }

    #[test]
    fn every_enum_round_trips_through_its_stored_string() {
        for kind in [
            NodeKind::File,
            NodeKind::Symbol,
            NodeKind::Module,
            NodeKind::Test,
        ] {
            assert_eq!(NodeKind::parse(kind.as_str()), Some(kind));
        }
        for kind in [
            EdgeKind::Contains,
            EdgeKind::Imports,
            EdgeKind::Calls,
            EdgeKind::Defines,
            EdgeKind::Tests,
        ] {
            assert_eq!(EdgeKind::parse(kind.as_str()), Some(kind));
        }
        for confidence in [Confidence::Exact, Confidence::Likely, Confidence::Ambiguous] {
            assert_eq!(Confidence::parse(confidence.as_str()), Some(confidence));
        }
    }

    #[test]
    fn confidence_orders_from_most_to_least_trustworthy() {
        assert!(Confidence::Exact < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Ambiguous);
    }
}
