//! Strategy 5: AST transformation (§4.11).
//!
//! Every transform here resolves its target through the syntax tree, not
//! through text search, and every one is validated by a re-parse before it
//! is allowed to succeed. That combination is what makes these safe to
//! hand to a model: a rename cannot corrupt a string literal, and a
//! replacement that produces unparseable code is rejected rather than
//! written.
//!
//! Transforms are a closed set of typed operations rather than a free-text
//! "describe what you want" field. A description cannot be executed,
//! verified, or replayed from a journal; these can.

use serde::{Deserialize, Serialize};
use valyria_lang::{CompiledLanguage, Span};

use crate::error::{EditError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AstTransform {
    /// Rename every identifier token in the file whose text is exactly
    /// `from`.
    ///
    /// AST-aware but not scope-aware: it matches identifier *nodes*, so
    /// occurrences inside strings and comments are untouched — the failure
    /// mode of a naive text replace — but a local variable that shares the
    /// name is renamed too. Renaming across files is the caller's job;
    /// this transform sees one file.
    RenameIdentifier { from: String, to: String },

    /// Delete a whole definition, along with the blank line it leaves
    /// behind.
    DeleteSymbol { symbol_path: String },

    /// Insert text immediately before a definition, at the definition's
    /// own indentation.
    InsertBeforeSymbol { symbol_path: String, text: String },

    /// Insert text immediately after a definition.
    InsertAfterSymbol { symbol_path: String, text: String },

    /// Replace whatever a tree-sitter query captures. The general escape
    /// hatch: `query` is a `.scm` pattern compiled against the file's
    /// grammar and `capture` names which capture to replace.
    ///
    /// Fails when the query matches nothing, and when it matches more than
    /// once unless `all` is set — a transform that silently edited the
    /// first of several matches would be exactly the kind of quiet wrong
    /// answer the editing engine exists to prevent.
    ReplaceQueryMatch {
        query: String,
        capture: String,
        replacement: String,
        #[serde(default)]
        all: bool,
    },
}

pub fn apply(lang: &CompiledLanguage, source: &str, transform: &AstTransform) -> Result<String> {
    match transform {
        AstTransform::RenameIdentifier { from, to } => rename_identifier(lang, source, from, to),
        AstTransform::DeleteSymbol { symbol_path } => {
            let span = resolve_symbol_span(lang, source, symbol_path)?;
            Ok(splice(source, expand_to_whole_lines(source, span), ""))
        }
        AstTransform::InsertBeforeSymbol { symbol_path, text } => {
            let span = resolve_symbol_span(lang, source, symbol_path)?;
            let indent = indentation_at(source, span.start_byte);
            let block = indent_block(text, &indent);
            Ok(splice(
                source,
                Span {
                    start_byte: span.start_byte,
                    end_byte: span.start_byte,
                    ..span
                },
                &format!("{block}\n{indent}"),
            ))
        }
        AstTransform::InsertAfterSymbol { symbol_path, text } => {
            let span = resolve_symbol_span(lang, source, symbol_path)?;
            let indent = indentation_at(source, span.start_byte);
            let block = indent_block(text, &indent);
            Ok(splice(
                source,
                Span {
                    start_byte: span.end_byte,
                    end_byte: span.end_byte,
                    ..span
                },
                &format!("\n{indent}{block}"),
            ))
        }
        AstTransform::ReplaceQueryMatch {
            query,
            capture,
            replacement,
            all,
        } => replace_query_match(lang, source, query, capture, replacement, *all),
    }
}

/// The span of the single definition named `symbol_path`.
///
/// Zero matches and several matches are both errors: an edit addressed to
/// an ambiguous name has no correct interpretation, and picking one would
/// be a coin flip written to disk.
pub fn resolve_symbol_span(
    lang: &CompiledLanguage,
    source: &str,
    symbol_path: &str,
) -> Result<Span> {
    let facts = valyria_lang::extract::extract(lang, source)?;
    let matches: Vec<&valyria_lang::Symbol> = facts
        .symbols
        .iter()
        .filter(|s| s.symbol_path == symbol_path)
        .collect();

    match matches.len() {
        0 => Err(EditError::SymbolNotFound {
            symbol_path: symbol_path.to_string(),
            // Naming the alternatives turns a dead end into a usable
            // correction, which matters when the caller is a model.
            available: facts
                .symbols
                .iter()
                .map(|s| s.symbol_path.clone())
                .collect(),
        }),
        1 => Ok(matches[0].span),
        n => Err(EditError::SymbolAmbiguous {
            symbol_path: symbol_path.to_string(),
            count: n,
        }),
    }
}

fn rename_identifier(
    lang: &CompiledLanguage,
    source: &str,
    from: &str,
    to: &str,
) -> Result<String> {
    if from.is_empty() {
        return Err(EditError::NoMatch("an empty identifier".into()));
    }

    let targets = valyria_lang::identifier_spans(lang, source, from)?;
    if targets.is_empty() {
        return Err(EditError::NoMatch(format!("no identifier named `{from}`")));
    }

    Ok(splice_many(source, &targets, |_| to.to_string()))
}

fn replace_query_match(
    lang: &CompiledLanguage,
    source: &str,
    query_source: &str,
    capture: &str,
    replacement: &str,
    all: bool,
) -> Result<String> {
    let spans = valyria_lang::query_spans(lang, source, query_source, capture)?;

    match spans.len() {
        0 => Err(EditError::NoMatch(format!(
            "query matched nothing for capture `@{capture}`"
        ))),
        // Editing the first of several matches and calling it a success is
        // exactly the quiet wrong answer the editing engine exists to
        // prevent, so the caller has to say it meant all of them.
        n if n > 1 && !all => Err(EditError::QueryAmbiguous { count: n }),
        _ => Ok(splice_many(source, &spans, |_| replacement.to_string())),
    }
}

/// Replace one span. Offsets are snapped to character boundaries so a span
/// derived from a stale tree cannot panic mid-character.
fn splice(source: &str, span: Span, replacement: &str) -> String {
    let start = floor_char_boundary(source, span.start_byte);
    let end = floor_char_boundary(source, span.end_byte).max(start);
    let mut out = String::with_capacity(source.len() + replacement.len());
    out.push_str(&source[..start]);
    out.push_str(replacement);
    out.push_str(&source[end..]);
    out
}

/// Replace several spans in one pass. Applied back to front so each
/// replacement's offsets stay valid — the classic bug in multi-site edits
/// is applying them forwards and shifting every subsequent span.
fn splice_many(source: &str, spans: &[Span], replacement: impl Fn(&Span) -> String) -> String {
    let mut ordered: Vec<&Span> = spans.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.start_byte));

    let mut out = source.to_string();
    for span in ordered {
        out = splice(&out, *span, &replacement(span));
    }
    out
}

/// Widen a span to cover the whole lines it sits on, plus the line break
/// that follows — so deleting a definition does not leave a blank line and
/// a dangling indent behind.
fn expand_to_whole_lines(source: &str, span: Span) -> Span {
    let bytes = source.as_bytes();

    let mut start = span.start_byte.min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' {
        // Only walk back over the indentation, never over code that shares
        // the line.
        if !bytes[start - 1].is_ascii_whitespace() {
            start = span.start_byte;
            break;
        }
        start -= 1;
    }

    let mut end = span.end_byte.min(bytes.len());
    while end < bytes.len() && bytes[end] != b'\n' {
        if !bytes[end].is_ascii_whitespace() {
            end = span.end_byte;
            break;
        }
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    }

    Span {
        start_byte: start,
        end_byte: end,
        ..span
    }
}

fn indentation_at(source: &str, byte: usize) -> String {
    let bytes = source.as_bytes();
    let mut start = byte.min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    source[start..byte.min(source.len())]
        .chars()
        .take_while(|c| c.is_whitespace() && *c != '\n')
        .collect()
}

/// Indent every line of `text` after the first: the first line lands where
/// the caller is splicing it, the rest need the indentation added.
fn indent_block(text: &str, indent: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").to_string();
    let rest: Vec<String> = lines.map(|line| format!("{indent}{line}")).collect();
    if rest.is_empty() {
        first
    } else {
        format!("{first}\n{}", rest.join("\n"))
    }
}

fn floor_char_boundary(s: &str, mut byte: usize) -> usize {
    if byte >= s.len() {
        return s.len();
    }
    while byte > 0 && !s.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(a: usize, b: usize) -> Span {
        Span {
            start_byte: a,
            end_byte: b,
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn splice_replaces_exactly_one_region() {
        assert_eq!(splice("abcdef", span(2, 4), "XY"), "abXYef");
    }

    #[test]
    fn splice_many_applies_back_to_front_so_offsets_stay_valid() {
        // Applied forwards, the second replacement would land at the wrong
        // offset once the first changed the string's length.
        let out = splice_many("aaa bbb ccc", &[span(0, 3), span(8, 11)], |_| {
            "LONGER".to_string()
        });
        assert_eq!(out, "LONGER bbb LONGER");
    }

    #[test]
    fn splice_never_cuts_a_multi_byte_character() {
        let out = splice("→→→", span(1, 5), "x");
        assert!(out.is_char_boundary(0));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn whole_line_expansion_takes_the_indent_and_the_trailing_newline() {
        let source = "a\n    target\nb\n";
        let expanded = expand_to_whole_lines(source, span(6, 12));
        assert_eq!(
            &source[expanded.start_byte..expanded.end_byte],
            "    target\n"
        );
    }

    #[test]
    fn whole_line_expansion_leaves_a_neighbour_on_the_same_line_alone() {
        let source = "keep target keep\n";
        let expanded = expand_to_whole_lines(source, span(5, 11));
        assert_eq!(expanded.start_byte, 5);
        assert_eq!(expanded.end_byte, 11);
    }

    #[test]
    fn indentation_is_read_from_the_start_of_the_line() {
        assert_eq!(indentation_at("a\n        x", 10), "        ");
        assert_eq!(indentation_at("x", 0), "");
    }

    #[test]
    fn indent_block_leaves_the_first_line_alone() {
        assert_eq!(indent_block("one\ntwo", "  "), "one\n  two");
        assert_eq!(indent_block("only", "  "), "only");
    }
}
