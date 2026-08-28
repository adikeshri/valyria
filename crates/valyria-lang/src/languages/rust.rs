use crate::provider::{LanguageProvider, LanguageQueries, Tier};

#[derive(Debug, Clone, Copy)]
pub struct Rust;

impl LanguageProvider for Rust {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn display_name(&self) -> &'static str {
        "Rust"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: include_str!("../../queries/rust/symbols.scm"),
            imports: Some(include_str!("../../queries/rust/imports.scm")),
            calls: Some(include_str!("../../queries/rust/calls.scm")),
            tests: Some(include_str!("../../queries/rust/tests.scm")),
        }
    }

    fn path_separator(&self) -> &'static str {
        "::"
    }

    fn comment_node_kinds(&self) -> &'static [&'static str] {
        &["line_comment", "block_comment"]
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        &[
            "block",
            "declaration_list",
            "field_declaration_list",
            "enum_variant_list",
        ]
    }
}
