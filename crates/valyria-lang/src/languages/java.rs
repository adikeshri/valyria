use crate::provider::{LanguageProvider, LanguageQueries, Tier};

#[derive(Debug, Clone, Copy)]
pub struct Java;

impl LanguageProvider for Java {
    fn id(&self) -> &'static str {
        "java"
    }

    fn display_name(&self) -> &'static str {
        "Java"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: include_str!("../../queries/java/symbols.scm"),
            imports: Some(include_str!("../../queries/java/imports.scm")),
            calls: Some(include_str!("../../queries/java/calls.scm")),
            tests: Some(include_str!("../../queries/java/tests.scm")),
        }
    }

    fn comment_node_kinds(&self) -> &'static [&'static str] {
        &["line_comment", "block_comment"]
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        &[
            "block",
            "class_body",
            "interface_body",
            "enum_body",
            "constructor_body",
            "annotation_type_body",
        ]
    }
}
