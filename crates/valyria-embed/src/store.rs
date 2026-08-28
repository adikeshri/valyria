//! [`EmbedStore`]: persist chunk vectors for an index generation and
//! search them.
//!
//! The write path ([`EmbedStore::build_for`]) mirrors `valyria-graph`: a
//! generation's vectors are replaced wholesale, never merged, so a
//! rebuild cannot leave a stale vector behind. The one refinement is
//! reuse — a chunk whose `chunk_hash` already has a vector in the
//! generation being rebuilt from is copied forward rather than
//! re-embedded, which is the chunk-level invalidation §4.15 asks for.
//!
//! The read path has two entry points on purpose. [`EmbedStore::search`]
//! is approximate ([`Hnsw`]); [`EmbedStore::search_exact`] is
//! brute-force cosine. They must return the same top results on the same
//! data, and a test asserts it — the same defence `valyria-index` uses
//! against drift.

use std::sync::Arc;

use rusqlite::{params, Row};
use valyria_lang::Span;
use valyria_store::Store;
use valyria_types::Generation;
use valyria_util::ContentHash;

use crate::chunking::EmbedChunk;
use crate::embedder::Embedder;
use crate::error::{EmbedError, Result};
use crate::hnsw::{Hnsw, HnswParams};
use crate::vector::{cosine, Embedding};

/// One search result: where the matching chunk is, and how close it was.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub path: String,
    pub symbol_path: Option<String>,
    pub span: Span,
    /// Cosine similarity in `[-1.0, 1.0]`; higher is nearer.
    pub score: f32,
}

/// What one build produced. `reused` counts chunks whose vector was
/// copied from an earlier generation instead of recomputed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedStats {
    pub generation: Generation,
    pub embedder_id: String,
    pub dim: usize,
    pub chunks: u64,
    pub reused: u64,
}

#[derive(Clone)]
pub struct EmbedStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for EmbedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedStore")
            .field("db", &self.store.db_path())
            .finish()
    }
}

struct StoredChunk {
    path: String,
    symbol_path: Option<String>,
    span: Span,
    vector: Embedding,
}

impl EmbedStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Embed `chunks` and store them as the vectors for `generation`,
    /// replacing any previous build of the same generation.
    ///
    /// If `reuse_from` names a generation whose vectors are present, any
    /// chunk with a matching `chunk_hash` and the right dimensionality
    /// takes that generation's vector unchanged; only genuinely new or
    /// changed chunks are sent to the embedder.
    pub async fn build_for(
        &self,
        embedder: &dyn Embedder,
        generation: Generation,
        chunks: &[EmbedChunk],
        reuse_from: Option<Generation>,
    ) -> Result<EmbedStats> {
        let dim = embedder.dim();
        let embedder_id = embedder.id();

        // Which prior vectors can we reuse? Keyed by chunk hash; only
        // vectors of the current dimensionality qualify.
        let reusable = match reuse_from {
            Some(from) => self.vectors_by_hash(from, dim).await?,
            None => std::collections::HashMap::new(),
        };

        let mut to_embed: Vec<usize> = Vec::new();
        let mut vectors: Vec<Option<Embedding>> = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            match reusable.get(&chunk.chunk_hash) {
                Some(v) => vectors.push(Some(v.clone())),
                None => {
                    to_embed.push(i);
                    vectors.push(None);
                }
            }
        }

        let reused = (chunks.len() - to_embed.len()) as u64;

        if !to_embed.is_empty() {
            let texts: Vec<&str> = to_embed.iter().map(|&i| chunks[i].text.as_str()).collect();
            let fresh = embedder.embed_batch(&texts);
            for (&i, v) in to_embed.iter().zip(fresh) {
                vectors[i] = Some(v);
            }
        }

        let rows: Vec<(EmbedChunk, Vec<u8>)> = chunks
            .iter()
            .cloned()
            .zip(
                vectors
                    .into_iter()
                    .map(|v| v.expect("every chunk got a vector").to_blob()),
            )
            .collect();

        let g = generation.0 as i64;
        let stats_id = embedder_id.clone();
        let chunk_count = rows.len() as u64;

        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                for table in ["embed_chunk", "embed_build"] {
                    tx.execute(&format!("DELETE FROM {table} WHERE generation = ?1"), [g])?;
                }
                {
                    let mut stmt = tx.prepare(
                        "INSERT INTO embed_chunk
                            (generation, path, symbol_path, start_byte, end_byte,
                             start_line, end_line, chunk_hash, vector)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    )?;
                    for (chunk, blob) in &rows {
                        stmt.execute(params![
                            g,
                            chunk.path,
                            chunk.symbol_path,
                            chunk.span.start_byte as i64,
                            chunk.span.end_byte as i64,
                            chunk.span.start_line as i64,
                            chunk.span.end_line as i64,
                            chunk.chunk_hash.to_hex(),
                            blob,
                        ])?;
                    }
                }
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                tx.execute(
                    "INSERT INTO embed_build
                        (generation, built_at_ms, embedder_id, dim, chunk_count, reused_count)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        g,
                        now_ms,
                        stats_id,
                        dim as i64,
                        chunk_count as i64,
                        reused as i64
                    ],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(EmbedError::from)?;

        Ok(EmbedStats {
            generation,
            embedder_id,
            dim,
            chunks: chunk_count,
            reused,
        })
    }

    /// Approximate nearest-neighbour search: the `k` chunks whose vectors
    /// are closest to `query`, nearest first.
    pub async fn search(
        &self,
        generation: Generation,
        query: &Embedding,
        k: usize,
    ) -> Result<Vec<VectorHit>> {
        let rows = self.load(generation, query.dim()).await?;
        if rows.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        let mut hnsw = Hnsw::new(query.dim(), HnswParams::default());
        for row in &rows {
            hnsw.insert(row.vector.clone());
        }
        // A wide beam relative to k: this index is rebuilt per query at
        // present, so recall matters more than the last few percent of
        // speed.
        let ef = (k * 8).max(64);
        Ok(hnsw
            .search(query, k, ef)
            .into_iter()
            .map(|(id, score)| hit(&rows[id as usize], score))
            .collect())
    }

    /// Exact brute-force cosine search. Slower, but the ground truth the
    /// approximate path is checked against, and what `--explain` and the
    /// drift check use.
    pub async fn search_exact(
        &self,
        generation: Generation,
        query: &Embedding,
        k: usize,
    ) -> Result<Vec<VectorHit>> {
        let rows = self.load(generation, query.dim()).await?;
        let mut scored: Vec<VectorHit> = rows
            .iter()
            .map(|row| hit(row, cosine(&row.vector, query)))
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.path.cmp(&b.path))
                .then(a.span.start_byte.cmp(&b.span.start_byte))
        });
        scored.truncate(k);
        Ok(scored)
    }

    pub async fn stats(&self, generation: Generation) -> Result<EmbedStats> {
        self.ensure_built(generation).await?;
        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT embedder_id, dim, chunk_count, reused_count
                     FROM embed_build WHERE generation = ?1",
                    [g],
                    |row| {
                        Ok(EmbedStats {
                            generation: Generation(g as u64),
                            embedder_id: row.get(0)?,
                            dim: row.get::<_, i64>(1)? as usize,
                            chunks: row.get::<_, i64>(2)? as u64,
                            reused: row.get::<_, i64>(3)? as u64,
                        })
                    },
                )?)
            })
            .await
            .map_err(EmbedError::from)
    }

    /// Whether vectors exist for this generation. Unlike
    /// [`Self::ensure_built`] this never errors — it is the check
    /// `valyria-search` uses to decide whether semantic search runs or
    /// silently steps aside.
    pub async fn is_built(&self, generation: Generation) -> Result<bool> {
        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM embed_build WHERE generation = ?1)",
                    [g],
                    |row| row.get(0),
                )?)
            })
            .await
            .map_err(EmbedError::from)
    }

    pub async fn built_generations(&self) -> Result<Vec<Generation>> {
        self.store
            .call(|conn| {
                let mut stmt =
                    conn.prepare("SELECT generation FROM embed_build ORDER BY generation")?;
                let rows = stmt
                    .query_map([], |row| Ok(Generation(row.get::<_, i64>(0)? as u64)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(EmbedError::from)
    }

    /// Drop every build for a generation below `keep_from`.
    pub async fn prune_before(&self, keep_from: Generation) -> Result<u64> {
        let g = keep_from.0 as i64;
        self.store
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut removed = 0u64;
                for table in ["embed_chunk", "embed_build"] {
                    removed += tx
                        .execute(&format!("DELETE FROM {table} WHERE generation < ?1"), [g])?
                        as u64;
                }
                tx.commit()?;
                Ok(removed)
            })
            .await
            .map_err(EmbedError::from)
    }

    async fn ensure_built(&self, generation: Generation) -> Result<()> {
        if self.is_built(generation).await? {
            Ok(())
        } else {
            Err(EmbedError::NotBuilt(generation))
        }
    }

    async fn vectors_by_hash(
        &self,
        generation: Generation,
        dim: usize,
    ) -> Result<std::collections::HashMap<ContentHash, Embedding>> {
        let g = generation.0 as i64;
        let blob_len = dim * 4;
        self.store
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT chunk_hash, vector FROM embed_chunk WHERE generation = ?1")?;
                let rows = stmt
                    .query_map([g], |row| {
                        let hash: String = row.get(0)?;
                        let blob: Vec<u8> = row.get(1)?;
                        Ok((hash, blob))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = std::collections::HashMap::new();
                for (hash, blob) in rows {
                    if blob.len() != blob_len {
                        continue;
                    }
                    if let (Ok(h), Some(v)) =
                        (hash.parse::<ContentHash>(), Embedding::from_blob(&blob))
                    {
                        out.insert(h, v);
                    }
                }
                Ok(out)
            })
            .await
            .map_err(EmbedError::from)
    }

    async fn load(&self, generation: Generation, query_dim: usize) -> Result<Vec<StoredChunk>> {
        self.ensure_built(generation).await?;
        let stats = self.stats(generation).await?;
        if stats.dim != query_dim {
            return Err(EmbedError::DimensionMismatch {
                expected: query_dim,
                found: stats.dim,
            });
        }

        let g = generation.0 as i64;
        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT path, symbol_path, start_byte, end_byte, start_line, end_line, vector
                     FROM embed_chunk WHERE generation = ?1
                     ORDER BY path, start_byte",
                )?;
                let rows = stmt
                    .query_map([g], stored_chunk_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows.into_iter().flatten().collect())
            })
            .await
            .map_err(EmbedError::from)
    }
}

fn hit(row: &StoredChunk, score: f32) -> VectorHit {
    VectorHit {
        path: row.path.clone(),
        symbol_path: row.symbol_path.clone(),
        span: row.span,
        score,
    }
}

/// `Ok(None)` for a row whose vector blob is corrupt — skipped rather
/// than failing the whole search, the same way the scanner skips an
/// unreadable file.
fn stored_chunk_from_row(row: &Row<'_>) -> rusqlite::Result<Option<StoredChunk>> {
    let blob: Vec<u8> = row.get(6)?;
    let Some(vector) = Embedding::from_blob(&blob) else {
        return Ok(None);
    };
    Ok(Some(StoredChunk {
        path: row.get(0)?,
        symbol_path: row.get(1)?,
        span: Span {
            start_byte: row.get::<_, i64>(2)? as usize,
            end_byte: row.get::<_, i64>(3)? as usize,
            start_line: row.get::<_, i64>(4)? as u32,
            end_line: row.get::<_, i64>(5)? as u32,
        },
        vector,
    }))
}
