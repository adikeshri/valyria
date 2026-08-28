use crate::provider::{LanguageProvider, LanguageQueries, Tier};

#[derive(Debug, Clone, Copy)]
pub struct Go;

impl LanguageProvider for Go {
    fn id(&self) -> &'static str {
        "go"
    }

    fn display_name(&self) -> &'static str {
        "Go"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: include_str!("../../queries/go/symbols.scm"),
            imports: Some(include_str!("../../queries/go/imports.scm")),
            calls: Some(include_str!("../../queries/go/calls.scm")),
            tests: Some(include_str!("../../queries/go/tests.scm")),
        }
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        &["block", "field_declaration_list", "method_spec_list"]
    }
}
