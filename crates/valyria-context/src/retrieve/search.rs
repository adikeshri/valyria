//! [`SearchRetriever`]: `valyria-search` hits → context candidates.
//!
//! Each ranked hit becomes one candidate. When the hit is in a file the
//! index has symbols for, the candidate is a
//! [`CandidateContent::Source`](crate::candidate::CandidateContent::Source)
//! carrying every symbol's exact body — so the compressor can shed the
//! file symbol by symbol under budget pressure without ever cutting one.
//! Otherwise it is plain [`CandidateContent::Text`] of the file.
//!
//! The `Provenance` on every candidate is the hit's own
//! (`retrieval_path` + `score`), so `context.explain` (§14) can render
//! "why this file" straight from what search recorded.

use async_trait::async_trait;

use valyria_index::IndexStore;
use valyria_search::{SearchEngine, SearchMode, SearchQuery};
use valyria_types::{Generation, Trust};

use crate::budget::SectionKind;
use crate::candidate::{CandidateContent, RetrievalCandidate, SymbolSpan};
use crate::error::{ContextError, Result};
use crate::retrieve::{RetrievalQuery, Retriever};

/// Wraps a [`SearchEngine`] and the [`IndexStore`] behind it.
pub struct SearchRetriever {
    engine: SearchEngine,
    index: IndexStore,
    /// Cap on files pulled into candidates per query.
    max_files: usize,
}

impl std::fmt::Debug for SearchRetriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchRetriever")
            .field("root", &self.engine.root())
            .field("max_files", &self.max_files)
            .finish()
    }
}

impl SearchRetriever {
    pub fn new(engine: SearchEngine, index: IndexStore) -> Self {
        Self {
            engine,
            index,
            max_files: 12,
        }
    }

    pub fn with_max_files(mut self, max_files: usize) -> Self {
        self.max_files = max_files.max(1);
        self
    }

    async fn generation(&self, query: &RetrievalQuery) -> Result<Generation> {
        if let Some(g) = query.generation {
            return Ok(g);
        }
        self.index
            .current()
            .await
            .map_err(|e| ContextError::Retrieval(e.to_string()))?
            .map(|info| info.generation)
            .ok_or_else(|| ContextError::Retrieval("the workspace is not indexed".into()))
    }

    async fn source_candidate(
        &self,
        generation: Generation,
        path: &str,
        rank_relevance: f64,
        hit_symbol: Option<&str>,
        provenance: valyria_types::Provenance,
    ) -> Result<RetrievalCandidate> {
        let abs = self.engine.root().join(path);
        let content = std::fs::read_to_string(&abs).unwrap_or_default();

        let symbols = self
            .index
            .symbols_in(generation, path)
            .await
            .map_err(|e| ContextError::Retrieval(e.to_string()))?;

        let content_bytes = content.as_bytes();
        let spans: Vec<SymbolSpan> = symbols
            .iter()
            .filter_map(|s| {
                let body = content_bytes
                    .get(s.span.start_byte..s.span.end_byte)
                    .and_then(|b| std::str::from_utf8(b).ok())?;
                let relevance = if hit_symbol.is_some_and(|h| h == s.symbol_path || h == s.name) {
                    1.0
                } else {
                    0.5 * rank_relevance + 0.25
                };
                Some(SymbolSpan {
                    symbol_path: s.symbol_path.clone(),
                    kind: s.kind.as_str().to_string(),
                    signature: s.signature.clone(),
                    doc: s.doc.clone(),
                    start_line: s.span.start_line,
                    end_line: s.span.end_line,
                    body: body.to_string(),
                    relevance,
                })
            })
            .collect();

        let content = if spans.is_empty() {
            CandidateContent::Text {
                text: if content.is_empty() {
                    format!("(could not read {path})")
                } else {
                    content
                },
            }
        } else {
            CandidateContent::Source {
                path: path.to_string(),
                header: module_header(&content),
                symbols: spans,
            }
        };

        Ok(RetrievalCandidate::new(
            Trust::RepoData,
            provenance,
            SectionKind::Repository,
            rank_relevance,
            content,
        ))
    }
}

impl SearchRetriever {
    /// Run the search engine to completion.
    ///
    /// `SearchEngine::search` returns a `!Send` future — it holds a `gix`
    /// repository handle across `.await` points — so it cannot be awaited
    /// inside a `Send` async-trait method directly. Until that changes
    /// upstream, the search runs on a dedicated current-thread runtime on
    /// a scoped OS thread; only the plain-data `SearchResults` crosses
    /// back. `SearchRetriever` is a coarse, once-per-retrieval call, not a
    /// hot path, so the bridge is acceptable — and this keeps the
    /// [`Retriever`] trait `Send` for the eventual live-runtime wiring.
    fn run_search(&self, sq: &SearchQuery) -> Result<valyria_search::SearchResults> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| ContextError::Retrieval(e.to_string()))?;
                    rt.block_on(self.engine.search(sq))
                        .map_err(|e| ContextError::Retrieval(e.to_string()))
                })
                .join()
                .map_err(|_| ContextError::Retrieval("search thread panicked".into()))?
        })
    }
}

#[async_trait]
impl Retriever for SearchRetriever {
    async fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalCandidate>> {
        let generation = self.generation(query).await?;

        let mut sq = SearchQuery::new(&query.intent).limit(self.max_files.max(query.limit));
        for anchor in &query.anchors {
            sq = sq.anchor(anchor.clone());
        }
        if !query.symbols.is_empty() {
            sq = sq.mode(SearchMode::Symbol);
        }

        let results = self.run_search(&sq)?;

        // De-duplicate by path, keeping the best-ranked occurrence.
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let total = results.hits.len().max(1) as f64;
        for (i, hit) in results.hits.iter().enumerate() {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if out.len() >= self.max_files {
                break;
            }
            let rank_relevance = 1.0 - (i as f64 / total);
            let cand = self
                .source_candidate(
                    generation,
                    &hit.path,
                    rank_relevance,
                    hit.symbol_path.as_deref(),
                    hit.provenance(),
                )
                .await?;
            out.push(cand);
        }

        // Anything the caller explicitly named that search did not surface.
        for path in &query.explicit_paths {
            if seen.insert(path.clone()) {
                let prov = valyria_types::Provenance::new(valyria_types::ProvenanceSource::File {
                    path: path.clone(),
                })
                .with_step("context:explicit");
                out.push(
                    self.source_candidate(generation, path, 1.0, None, prov)
                        .await?,
                );
            }
        }

        Ok(out)
    }
}

/// The leading `//!` / `///` / `//` / `#` comment block of a file, if any —
/// used as the source candidate's header.
fn module_header(content: &str) -> Option<String> {
    let mut lines = Vec::new();
    for line in content.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with('#') || t.is_empty() {
            if !t.is_empty() {
                lines.push(t.to_string());
            }
            if lines.len() >= 8 {
                break;
            }
        } else {
            break;
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}
