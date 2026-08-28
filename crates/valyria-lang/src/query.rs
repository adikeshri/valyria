//! Ad-hoc structural queries over a parsed file.
//!
//! [`crate::extract`] runs the *shipped* query set to produce facts for
//! the index. This module serves the other caller: the editing engine,
//! which needs to locate nodes on demand — every identifier with a given
//! name, or whatever a caller-supplied `.scm` pattern captures.
//!
//! It exists so that nothing above layer 2 has to depend on tree-sitter
//! directly. The AST editing strategy is the one place in the runtime
//! that legitimately wants to address syntax nodes, and it addresses them
//! through [`Span`] values from here rather than through a parser handle.

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::error::{LangError, Result};
use crate::extract::span_of;
use crate::parse::CompiledLanguage;
use crate::symbol::Span;

/// Every identifier token in `source` whose text is exactly `name`,
/// in source order.
///
/// Leaf nodes whose kind ends in `identifier`, which is the naming
/// convention every tree-sitter grammar follows (`identifier`,
/// `type_identifier`, `field_identifier`, `property_identifier`). Because
/// it matches nodes rather than text, occurrences inside string literals
/// and comments are not returned — which is exactly the difference
/// between an AST-aware rename and a find-and-replace.
///
/// Not scope-aware: a local variable that shares the name is included.
/// The caller owns that judgement.
pub fn identifier_spans(lang: &CompiledLanguage, source: &str, name: &str) -> Result<Vec<Span>> {
    let parsed = lang.parse(source)?;
    let mut out = Vec::new();
    collect_identifiers(parsed.root(), source, name, &mut out);
    out.sort_by_key(|span| span.start_byte);
    out.dedup_by_key(|span| (span.start_byte, span.end_byte));
    Ok(out)
}

fn collect_identifiers(node: Node<'_>, source: &str, name: &str, out: &mut Vec<Span>) {
    if node.child_count() == 0 {
        if node.kind().ends_with("identifier") {
            let end = node.end_byte().min(source.len());
            let start = node.start_byte().min(end);
            if &source[start..end] == name {
                out.push(span_of(node));
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, source, name, out);
    }
}

/// The spans captured as `@capture` by `query_source`, compiled against
/// this file's grammar, in source order.
///
/// The query is caller-supplied, so a syntax error in it is a normal
/// outcome rather than a bug: it comes back as
/// [`LangError::AdHocQuery`], carrying tree-sitter's own message so the
/// caller can be told what to fix.
pub fn query_spans(
    lang: &CompiledLanguage,
    source: &str,
    query_source: &str,
    capture: &str,
) -> Result<Vec<Span>> {
    let parsed = lang.parse(source)?;
    let language = lang.provider().ts_language();
    let query =
        Query::new(&language, query_source).map_err(|e| LangError::AdHocQuery(e.to_string()))?;

    let capture_index = query
        .capture_names()
        .iter()
        .position(|name| *name == capture)
        .ok_or_else(|| {
            LangError::AdHocQuery(format!(
                "query has no capture named `@{capture}` (it captures: {})",
                query.capture_names().join(", ")
            ))
        })? as u32;

    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(&query, parsed.root(), source.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures {
            if c.index == capture_index {
                out.push(span_of(c.node));
            }
        }
    }

    out.sort_by_key(|span| span.start_byte);
    out.dedup_by_key(|span| (span.start_byte, span.end_byte));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::LanguageRegistry;
    use std::path::Path;
    use std::sync::Arc;

    fn rust() -> Arc<CompiledLanguage> {
        let registry = LanguageRegistry::with_builtin_languages().unwrap();
        registry.for_path(Path::new("a.rs")).unwrap().clone()
    }

    #[test]
    fn finds_every_occurrence_of_an_identifier() {
        let source = "fn helper() {}\nfn run() { helper(); helper(); }\n";
        let spans = identifier_spans(&rust(), source, "helper").unwrap();
        assert_eq!(spans.len(), 3);
        assert!(spans.iter().all(|s| s.slice(source) == "helper"));
    }

    #[test]
    fn does_not_match_inside_strings_or_comments() {
        // The whole reason this is AST-aware rather than a text search.
        let source =
            "// call helper here\nfn run() { let s = \"helper\"; helper(); }\nfn helper() {}\n";
        let spans = identifier_spans(&rust(), source, "helper").unwrap();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn does_not_match_a_longer_identifier_that_contains_the_name() {
        let source = "fn helper() {}\nfn helper_two() {}\n";
        let spans = identifier_spans(&rust(), source, "helper").unwrap();
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn an_absent_identifier_yields_no_spans() {
        let spans = identifier_spans(&rust(), "fn a() {}", "nowhere").unwrap();
        assert!(spans.is_empty());
    }

    #[test]
    fn a_query_returns_its_captured_spans_in_source_order() {
        let source = "fn alpha() {}\nfn beta() {}\n";
        let spans = query_spans(
            &rust(),
            source,
            "(function_item name: (identifier) @name)",
            "name",
        )
        .unwrap();
        let names: Vec<&str> = spans.iter().map(|s| s.slice(source)).collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn a_malformed_query_is_an_error_not_a_panic() {
        // The query is caller-supplied, so this is a normal outcome.
        let err = query_spans(&rust(), "fn a() {}", "(this is not valid", "x").unwrap_err();
        assert!(matches!(err, LangError::AdHocQuery(_)));
    }

    #[test]
    fn asking_for_a_capture_the_query_does_not_have_names_the_ones_it_does() {
        let err = query_spans(
            &rust(),
            "fn a() {}",
            "(function_item name: (identifier) @name)",
            "body",
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("@body"), "{message}");
        assert!(message.contains("name"), "{message}");
    }
}
