//! The extraction engine: one implementation, driven entirely by capture
//! names, shared by every language.
//!
//! Capture vocabulary (this is the contract a `queries/<lang>/` directory
//! is written against):
//!
//! | capture | meaning |
//! |---|---|
//! | `@definition.<kind>` | the whole definition node; `<kind>` is a [`SymbolKind`] |
//! | `@name` | the identifier naming that definition |
//! | `@container.name` | an explicit path prefix for the definition (e.g. the type in a Rust `impl` block) |
//! | `@doc` | an explicit doc node, overriding the comment-block heuristic |
//! | `@import` / `@import.path` | an import statement and the module path within it |
//! | `@reference.call` / `@name` | a call site and its callee identifier |
//! | `@test` / `@test.name` | a test case and its name |
//!
//! An unrecognized `@definition.*` suffix is a hard error at query
//! *validation* time (see [`crate::registry::LanguageRegistry::validate`])
//! rather than a silently dropped capture, because the failure mode of
//! silence here is "this language mysteriously has no structs".

use tree_sitter::{Node, QueryCursor, StreamingIterator};

use crate::error::Result;
use crate::parse::CompiledLanguage;
use crate::provider::{LanguageProvider, Tier};
use crate::symbol::{Call, FileFacts, Import, Span, Symbol, SymbolKind, TestCase};

const DEFINITION_PREFIX: &str = "definition.";

pub fn span_of(node: Node<'_>) -> Span {
    Span {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
    }
}

/// Extract every fact this language's query set can produce from one
/// already-parsed file.
pub fn extract(lang: &CompiledLanguage, source: &str) -> Result<FileFacts> {
    let parsed = lang.parse(source)?;
    let root = parsed.root();
    let bytes = source.as_bytes();

    let symbols = extract_symbols(lang, root, bytes, source);
    let imports = extract_imports(lang, root, bytes);
    // A tier-2 language ships no `calls.scm`, so this is empty by
    // construction rather than by a silent failure to match anything.
    let calls = if lang.provider().tier() == Tier::Full {
        extract_calls(lang, root, bytes, &symbols)
    } else {
        Vec::new()
    };
    let tests = extract_tests(lang, root, bytes, &symbols);

    Ok(FileFacts {
        symbols,
        imports,
        calls,
        tests,
        has_parse_errors: parsed.has_errors(),
    })
}

/// How specific a kind is, used to resolve two patterns matching the same
/// node. `Method` beats `Function` (it carries a receiver), `Struct` beats
/// `TypeAlias` (Go's catch-all), and anything named beats `Variable`.
fn kind_specificity(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Variable => 0,
        SymbolKind::Function | SymbolKind::TypeAlias | SymbolKind::Constant | SymbolKind::Field => {
            1
        }
        SymbolKind::Method
        | SymbolKind::Class
        | SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Interface
        | SymbolKind::Trait
        | SymbolKind::Module
        | SymbolKind::Macro
        | SymbolKind::Test => 2,
    }
}

/// One raw `@definition.*` match, before path resolution.
struct RawDefinition<'t> {
    node: Node<'t>,
    name_node: Node<'t>,
    kind: SymbolKind,
    explicit_containers: Vec<String>,
    doc: Option<String>,
}

fn extract_symbols(
    lang: &CompiledLanguage,
    root: Node<'_>,
    bytes: &[u8],
    source: &str,
) -> Vec<Symbol> {
    let provider = lang.provider();
    let query = &lang.symbols;
    let capture_names = query.capture_names();

    let mut cursor = QueryCursor::new();
    let mut raw: Vec<RawDefinition<'_>> = Vec::new();

    let mut matches = cursor.matches(query, root, bytes);
    while let Some(m) = matches.next() {
        let mut def: Option<(Node<'_>, SymbolKind)> = None;
        let mut name_node: Option<Node<'_>> = None;
        let mut containers: Vec<String> = Vec::new();
        let mut doc: Option<String> = None;

        for capture in m.captures {
            let capture_name = capture_names[capture.index as usize];
            match capture_name {
                "name" => name_node = Some(capture.node),
                "container.name" => containers.push(text_of(capture.node, bytes).to_string()),
                "doc" => doc = Some(text_of(capture.node, bytes).to_string()),
                other => {
                    if let Some(suffix) = other.strip_prefix(DEFINITION_PREFIX) {
                        if let Some(kind) = SymbolKind::from_capture_suffix(suffix) {
                            def = Some((capture.node, kind));
                        }
                    }
                }
            }
        }

        // A pattern that captures a definition but no name is unusable —
        // an anonymous symbol cannot be searched for, referenced, or
        // edited by path — so it is dropped rather than recorded nameless.
        if let (Some((node, kind)), Some(name_node)) = (def, name_node) {
            raw.push(RawDefinition {
                node,
                name_node,
                kind,
                explicit_containers: containers,
                doc,
            });
        }
    }

    // One definition can match several patterns: a Rust `fn` inside an
    // `impl` matches both the bare `function_item` pattern and the
    // `impl_item` one that knows the containing type; a Go
    // `type_declaration` matches both the specific `struct_type` pattern
    // and the catch-all type-alias pattern; a decorated Python method
    // matches once as the `decorated_definition` and once as the
    // `function_definition` inside it. Keeping all of them would emit
    // several symbols for one definition.
    //
    // The *name* node is the identity that survives all three cases (the
    // definition node differs for the Python one), so matches collapse by
    // name node, keeping the richest: most container information, then
    // most specific kind, then the widest span — the widest span is what
    // picks `decorated_definition` over the bare function, so a symbol-
    // aware edit replaces the decorators along with the body.
    raw.sort_by_key(|d| {
        (
            d.name_node.id(),
            std::cmp::Reverse(d.explicit_containers.len()),
            std::cmp::Reverse(kind_specificity(d.kind)),
            std::cmp::Reverse(d.node.end_byte() - d.node.start_byte()),
        )
    });
    raw.dedup_by_key(|d| d.name_node.id());

    // Outermost-first at equal start: purely for deterministic output
    // ordering, which the index's drift check compares against.
    raw.sort_by_key(|d| (d.node.start_byte(), std::cmp::Reverse(d.node.end_byte())));

    let container_spans: Vec<(Span, String)> = raw
        .iter()
        .filter(|d| d.kind.is_container())
        .map(|d| (span_of(d.node), text_of(d.name_node, bytes).to_string()))
        .collect();

    raw.iter()
        .map(|d| {
            let span = span_of(d.node);
            let name = text_of(d.name_node, bytes).to_string();

            let mut path_parts: Vec<String> = container_spans
                .iter()
                .filter(|(cspan, _)| cspan.strictly_contains(&span))
                .map(|(cspan, cname)| (cspan.len_bytes(), cname.clone()))
                // Outermost container has the largest span, so descending
                // length gives outermost-to-innermost ordering without
                // needing a real tree walk.
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .rev()
                .map(|(_, cname)| cname)
                .collect();
            path_parts.extend(d.explicit_containers.iter().cloned());
            path_parts.push(name.clone());

            Symbol {
                name,
                kind: d.kind,
                symbol_path: path_parts.join(provider.path_separator()),
                span,
                name_span: span_of(d.name_node),
                signature: signature_of(provider, d.node, source),
                doc: d
                    .doc
                    .clone()
                    .or_else(|| doc_comment_above(provider, d.node, bytes)),
            }
        })
        .collect()
}

fn extract_imports(lang: &CompiledLanguage, root: Node<'_>, bytes: &[u8]) -> Vec<Import> {
    let Some(query) = lang.imports.as_ref() else {
        return Vec::new();
    };
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut matches = cursor.matches(query, root, bytes);
    while let Some(m) = matches.next() {
        let mut stmt: Option<Node<'_>> = None;
        let mut path: Option<Node<'_>> = None;
        for capture in m.captures {
            match capture_names[capture.index as usize] {
                "import" => stmt = Some(capture.node),
                "import.path" => path = Some(capture.node),
                _ => {}
            }
        }
        if let Some(path_node) = path {
            let raw_path = strip_quotes(text_of(path_node, bytes)).to_string();
            if raw_path.is_empty() {
                continue;
            }
            out.push(Import {
                raw_path,
                span: span_of(stmt.unwrap_or(path_node)),
            });
        }
    }
    out.sort_by_key(|i| (i.span.start_byte, i.raw_path.clone()));
    out.dedup();
    out
}

fn extract_calls(
    lang: &CompiledLanguage,
    root: Node<'_>,
    bytes: &[u8],
    symbols: &[Symbol],
) -> Vec<Call> {
    let Some(query) = lang.calls.as_ref() else {
        return Vec::new();
    };
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut matches = cursor.matches(query, root, bytes);
    while let Some(m) = matches.next() {
        let mut site: Option<Node<'_>> = None;
        let mut name: Option<Node<'_>> = None;
        for capture in m.captures {
            match capture_names[capture.index as usize] {
                "reference.call" => site = Some(capture.node),
                "name" => name = Some(capture.node),
                _ => {}
            }
        }
        if let Some(name_node) = name {
            let span = span_of(site.unwrap_or(name_node));
            out.push(Call {
                name: text_of(name_node, bytes).to_string(),
                span,
                enclosing_symbol_path: enclosing_callable(symbols, &span),
            });
        }
    }
    out.sort_by_key(|c| (c.span.start_byte, c.name.clone()));
    out.dedup();
    out
}

fn extract_tests(
    lang: &CompiledLanguage,
    root: Node<'_>,
    bytes: &[u8],
    symbols: &[Symbol],
) -> Vec<TestCase> {
    let Some(query) = lang.tests.as_ref() else {
        return Vec::new();
    };
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();

    let mut matches = cursor.matches(query, root, bytes);
    while let Some(m) = matches.next() {
        let mut node: Option<Node<'_>> = None;
        let mut name: Option<Node<'_>> = None;
        for capture in m.captures {
            match capture_names[capture.index as usize] {
                "test" => node = Some(capture.node),
                "test.name" => name = Some(capture.node),
                _ => {}
            }
        }
        if let Some(name_node) = name {
            let span = span_of(node.unwrap_or(name_node));
            let name = text_of(name_node, bytes).to_string();
            // Prefer the symbol path the symbol extractor already computed
            // for the same span, so a test's identity matches its symbol's
            // and the graph can join the two without guessing.
            let symbol_path = symbols
                .iter()
                .find(|s| s.span == span || s.name_span == span_of(name_node))
                .map(|s| s.symbol_path.clone())
                .unwrap_or_else(|| name.clone());
            out.push(TestCase {
                name,
                symbol_path,
                span,
            });
        }
    }
    out.sort_by_key(|t| (t.span.start_byte, t.name.clone()));
    out.dedup();
    out
}

/// The innermost function-like symbol containing `span` — the "who is
/// making this call" half of a `Calls` edge.
fn enclosing_callable(symbols: &[Symbol], span: &Span) -> Option<String> {
    symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Test
            ) && s.span.contains(span)
        })
        .min_by_key(|s| s.span.len_bytes())
        .map(|s| s.symbol_path.clone())
}

/// The definition's text up to the start of its body: `fn foo(a: u32) -> bool`.
/// Falls back to the first line when no body child is recognized, which is
/// the right answer for bodiless declarations (a trait method signature,
/// an interface member, a constant).
fn signature_of(provider: &dyn LanguageProvider, node: Node<'_>, source: &str) -> String {
    // Two levels deep, not one: a decorated Python method's definition
    // node is the `decorated_definition`, whose body lives one level down
    // inside the `function_definition`. Taking the earliest body node
    // found keeps a container's own body (a Java `class_body`) winning
    // over its members' bodies.
    let body_start = first_body_start(provider, node, 2);

    let end = body_start.unwrap_or_else(|| {
        let start = node.start_byte();
        source[start..node.end_byte().min(source.len())]
            .find('\n')
            .map(|offset| start + offset)
            .unwrap_or(node.end_byte())
    });

    let start = node.start_byte().min(source.len());
    let end = end.clamp(start, source.len());
    source[start..end].trim_end().to_string()
}

fn first_body_start(
    provider: &dyn LanguageProvider,
    node: Node<'_>,
    depth: usize,
) -> Option<usize> {
    if depth == 0 {
        return None;
    }
    let body_kinds = provider.body_node_kinds();
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|child| {
            if body_kinds.contains(&child.kind()) {
                Some(child.start_byte())
            } else {
                first_body_start(provider, child, depth - 1)
            }
        })
        .min()
}

/// The contiguous comment block immediately above a definition, with no
/// blank line between it and the definition. Blank-line separation is what
/// distinguishes "this comment documents this item" from "this comment is
/// about something else that happens to be above it".
fn doc_comment_above(
    provider: &dyn LanguageProvider,
    node: Node<'_>,
    bytes: &[u8],
) -> Option<String> {
    let comment_kinds = provider.comment_node_kinds();
    let mut lines: Vec<String> = Vec::new();
    let mut current = node;

    while let Some(prev) = current.prev_named_sibling() {
        if !comment_kinds.contains(&prev.kind()) {
            break;
        }
        if !adjacent(bytes, prev.end_byte(), current.start_byte()) {
            break;
        }
        lines.push(text_of(prev, bytes).trim_end().to_string());
        current = prev;
    }

    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

/// Whether two nodes are separated by nothing but a single line break.
///
/// Deliberately measured in bytes rather than rows: grammars disagree
/// about whether a line comment's node includes its trailing newline
/// (Rust's does, Java's does not), so comparing `end_position().row` to
/// `start_position().row` is off by one for exactly one of them. The gap
/// text tells the truth for both.
fn adjacent(bytes: &[u8], from: usize, to: usize) -> bool {
    if to < from || to > bytes.len() {
        return false;
    }
    let gap = &bytes[from..to];
    gap.iter().all(|b| b.is_ascii_whitespace()) && gap.iter().filter(|b| **b == b'\n').count() <= 1
}

fn text_of<'a>(node: Node<'_>, bytes: &'a [u8]) -> &'a str {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte().min(bytes.len())]).unwrap_or("")
}

/// Import paths arrive as string literals in most languages; the quotes
/// are syntax, not part of the path.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = s.strip_prefix(quote).and_then(|r| r.strip_suffix(quote)) {
            return inner;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_each_quote_style_but_leaves_bare_paths_alone() {
        assert_eq!(strip_quotes("\"./util\""), "./util");
        assert_eq!(strip_quotes("'os.path'"), "os.path");
        assert_eq!(strip_quotes("`x`"), "x");
        assert_eq!(strip_quotes("std::collections"), "std::collections");
    }

    #[test]
    fn unbalanced_quotes_are_left_untouched() {
        assert_eq!(strip_quotes("\"unterminated"), "\"unterminated");
    }
}
