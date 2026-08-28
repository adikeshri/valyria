//! `valyria-embed` — layer 2 (Repository intelligence).
//!
//! The semantic-retrieval half of search: turn source into vectors, store
//! them, and find the nearest ones to a query vector.
//!
//! Three deliberate choices shape this crate:
//!
//! **Embedding is a trait, and the default implementation needs no model.**
//! [`Embedder`] is what `valyria-model` will implement once a real
//! embedding model is loaded (Phase 9). Until then — and forever, on a
//! machine that never installs one — [`HashingEmbedder`] produces
//! deterministic feature-hashed vectors offline. Semantic search is
//! therefore *optional enrichment*: it is better with a real model, but it
//! is never broken without one (§8 risk register, "search degrades
//! gracefully").
//!
//! **Vectors are generational, exactly like the index** (D8). Every row is
//! stamped with the [`Generation`](valyria_types::Generation) of the index
//! it was derived from, so a search at generation *N* sees the vectors for
//! *N* however far the index has moved on, and a rebuild for a new
//! generation reuses the vectors of unchanged chunks by content hash
//! rather than re-embedding them (§4.15, "embedding invalidation
//! (chunk-level, by content hash)").
//!
//! **The nearest-neighbour index is an [`Hnsw`], checked against exact
//! search.** Approximate search has no symptom of its own when it is
//! subtly wrong — it just returns slightly worse neighbours — so
//! [`EmbedStore::search`] (HNSW) and [`EmbedStore::search_exact`]
//! (brute-force cosine) exist side by side and a test asserts their
//! agreement, the same way `valyria-index` guards against index drift.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod chunking;
pub mod embedder;
pub mod error;
pub mod hnsw;
pub mod migrations;
pub mod pipeline;
pub mod store;
pub mod vector;

pub use chunking::{chunk_source, EmbedChunk};
pub use embedder::{Embedder, HashingEmbedder, DEFAULT_EMBED_DIM};
pub use error::{EmbedError, Result};
pub use hnsw::{Hnsw, HnswParams};
pub use migrations::MIGRATIONS;
pub use pipeline::{EmbedOptions, EmbedPipeline};
pub use store::{EmbedStats, EmbedStore, VectorHit};
pub use vector::{cosine, Embedding};
