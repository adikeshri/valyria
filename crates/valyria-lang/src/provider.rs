//! [`LanguageProvider`]: the whole of D9 — "language support is data + a
//! trait, never a `match` on extension".
//!
//! A provider contributes a grammar, a declarative query set, and a
//! handful of naming conventions. It contains no extraction logic: that
//! lives once, in [`crate::extract`], driven entirely by the capture names
//! in the `.scm` files. Adding a language is therefore adding a directory
//! under `queries/` plus a ~40-line provider — never an edit to the
//! extraction engine.

use serde::{Deserialize, Serialize};

/// How much the runtime can do with a language.
///
/// The distinction is honest rather than aspirational: a tier-2 language
/// is indexed for structure and searched lexically, but nothing claims to
/// know its call graph, so ranking and impact analysis do not silently
/// produce empty answers that look like real ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Symbols, imports, tests, and call sites — full participation in
    /// the knowledge graph.
    Full,
    /// Symbols and imports only. `calls.scm` is absent, and the graph
    /// records no `Calls` edges for files in this language.
    StructureOnly,
}

/// The declarative query set backing one language. Every field is the
/// text of a `.scm` file compiled against the provider's grammar.
///
/// `symbols` is required; the rest are optional so a tier-2 language can
/// ship structure only.
#[derive(Debug, Clone, Copy)]
pub struct LanguageQueries {
    pub symbols: &'static str,
    pub imports: Option<&'static str>,
    pub calls: Option<&'static str>,
    pub tests: Option<&'static str>,
}

/// Everything the runtime needs to know about one language.
pub trait LanguageProvider: Send + Sync + std::fmt::Debug {
    /// Stable identifier, persisted in the index and used in config and
    /// protocol payloads. Never change one of these without a migration.
    fn id(&self) -> &'static str;

    fn display_name(&self) -> &'static str;

    fn tier(&self) -> Tier;

    /// Extensions without the leading dot, lowercase.
    fn extensions(&self) -> &'static [&'static str];

    /// Exact file names that identify this language regardless of
    /// extension (`Makefile`, `Dockerfile`, `go.mod`). Most languages have
    /// none.
    fn filenames(&self) -> &'static [&'static str] {
        &[]
    }

    fn ts_language(&self) -> tree_sitter::Language;

    fn queries(&self) -> LanguageQueries;

    /// Separator joining a nested symbol to its container when building a
    /// symbol path: `::` for Rust, `.` for most others.
    fn path_separator(&self) -> &'static str {
        "."
    }

    /// Node kinds whose text is a doc comment. Used to attach the comment
    /// block immediately above a definition without every `symbols.scm`
    /// having to capture it by hand.
    fn comment_node_kinds(&self) -> &'static [&'static str] {
        &["comment"]
    }

    /// Node kinds that delimit a definition's body. Everything before the
    /// first such child is the signature — which is what outline-level
    /// context compression sends instead of the whole definition.
    fn body_node_kinds(&self) -> &'static [&'static str] {
        &[
            "block",
            "body",
            "class_body",
            "declaration_list",
            "statement_block",
        ]
    }
}

/// Whether `path`'s file name matches this provider (by exact name first,
/// then by extension). Extension comparison is case-insensitive because
/// macOS and Windows filesystems are.
pub fn matches_path(provider: &dyn LanguageProvider, path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if provider.filenames().contains(&name) {
        return true;
    }
    // Deliberately not `Path::extension`: it returns `gitignore` for
    // `.gitignore`, treating a dotfile's whole name as an extension.
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() {
        return false;
    }
    let lower = ext.to_ascii_lowercase();
    provider.extensions().iter().any(|e| *e == lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Debug)]
    struct Fake;

    impl LanguageProvider for Fake {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn display_name(&self) -> &'static str {
            "Fake"
        }
        fn tier(&self) -> Tier {
            Tier::StructureOnly
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["fk", "fake"]
        }
        fn filenames(&self) -> &'static [&'static str] {
            &["Fakefile"]
        }
        fn ts_language(&self) -> tree_sitter::Language {
            unreachable!("path matching never touches the grammar")
        }
        fn queries(&self) -> LanguageQueries {
            LanguageQueries {
                symbols: "",
                imports: None,
                calls: None,
                tests: None,
            }
        }
    }

    #[test]
    fn matches_by_extension() {
        assert!(matches_path(&Fake, Path::new("src/a.fk")));
        assert!(matches_path(&Fake, Path::new("src/a.fake")));
        assert!(!matches_path(&Fake, Path::new("src/a.rs")));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(matches_path(&Fake, Path::new("src/A.FK")));
    }

    #[test]
    fn matches_by_exact_file_name() {
        assert!(matches_path(&Fake, Path::new("nested/Fakefile")));
        assert!(!matches_path(&Fake, Path::new("nested/fakefile")));
    }

    #[test]
    fn a_dotfile_is_not_treated_as_a_bare_extension() {
        // `.fk` is a dotfile named ".fk", not a file with extension "fk";
        // `Path::extension` disagrees, which is why `matches_path` does
        // its own splitting.
        assert!(!matches_path(&Fake, Path::new(".fk")));
    }

    #[test]
    fn a_file_with_no_extension_never_matches_by_extension() {
        assert!(!matches_path(&Fake, Path::new("README")));
    }
}
