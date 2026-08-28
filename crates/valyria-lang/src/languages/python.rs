use crate::provider::{LanguageProvider, LanguageQueries, Tier};

#[derive(Debug, Clone, Copy)]
pub struct Python;

impl LanguageProvider for Python {
    fn id(&self) -> &'static str {
        "python"
    }

    fn display_name(&self) -> &'static str {
        "Python"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: include_str!("../../queries/python/symbols.scm"),
            imports: Some(include_str!("../../queries/python/imports.scm")),
            calls: Some(include_str!("../../queries/python/calls.scm")),
            tests: Some(include_str!("../../queries/python/tests.scm")),
        }
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        &["block"]
    }
}
