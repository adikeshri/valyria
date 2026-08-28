//! The retrieval modes (§4.16). Each takes a [`ModeCtx`] and returns a
//! ranked, file-deduplicated list of [`ModeHit`]s; [`crate::fusion`]
//! combines them.
//!
//! Modes never fail the whole search by being unavailable. A mode with
//! nothing to contribute — no embeddings, not a git repo, no anchors —
//! returns [`ModeOutcome::degraded`] with a reason, and search proceeds
//! on the modes that do work. The one exception is a caller-supplied
//! pattern that does not compile: `regex` and `ast` are only ever run
//! when explicitly asked for, so a broken pattern there is a real error.

pub mod ast;
pub mod dependency;
pub mod git;
pub mod lexical;
pub mod regex;
pub mod semantic;
pub mod symbol;

use std::path::Path;

use valyria_embed::{EmbedStore, Embedder};
use valyria_graph::GraphStore;
use valyria_index::IndexStore;
use valyria_lang::LanguageRegistry;
use valyria_types::Generation;

use crate::query::SearchQuery;

/// A file read once by the engine and shared by every text-scanning mode
/// (`lexical`, `regex`, `ast`), so the tree is walked at most once per
/// search.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    pub path: String,
    pub text: String,
    pub language: Option<String>,
}

/// Everything a mode may need. Assembled once per search.
pub struct ModeCtx<'a> {
    pub generation: Generation,
    pub root: &'a Path,
    pub index: &'a IndexStore,
    pub graph: &'a GraphStore,
    pub embed: &'a EmbedStore,
    pub embedder: &'a dyn Embedder,
    pub registry: &'a LanguageRegistry,
    pub repo: Option<&'a valyria_git::Repo>,
    /// Populated only if a text-scanning mode is active.
    pub files: &'a [LoadedFile],
    pub query: &'a SearchQuery,
}

impl std::fmt::Debug for ModeCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeCtx")
            .field("generation", &self.generation)
            .field("root", &self.root)
            .field("files", &self.files.len())
            .field("has_repo", &self.repo.is_some())
            .field("query", &self.query.text)
            .finish()
    }
}

/// One location a mode matched.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeHit {
    pub path: String,
    pub symbol_path: Option<String>,
    pub line: Option<u32>,
    pub snippet: Option<String>,
    /// The mode's own score in its own units. Only the *rank* derived
    /// from it feeds fusion, but it is carried through for the
    /// explanation.
    pub raw_score: f64,
}

/// What a mode produced, plus an optional note explaining why it
/// contributed nothing.
#[derive(Debug, Clone, Default)]
pub struct ModeOutcome {
    pub hits: Vec<ModeHit>,
    pub degraded: Option<String>,
}

impl ModeOutcome {
    fn empty() -> Self {
        Self::default()
    }

    fn degraded(reason: impl Into<String>) -> Self {
        Self {
            hits: Vec::new(),
            degraded: Some(reason.into()),
        }
    }

    /// Collapse to one hit per file (the highest-scoring, tie-broken by
    /// earliest line) and sort descending, so a mode's output is already
    /// a clean ranked list of files by the time fusion sees it.
    fn into_ranked_files(mut self) -> Self {
        use std::collections::HashMap;
        let mut best: HashMap<String, ModeHit> = HashMap::new();
        for hit in self.hits.drain(..) {
            best.entry(hit.path.clone())
                .and_modify(|existing| {
                    let better = hit.raw_score > existing.raw_score
                        || (hit.raw_score == existing.raw_score
                            && hit.line.unwrap_or(u32::MAX) < existing.line.unwrap_or(u32::MAX));
                    if better {
                        *existing = hit.clone();
                    }
                })
                .or_insert(hit);
        }
        let mut hits: Vec<ModeHit> = best.into_values().collect();
        hits.sort_by(|a, b| {
            b.raw_score
                .partial_cmp(&a.raw_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.path.cmp(&b.path))
        });
        self.hits = hits;
        self
    }
}

/// Read the indexed, non-binary files of `generation` into memory once.
pub(crate) async fn load_files(
    root: &Path,
    index: &IndexStore,
    generation: Generation,
) -> crate::Result<Vec<LoadedFile>> {
    use rayon::prelude::*;

    let records = index.files(generation).await?;
    // rayon's own pool does the file reads; this is the same "CPU work
    // off a parallel iterator" shape the index scanner uses.
    let files = records
        .par_iter()
        .filter(|r| !r.is_binary)
        .filter_map(|r| {
            let text = std::fs::read_to_string(root.join(&r.path)).ok()?;
            Some(LoadedFile {
                path: r.path.clone(),
                text,
                language: r.language.clone(),
            })
        })
        .collect();
    Ok(files)
}

/// Case-insensitive check for `needle` as a whole word inside `haystack`
/// (both already lowercased by the caller).
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack.as_bytes()[start - 1].is_ascii_alphanumeric()
                && haystack.as_bytes()[start - 1] != b'_';
        let after_ok = end == haystack.len()
            || !haystack.as_bytes()[end].is_ascii_alphanumeric()
                && haystack.as_bytes()[end] != b'_';
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.len().max(1);
    }
    false
}

/// A short single-line excerpt for display.
fn snippet_of(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.len() <= 200 {
        trimmed.to_string()
    } else {
        let mut end = 200;
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_files_keeps_one_hit_per_file_the_best_one() {
        let outcome = ModeOutcome {
            hits: vec![
                ModeHit {
                    path: "a.rs".into(),
                    symbol_path: None,
                    line: Some(10),
                    snippet: None,
                    raw_score: 1.0,
                },
                ModeHit {
                    path: "a.rs".into(),
                    symbol_path: None,
                    line: Some(3),
                    snippet: None,
                    raw_score: 5.0,
                },
                ModeHit {
                    path: "b.rs".into(),
                    symbol_path: None,
                    line: Some(1),
                    snippet: None,
                    raw_score: 2.0,
                },
            ],
            degraded: None,
        }
        .into_ranked_files();

        assert_eq!(outcome.hits.len(), 2);
        assert_eq!(outcome.hits[0].path, "a.rs");
        assert_eq!(outcome.hits[0].raw_score, 5.0);
        assert_eq!(outcome.hits[0].line, Some(3));
        assert_eq!(outcome.hits[1].path, "b.rs");
    }

    #[test]
    fn contains_word_respects_identifier_boundaries() {
        assert!(contains_word("let parser = make()", "parser"));
        assert!(!contains_word("let parser_state = 1", "parser"));
        assert!(!contains_word("reparse()", "parse"));
        assert!(contains_word("fn parse() {}", "parse"));
    }
}
