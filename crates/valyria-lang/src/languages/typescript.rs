use crate::languages::javascript;
use crate::provider::{LanguageProvider, LanguageQueries, Tier};

/// The TypeScript symbol query is the JavaScript one plus the constructs
/// JavaScript has no equivalent for (interfaces, type aliases, enums,
/// abstract classes). Concatenation rather than duplication: a fix to a
/// shared pattern lands in both languages at once.
const SYMBOLS: &str = concat!(
    include_str!("../../queries/javascript/symbols.scm"),
    "\n",
    include_str!("../../queries/typescript/symbols.scm"),
);

const BODY_KINDS: &[&str] = &[
    "statement_block",
    "class_body",
    "interface_body",
    "enum_body",
    "object",
];

#[derive(Debug, Clone, Copy)]
pub struct TypeScript;

impl LanguageProvider for TypeScript {
    fn id(&self) -> &'static str {
        "typescript"
    }

    fn display_name(&self) -> &'static str {
        "TypeScript"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "mts", "cts"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: SYMBOLS,
            imports: Some(javascript::IMPORTS),
            calls: Some(javascript::CALLS),
            tests: Some(javascript::TESTS),
        }
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        BODY_KINDS
    }
}

/// TSX is a separate grammar, not a flag on the TypeScript one: `<T>(x)`
/// is a type assertion in `.ts` and a JSX element in `.tsx`, so the two
/// cannot share a parser. Everything else — queries, conventions — is
/// identical.
#[derive(Debug, Clone, Copy)]
pub struct Tsx;

impl LanguageProvider for Tsx {
    fn id(&self) -> &'static str {
        "tsx"
    }

    fn display_name(&self) -> &'static str {
        "TypeScript (TSX)"
    }

    fn tier(&self) -> Tier {
        Tier::Full
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tsx"]
    }

    fn ts_language(&self) -> tree_sitter::Language {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    }

    fn queries(&self) -> LanguageQueries {
        LanguageQueries {
            symbols: SYMBOLS,
            imports: Some(javascript::IMPORTS),
            calls: Some(javascript::CALLS),
            tests: Some(javascript::TESTS),
        }
    }

    fn body_node_kinds(&self) -> &'static [&'static str] {
        BODY_KINDS
    }
}
