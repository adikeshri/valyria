//! Turning a file into the units that get embedded.
//!
//! The split itself is `valyria-lang`'s syntax-aware [`chunk_file`] —
//! cutting at definition boundaries so an embedding describes a whole
//! function rather than an arbitrary byte window. This module adds the
//! two things the store needs on top: the file each chunk came from, and
//! a content hash *of the chunk text*, which is the invalidation key. A
//! rebuild re-embeds a chunk only when its own bytes changed, not when
//! anything else in the file did (§4.15).

use valyria_lang::{chunk_file, FileFacts, Span, DEFAULT_MAX_CHUNK_BYTES};
use valyria_util::ContentHash;

/// One embeddable unit: a span of one file, the definition it belongs to
/// (when it belongs to one), and the hash that decides whether it needs
/// re-embedding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedChunk {
    pub path: String,
    pub symbol_path: Option<String>,
    pub span: Span,
    /// `blake3` of [`Self::text`]. Two chunks with the same hash have the
    /// same bytes and therefore the same vector, regardless of which file
    /// or generation they came from.
    pub chunk_hash: ContentHash,
    pub text: String,
}

/// Chunk one file's `source`, given the [`FileFacts`] extracted from it.
///
/// `facts` may be [`FileFacts::default`] for a file with no grammar (a
/// Markdown doc, a config file); the chunker then falls back to
/// line-boundary splitting and every chunk is unlabelled. That is
/// intentional — prose and config are worth retrieving semantically too.
pub fn chunk_source(
    path: &str,
    source: &str,
    facts: &FileFacts,
    max_bytes: usize,
) -> Vec<EmbedChunk> {
    chunk_file(facts, source, max_bytes)
        .into_iter()
        .filter(|c| !c.text.trim().is_empty())
        .map(|c| EmbedChunk {
            path: path.to_string(),
            symbol_path: c.symbol_path,
            span: c.span,
            chunk_hash: ContentHash::of_bytes(c.text.as_bytes()),
            text: c.text,
        })
        .collect()
}

/// The chunk-size budget the pipeline uses unless told otherwise — the
/// same one `valyria-lang` picked for embeddings.
pub const DEFAULT_CHUNK_BYTES: usize = DEFAULT_MAX_CHUNK_BYTES;

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_lang::{Symbol, SymbolKind};

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
    fn one_chunk_per_definition_each_carrying_its_symbol_path() {
        let source = "fn a() { 1 }\nfn b() { 2 }\n";
        let facts = FileFacts {
            symbols: vec![symbol("a", 0, 12), symbol("b", 13, 25)],
            ..Default::default()
        };
        let chunks = chunk_source("src/x.rs", source, &facts, DEFAULT_CHUNK_BYTES);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].symbol_path.as_deref(), Some("a"));
        assert_eq!(chunks[0].path, "src/x.rs");
    }

    #[test]
    fn identical_chunk_text_hashes_identically_across_files() {
        let body = "fn shared() { do_thing() }";
        let a = chunk_source("a.rs", body, &FileFacts::default(), DEFAULT_CHUNK_BYTES);
        let b = chunk_source("b.rs", body, &FileFacts::default(), DEFAULT_CHUNK_BYTES);
        assert_eq!(a[0].chunk_hash, b[0].chunk_hash);
    }

    #[test]
    fn a_file_with_no_facts_is_still_chunked() {
        let chunks = chunk_source(
            "notes.md",
            "# Title\n\nsome prose about the design\n",
            &FileFacts::default(),
            DEFAULT_CHUNK_BYTES,
        );
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].symbol_path.is_none());
    }

    #[test]
    fn whitespace_only_chunks_are_dropped() {
        let chunks = chunk_source(
            "x.rs",
            "\n\n\n   \n",
            &FileFacts::default(),
            DEFAULT_CHUNK_BYTES,
        );
        assert!(chunks.is_empty());
    }
}
