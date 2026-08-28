//! Syntax-aware chunking (§4.13: "a `chunker` for embeddings that
//! respects syntactic boundaries").
//!
//! A fixed-window chunker cuts through the middle of a function, and the
//! resulting embedding describes neither half. This one cuts at
//! definition boundaries wherever it can, falls back to line boundaries
//! for a definition that exceeds the budget on its own, and labels every
//! chunk with the symbol it came from so a retrieval hit can say *which
//! function* matched rather than "bytes 4000-6000 of this file".

use crate::symbol::{FileFacts, Span, Symbol};

/// Chunk size budget in bytes. Not tokens: this crate deliberately knows
/// nothing about tokenizers (that is `valyria-model`'s concern), and bytes
/// are a stable proxy for splitting decisions.
pub const DEFAULT_MAX_CHUNK_BYTES: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub span: Span,
    /// The definition this chunk came from, when it came from one. `None`
    /// for the gaps between definitions — a file header, an import block,
    /// module-level statements.
    pub symbol_path: Option<String>,
    pub text: String,
}

/// Split `source` into chunks no larger than `max_bytes`, cutting at
/// definition boundaries where possible.
pub fn chunk_file(facts: &FileFacts, source: &str, max_bytes: usize) -> Vec<Chunk> {
    let max_bytes = max_bytes.max(1);

    // Only outermost definitions: a class and its methods would otherwise
    // produce overlapping chunks covering the same bytes twice.
    let mut top_level: Vec<&Symbol> = facts
        .symbols
        .iter()
        .filter(|s| {
            !facts
                .symbols
                .iter()
                .any(|other| other.span.strictly_contains(&s.span))
        })
        .collect();
    top_level.sort_by_key(|s| s.span.start_byte);

    let mut chunks = Vec::new();
    let mut cursor = 0usize;

    for symbol in top_level {
        if symbol.span.start_byte < cursor {
            continue; // overlapping definitions: keep the first, skip the rest
        }
        if symbol.span.start_byte > cursor {
            push_split(
                &mut chunks,
                source,
                cursor,
                symbol.span.start_byte,
                None,
                max_bytes,
            );
        }
        push_split(
            &mut chunks,
            source,
            symbol.span.start_byte,
            symbol.span.end_byte.min(source.len()),
            Some(symbol.symbol_path.clone()),
            max_bytes,
        );
        cursor = symbol.span.end_byte.min(source.len());
    }

    if cursor < source.len() {
        push_split(&mut chunks, source, cursor, source.len(), None, max_bytes);
    }

    chunks
}

/// Emit `source[start..end]` as one chunk, or as several split at line
/// boundaries when it exceeds the budget.
fn push_split(
    out: &mut Vec<Chunk>,
    source: &str,
    start: usize,
    end: usize,
    symbol_path: Option<String>,
    max_bytes: usize,
) {
    let start = floor_char_boundary(source, start);
    let end = floor_char_boundary(source, end).max(start);
    if source[start..end].trim().is_empty() {
        return;
    }

    let mut segment_start = start;
    while segment_start < end {
        let remaining = end - segment_start;
        if remaining <= max_bytes {
            out.push(make_chunk(source, segment_start, end, symbol_path.clone()));
            return;
        }

        let hard_limit = floor_char_boundary(source, segment_start + max_bytes);
        // Prefer the last newline inside the budget; a segment with no
        // newline at all (a minified bundle, a long data literal) falls
        // back to the hard limit rather than growing without bound.
        let cut = source[segment_start..hard_limit]
            .rfind('\n')
            .map(|offset| segment_start + offset + 1)
            .filter(|cut| *cut > segment_start)
            .unwrap_or(hard_limit);

        out.push(make_chunk(source, segment_start, cut, symbol_path.clone()));
        segment_start = cut;
    }
}

fn make_chunk(source: &str, start: usize, end: usize, symbol_path: Option<String>) -> Chunk {
    let text = source[start..end].to_string();
    Chunk {
        span: Span {
            start_byte: start,
            end_byte: end,
            start_line: line_of(source, start),
            end_line: line_of(source, end.saturating_sub(1).max(start)),
        },
        symbol_path,
        text,
    }
}

/// Counts over `as_bytes()` rather than slicing `source`: `byte` can land
/// mid-character (it is derived from a chunk end minus one), and slicing
/// a `str` there panics while counting bytes does not.
fn line_of(source: &str, byte: usize) -> u32 {
    let byte = byte.min(source.len());
    source.as_bytes()[..byte]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
        + 1
}

/// `str::floor_char_boundary` is still unstable; splitting a multi-byte
/// character would panic on the slice, so every offset is snapped down to
/// a boundary first.
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
    use crate::symbol::SymbolKind;

    fn symbol(path: &str, start: usize, end: usize) -> Symbol {
        Symbol {
            name: path.to_string(),
            kind: SymbolKind::Function,
            symbol_path: path.to_string(),
            span: Span {
                start_byte: start,
                end_byte: end,
                start_line: 1,
                end_line: 1,
            },
            name_span: Span {
                start_byte: start,
                end_byte: start,
                start_line: 1,
                end_line: 1,
            },
            signature: String::new(),
            doc: None,
        }
    }

    #[test]
    fn each_definition_becomes_its_own_chunk() {
        let source = "fn a() { 1 }\nfn b() { 2 }\n";
        let facts = FileFacts {
            symbols: vec![symbol("a", 0, 12), symbol("b", 13, 25)],
            ..Default::default()
        };

        let chunks = chunk_file(&facts, source, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].symbol_path.as_deref(), Some("a"));
        assert_eq!(chunks[0].text, "fn a() { 1 }");
        assert_eq!(chunks[1].symbol_path.as_deref(), Some("b"));
    }

    #[test]
    fn the_gap_before_the_first_definition_is_its_own_unlabeled_chunk() {
        let source = "use std::fmt;\n\nfn a() { 1 }\n";
        let facts = FileFacts {
            symbols: vec![symbol("a", 15, 27)],
            ..Default::default()
        };

        let chunks = chunk_file(&facts, source, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].symbol_path, None);
        assert!(chunks[0].text.contains("use std::fmt;"));
    }

    #[test]
    fn an_oversized_definition_is_split_at_line_boundaries() {
        let body: String = (0..50).map(|i| format!("    line {i};\n")).collect();
        let source = format!("fn big() {{\n{body}}}\n");
        let facts = FileFacts {
            symbols: vec![symbol("big", 0, source.len())],
            ..Default::default()
        };

        let chunks = chunk_file(&facts, &source, 200);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.span.len_bytes() <= 200));
        // Every chunk still carries the symbol it belongs to, so a hit
        // anywhere in the body is attributable to `big`.
        assert!(chunks
            .iter()
            .all(|c| c.symbol_path.as_deref() == Some("big")));
        // Splitting is lossless: concatenating the chunks reproduces the
        // definition exactly.
        let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rejoined, source);
    }

    #[test]
    fn nested_definitions_do_not_produce_overlapping_chunks() {
        let source = "class A {\n  fn m() {}\n}\n";
        let mut class = symbol("A", 0, 23);
        class.kind = SymbolKind::Class;
        let facts = FileFacts {
            symbols: vec![class, symbol("A.m", 12, 21)],
            ..Default::default()
        };

        let chunks = chunk_file(&facts, source, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_path.as_deref(), Some("A"));
    }

    #[test]
    fn a_file_with_no_symbols_is_still_chunked() {
        let source = "# just a config file\nkey = value\n";
        let chunks = chunk_file(&FileFacts::default(), source, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_path, None);
        assert_eq!(chunks[0].text, source);
    }

    #[test]
    fn whitespace_only_regions_are_dropped() {
        let source = "fn a() {}\n\n\n\n";
        let facts = FileFacts {
            symbols: vec![symbol("a", 0, 9)],
            ..Default::default()
        };
        let chunks = chunk_file(&facts, source, DEFAULT_MAX_CHUNK_BYTES);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn splitting_never_cuts_a_multi_byte_character() {
        // Each `→` is three bytes; a naive byte split at max_bytes would
        // land mid-character and panic on the slice.
        let source = "→→→→→→→→→→→→→→→→→→→→";
        let chunks = chunk_file(&FileFacts::default(), source, 8);
        assert!(chunks.len() > 1);
        let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(rejoined, source);
    }

    #[test]
    fn a_line_longer_than_the_budget_still_terminates() {
        let source = "x".repeat(1000);
        let chunks = chunk_file(&FileFacts::default(), &source, 100);
        assert_eq!(chunks.len(), 10);
    }
}
