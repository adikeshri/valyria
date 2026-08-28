//! [`EmbedPipeline`]: from an index generation to stored vectors.
//!
//! The pipeline reads the file list from the index (so it embeds exactly
//! what was indexed — same `.gitignore` handling, same binary/oversize
//! filtering), reads each file's bytes from disk, chunks it at syntactic
//! boundaries, and hands the chunks to [`EmbedStore::build_for`]. Parsing
//! and chunking run across a `rayon` pool; the store sees one transaction.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use valyria_index::IndexStore;
use valyria_lang::{FileFacts, LanguageRegistry};
use valyria_types::Generation;

use crate::chunking::{chunk_source, EmbedChunk, DEFAULT_CHUNK_BYTES};
use crate::embedder::Embedder;
use crate::error::Result;
use crate::store::{EmbedStats, EmbedStore};

#[derive(Debug, Clone, Copy)]
pub struct EmbedOptions {
    /// Chunk-size budget in bytes, passed through to the chunker.
    pub max_chunk_bytes: usize,
    /// Files larger than this are skipped. They were recorded by the
    /// index (search can still find them by path and content) but a
    /// multi-megabyte generated file has nothing a semantic search wants.
    pub max_file_bytes: u64,
}

impl Default for EmbedOptions {
    fn default() -> Self {
        Self {
            max_chunk_bytes: DEFAULT_CHUNK_BYTES,
            max_file_bytes: 1_000_000,
        }
    }
}

pub struct EmbedPipeline {
    root: PathBuf,
    registry: LanguageRegistry,
    embedder: std::sync::Arc<dyn Embedder>,
    store: EmbedStore,
    options: EmbedOptions,
}

impl std::fmt::Debug for EmbedPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedPipeline")
            .field("root", &self.root)
            .field("embedder", &self.embedder.id())
            .field("store", &self.store)
            .field("options", &self.options)
            .finish()
    }
}

impl EmbedPipeline {
    pub fn new(
        root: impl Into<PathBuf>,
        registry: LanguageRegistry,
        embedder: std::sync::Arc<dyn Embedder>,
        store: EmbedStore,
    ) -> Self {
        Self {
            root: root.into(),
            registry,
            embedder,
            store,
            options: EmbedOptions::default(),
        }
    }

    pub fn with_options(mut self, options: EmbedOptions) -> Self {
        self.options = options;
        self
    }

    pub fn store(&self) -> &EmbedStore {
        &self.store
    }

    pub fn embedder(&self) -> &std::sync::Arc<dyn Embedder> {
        &self.embedder
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Embed every eligible file in `generation` from scratch.
    pub async fn bootstrap(
        &self,
        index: &IndexStore,
        generation: Generation,
    ) -> Result<EmbedStats> {
        self.embed_generation(index, generation, None).await
    }

    /// Embed `generation`, reusing the vectors of unchanged chunks from
    /// `previous`. The incremental path: only chunks whose bytes actually
    /// changed are sent to the embedder.
    pub async fn reembed(
        &self,
        index: &IndexStore,
        generation: Generation,
        previous: Generation,
    ) -> Result<EmbedStats> {
        self.embed_generation(index, generation, Some(previous))
            .await
    }

    async fn embed_generation(
        &self,
        index: &IndexStore,
        generation: Generation,
        reuse_from: Option<Generation>,
    ) -> Result<EmbedStats> {
        let files = index.files(generation).await?;
        let chunks = self.chunk_files(&files);
        self.store
            .build_for(&*self.embedder, generation, &chunks, reuse_from)
            .await
    }

    fn chunk_files(&self, files: &[valyria_index::FileRecord]) -> Vec<EmbedChunk> {
        let mut chunks: Vec<EmbedChunk> = files
            .par_iter()
            .filter(|f| !f.is_binary && f.size_bytes <= self.options.max_file_bytes)
            .filter_map(|f| {
                let source = match std::fs::read_to_string(self.root.join(&f.path)) {
                    Ok(s) => s,
                    Err(e) => {
                        // The file was indexed but is now unreadable or no
                        // longer valid UTF-8: skip it rather than fail the
                        // whole build.
                        tracing::warn!(path = %f.path, error = %e, "skipping file for embedding");
                        return None;
                    }
                };
                let facts = match f.language.as_deref() {
                    Some(id) => self
                        .registry
                        .extract_facts_as(id, &source)
                        .unwrap_or_else(|e| {
                            tracing::warn!(path = %f.path, error = %e, "extraction failed; chunking without symbols");
                            FileFacts::default()
                        }),
                    None => FileFacts::default(),
                };
                Some(chunk_source(
                    &f.path,
                    &source,
                    &facts,
                    self.options.max_chunk_bytes,
                ))
            })
            .flatten()
            .collect();
        // Deterministic order in, deterministic ids out of the HNSW build.
        chunks.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.span.start_byte.cmp(&b.span.start_byte))
        });
        chunks
    }
}
