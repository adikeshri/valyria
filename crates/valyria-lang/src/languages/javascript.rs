use crate::provider::{LanguageProvider, LanguageQueries, Tier};

#[derive(Debug, Clone, Copy)]
pub struct JavaScript;

/// The JavaScript query set, shared verbatim with TypeScript and TSX
/// (their grammars are supersets, so every pattern here compiles against
/// them too).
pub(crate) const SYMBOLS: &str = include_str!("../../queries/javascript/symbols.scm");
pub(crate) const IMPORTS: &str = include_str!("../../queries/javascript/imports.scm");
pub(crate) const CALLS: &str = include_str!("../../queries/javascript/calls.scm");
pub(crate) const TESTS: &str = include_str!("../../queries/javascript/tests.scm");

pub(crate) const BODY_KINDS: &[&str] = &["statement_block", "class_body", "object"];

impl LanguageProvider for JavaScript {
    fn id(&self) -> &'static str {
        "javascript"
    }

    fn display_name(&self) -> &'static str {
        "JavaScript"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "mjs", "cjs", "jsx"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: SYMBOLS,
            imports: Some(IMPORTS),
            calls: Some(CALLS),
            tests: Some(TESTS),
        }
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        BODY_KINDS
    }
}
