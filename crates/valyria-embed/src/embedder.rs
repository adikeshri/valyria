//! [`Embedder`]: the trait a real embedding model will implement, and a
//! deterministic offline default.
//!
//! `valyria-model` (Phase 9) will provide a model-backed `Embedder`. This
//! crate ships [`HashingEmbedder`] so that everything above it —
//! `valyria-search`, the context engine — can be built and tested now,
//! and so that semantic search works on a machine that never downloads a
//! model.
//!
//! ## What a hashing embedder can and cannot do
//!
//! [`HashingEmbedder`] is *feature hashing*: every token and every
//! character trigram of every token is hashed into a fixed number of
//! buckets with a pseudo-random sign, and the accumulated vector is
//! L2-normalized. Two texts that share vocabulary — the same identifiers,
//! the same domain words — land close together; two texts that share only
//! meaning do not. That is a real, if modest, retrieval signal, and it is
//! completely deterministic, which is what lets the search tests assert
//! exact rankings. It is explicitly *not* a substitute for a trained
//! model, and `valyria-search` treats semantic hits as one ranked input
//! among several, never the sole authority.

use std::hash::Hasher;

use rayon::prelude::*;

use crate::vector::Embedding;

/// Default embedding width. Large enough that feature-hash collisions are
/// rare over a repository's vocabulary, small enough that a 100k-chunk
/// store is tens of megabytes rather than hundreds.
pub const DEFAULT_EMBED_DIM: usize = 256;

/// Produces a vector for a piece of text.
///
/// Implementations must be deterministic (the same text yields the same
/// vector every call) and must return vectors of exactly [`Self::dim`]
/// length. [`Self::embed_batch`] exists so a model-backed implementation
/// can amortize a GPU round-trip; the default runs [`Self::embed`] across
/// a `rayon` pool.
pub trait Embedder: Send + Sync {
    /// A stable identifier for this embedder and its configuration
    /// (`"hashing-v1-d256"`). Stored alongside the vectors so a rebuild
    /// can tell whether cached vectors are still valid.
    fn id(&self) -> String;

    fn dim(&self) -> usize;

    fn embed(&self, text: &str) -> Embedding;

    fn embed_batch(&self, texts: &[&str]) -> Vec<Embedding> {
        texts.par_iter().map(|t| self.embed(t)).collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HashingEmbedder {
    dim: usize,
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self {
            dim: DEFAULT_EMBED_DIM,
        }
    }
}

impl HashingEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl Embedder for HashingEmbedder {
    fn id(&self) -> String {
        format!("hashing-v1-d{}", self.dim)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Embedding {
        let mut acc = vec![0.0f32; self.dim];
        for token in tokenize(text) {
            add_feature(&mut acc, &token, self.dim);
            // Character trigrams give partial-match behaviour: `parse`
            // and `parser` share two of three trigrams, so a query for
            // one retrieves the other even though the whole tokens
            // differ.
            for trigram in char_ngrams(&token, 3) {
                add_feature(&mut acc, &trigram, self.dim);
            }
        }
        Embedding::new(acc).normalized()
    }
}

/// Lowercase alphanumeric runs, `_` kept so `snake_case` identifiers stay
/// whole. Mirrors the tokenization `valyria-index`'s FTS query builder
/// uses, so lexical and semantic search agree on what a "word" is.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

fn char_ngrams(token: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < n {
        return Vec::new();
    }
    chars.windows(n).map(|w| w.iter().collect()).collect()
}

fn add_feature(acc: &mut [f32], feature: &str, dim: usize) {
    let h = stable_hash(feature.as_bytes());
    let bucket = (h % dim as u64) as usize;
    // Top bit of the hash picks the sign, so unrelated features that
    // collide in the same bucket tend to cancel rather than always add.
    let sign = if (h >> 63) & 1 == 1 { 1.0 } else { -1.0 };
    acc[bucket] += sign;
}

/// A fixed, dependency-free hash (FNV-1a). It only needs to be stable
/// across runs and platforms and reasonably well distributed — not
/// cryptographic — and pinning the algorithm here means a stored vector
/// stays reproducible regardless of what `DefaultHasher` does in a future
/// std release.
fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hasher = Fnv1a::default();
    hasher.write(bytes);
    hasher.finish()
}

struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf29ce484222325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::cosine;

    #[test]
    fn embedding_is_deterministic() {
        let e = HashingEmbedder::default();
        assert_eq!(
            e.embed("fn parse the parser"),
            e.embed("fn parse the parser")
        );
    }

    #[test]
    fn embedding_has_the_configured_dimension() {
        let e = HashingEmbedder::new(64);
        assert_eq!(e.embed("anything at all").dim(), 64);
    }

    #[test]
    fn shared_vocabulary_lands_closer_than_unrelated_text() {
        let e = HashingEmbedder::default();
        let parser = e.embed("struct Parser parse tokens into an ast syntax tree");
        let parser2 = e.embed("the parser parses a token stream and builds the ast");
        let unrelated = e.embed("configure the retry backoff and network timeout settings");

        assert!(
            cosine(&parser, &parser2) > cosine(&parser, &unrelated),
            "texts about parsing should be nearer each other than to text about networking"
        );
    }

    #[test]
    fn trigrams_give_partial_identifier_matches() {
        let e = HashingEmbedder::default();
        let a = e.embed("parser");
        let b = e.embed("parse");
        let c = e.embed("network");
        assert!(cosine(&a, &b) > cosine(&a, &c));
    }

    #[test]
    fn empty_text_is_the_zero_vector() {
        let e = HashingEmbedder::default();
        assert_eq!(e.embed("").norm(), 0.0);
        assert_eq!(e.embed("   !!! ---").norm(), 0.0);
    }

    #[test]
    fn batch_matches_one_at_a_time() {
        let e = HashingEmbedder::default();
        let texts: [&str; 3] = ["alpha beta", "gamma delta", "epsilon"];
        let batch = e.embed_batch(&texts);
        for (text, vec) in texts.iter().zip(&batch) {
            assert_eq!(&e.embed(text), vec);
        }
    }

    #[test]
    fn id_reflects_dimension() {
        assert_eq!(HashingEmbedder::new(128).id(), "hashing-v1-d128");
    }
}
