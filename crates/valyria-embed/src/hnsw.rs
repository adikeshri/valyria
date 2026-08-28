//! A small, deterministic HNSW index for approximate nearest-neighbour
//! search over [`Embedding`]s.
//!
//! HNSW (Hierarchical Navigable Small World) keeps a layered set of
//! proximity graphs: most nodes live only on layer 0, a geometrically
//! thinning fraction reach higher layers, and a search greedily descends
//! from a single entry point. It turns "compare the query to every
//! vector" into "follow a few dozen edges", which is what makes semantic
//! search over a large repository feasible.
//!
//! This implementation is deliberately compact and has two properties the
//! rest of the crate leans on:
//!
//! * **Deterministic.** Level assignment uses a seeded RNG, so the same
//!   inserts in the same order build the same graph. Search tie-breaks by
//!   id. Two runs return identical results.
//! * **Checkable.** [`EmbedStore`](crate::EmbedStore) runs both this and
//!   exact brute-force cosine and a test asserts they agree, because an
//!   approximate index that is subtly wrong has no symptom of its own.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::vector::Embedding;

#[derive(Debug, Clone, Copy)]
pub struct HnswParams {
    /// Max neighbours per node on layers above 0.
    pub m: usize,
    /// Max neighbours per node on layer 0, where the graph must stay
    /// well-connected; conventionally `2 * m`.
    pub m0: usize,
    /// Size of the dynamic candidate list while inserting. Larger builds
    /// a better graph at a higher one-time cost.
    pub ef_construction: usize,
    /// Seed for level assignment. Fixed by default so builds are
    /// reproducible.
    pub seed: u64,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            m0: 32,
            ef_construction: 100,
            seed: 0x5eed_1234_abcd_0001,
        }
    }
}

/// Distance is `1 - cosine_similarity` over unit vectors, so smaller is
/// nearer and the range is `[0, 2]`.
fn distance(a: &Embedding, b: &Embedding) -> f32 {
    1.0 - a.dot(b)
}

/// An `(f32, u32)` pair ordered by the float (with `u32` as a stable
/// tie-break), usable in a `BinaryHeap`. `Max` orders so the *largest*
/// distance is on top (a bounded nearest set evicts its worst); `Min`
/// orders the other way (a frontier expands its best first).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    dist: f32,
    id: u32,
}

impl Eq for Scored {}

impl Scored {
    fn cmp_key(&self) -> (f32, u32) {
        (self.dist, self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaxByDist(Scored);
impl Ord for MaxByDist {
    fn cmp(&self, other: &Self) -> Ordering {
        let (ad, ai) = self.0.cmp_key();
        let (bd, bi) = other.0.cmp_key();
        ad.total_cmp(&bd).then(ai.cmp(&bi))
    }
}
impl PartialOrd for MaxByDist {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MinByDist(Scored);
impl Ord for MinByDist {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse of `MaxByDist` so the smallest distance is "greatest"
        // and pops first from a `BinaryHeap`.
        MaxByDist(other.0).cmp(&MaxByDist(self.0))
    }
}
impl PartialOrd for MinByDist {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub struct Hnsw {
    params: HnswParams,
    dim: usize,
    vectors: Vec<Embedding>,
    /// `links[node][layer]` — neighbour ids of `node` on `layer`.
    links: Vec<Vec<Vec<u32>>>,
    entry: Option<u32>,
    max_layer: usize,
    rng: StdRng,
    level_mult: f64,
}

impl Hnsw {
    pub fn new(dim: usize, params: HnswParams) -> Self {
        let level_mult = 1.0 / (params.m.max(2) as f64).ln();
        Self {
            params,
            dim,
            vectors: Vec::new(),
            links: Vec::new(),
            entry: None,
            max_layer: 0,
            rng: StdRng::seed_from_u64(params.seed),
            level_mult,
        }
    }

    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Insert one vector (assumed unit length) and return its internal id.
    ///
    /// Ids are assigned sequentially from 0, so a caller that inserts in a
    /// known order can map ids back to its own rows without a side table.
    pub fn insert(&mut self, vector: Embedding) -> u32 {
        let id = self.vectors.len() as u32;
        let level = self.random_level();
        self.vectors.push(vector);
        self.links.push(vec![Vec::new(); level + 1]);

        let Some(entry) = self.entry else {
            self.entry = Some(id);
            self.max_layer = level;
            return id;
        };

        let query = self.vectors[id as usize].clone();

        // Descend from the top with a single greedy walker until we reach
        // the highest layer this node participates in.
        let mut ep = entry;
        let mut layer = self.max_layer;
        while layer > level {
            ep = self.greedy_descend(&query, ep, layer);
            layer -= 1;
        }

        // From there down to layer 0, find candidates and wire up.
        let mut layer = level.min(self.max_layer);
        loop {
            let found = self.search_layer(&query, &[ep], self.params.ef_construction, layer);
            let m = if layer == 0 {
                self.params.m0
            } else {
                self.params.m
            };
            let selected = nearest(found, m);

            for &neighbour in &selected {
                self.links[id as usize][layer].push(neighbour);
                self.links[neighbour as usize][layer].push(id);
                self.prune(neighbour, layer);
            }
            self.prune(id, layer);

            ep = selected.first().copied().unwrap_or(ep);
            if layer == 0 {
                break;
            }
            layer -= 1;
        }

        if level > self.max_layer {
            self.max_layer = level;
            self.entry = Some(id);
        }
        id
    }

    /// The `k` nearest ids to `query`, nearest first, as `(id,
    /// similarity)` where similarity is cosine in `[-1, 1]`.
    ///
    /// `ef` is the search beam width: it is raised to at least `k`, and a
    /// larger value trades speed for recall.
    pub fn search(&self, query: &Embedding, k: usize, ef: usize) -> Vec<(u32, f32)> {
        if self.is_empty() || k == 0 {
            return Vec::new();
        }
        let Some(entry) = self.entry else {
            return Vec::new();
        };

        let mut ep = entry;
        let mut layer = self.max_layer;
        while layer > 0 {
            ep = self.greedy_descend(query, ep, layer);
            layer -= 1;
        }

        let ef = ef.max(k).max(1);
        let mut found = self.search_layer(query, &[ep], ef, 0);
        found.sort_by(|a, b| a.dist.total_cmp(&b.dist).then(a.id.cmp(&b.id)));
        found.truncate(k);
        found
            .into_iter()
            .map(|s| (s.id, (1.0 - s.dist).clamp(-1.0, 1.0)))
            .collect()
    }

    fn random_level(&mut self) -> usize {
        // Classic HNSW: level = floor(-ln(U) * level_mult).
        let u: f64 = self.rng.gen_range(f64::MIN_POSITIVE..1.0);
        (-u.ln() * self.level_mult).floor() as usize
    }

    /// Walk greedily on one layer from `start`, always stepping to the
    /// strictly-nearer neighbour, and return where it stops.
    fn greedy_descend(&self, query: &Embedding, start: u32, layer: usize) -> u32 {
        let mut current = start;
        let mut current_dist = distance(query, &self.vectors[current as usize]);
        loop {
            let mut moved = false;
            for &n in self.neighbours(current, layer) {
                let d = distance(query, &self.vectors[n as usize]);
                if d < current_dist {
                    current_dist = d;
                    current = n;
                    moved = true;
                }
            }
            if !moved {
                return current;
            }
        }
    }

    /// The core HNSW layer search: expand a frontier of the best-so-far
    /// candidates, keeping a bounded nearest set of size `ef`.
    fn search_layer(
        &self,
        query: &Embedding,
        entries: &[u32],
        ef: usize,
        layer: usize,
    ) -> Vec<Scored> {
        let mut visited: Vec<bool> = vec![false; self.vectors.len()];
        let mut frontier: BinaryHeap<MinByDist> = BinaryHeap::new();
        let mut nearest: BinaryHeap<MaxByDist> = BinaryHeap::new();

        for &e in entries {
            let d = distance(query, &self.vectors[e as usize]);
            let s = Scored { dist: d, id: e };
            visited[e as usize] = true;
            frontier.push(MinByDist(s));
            nearest.push(MaxByDist(s));
        }

        while let Some(MinByDist(candidate)) = frontier.pop() {
            let worst = nearest.peek().map(|w| w.0.dist).unwrap_or(f32::INFINITY);
            if candidate.dist > worst && nearest.len() >= ef {
                break;
            }
            for &n in self.neighbours(candidate.id, layer) {
                if visited[n as usize] {
                    continue;
                }
                visited[n as usize] = true;
                let d = distance(query, &self.vectors[n as usize]);
                let worst = nearest.peek().map(|w| w.0.dist).unwrap_or(f32::INFINITY);
                if d < worst || nearest.len() < ef {
                    let s = Scored { dist: d, id: n };
                    frontier.push(MinByDist(s));
                    nearest.push(MaxByDist(s));
                    if nearest.len() > ef {
                        nearest.pop();
                    }
                }
            }
        }

        nearest.into_iter().map(|m| m.0).collect()
    }

    fn neighbours(&self, node: u32, layer: usize) -> &[u32] {
        self.links[node as usize]
            .get(layer)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Trim `node`'s neighbour list on `layer` back to the layer's cap,
    /// keeping the closest, and drop duplicates that bidirectional wiring
    /// can introduce.
    fn prune(&mut self, node: u32, layer: usize) {
        let cap = if layer == 0 {
            self.params.m0
        } else {
            self.params.m
        };
        let query = self.vectors[node as usize].clone();
        let mut list = std::mem::take(&mut self.links[node as usize][layer]);
        list.sort_unstable();
        list.dedup();
        list.retain(|&n| n != node);
        if list.len() > cap {
            list.sort_by(|&a, &b| {
                let da = distance(&query, &self.vectors[a as usize]);
                let db = distance(&query, &self.vectors[b as usize]);
                da.total_cmp(&db).then(a.cmp(&b))
            });
            list.truncate(cap);
        }
        self.links[node as usize][layer] = list;
    }
}

/// The `m` nearest ids from a layer-search result, nearest first.
fn nearest(mut found: Vec<Scored>, m: usize) -> Vec<u32> {
    found.sort_by(|a, b| a.dist.total_cmp(&b.dist).then(a.id.cmp(&b.id)));
    found.truncate(m);
    found.into_iter().map(|s| s.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn random_unit(rng: &mut StdRng, dim: usize) -> Embedding {
        let v: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
        Embedding::new(v).normalized()
    }

    fn brute_force(vectors: &[Embedding], query: &Embedding, k: usize) -> Vec<u32> {
        let mut scored: Vec<(f32, u32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (1.0 - v.dot(query), i as u32))
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(k).map(|(_, i)| i).collect()
    }

    #[test]
    fn empty_index_returns_nothing() {
        let hnsw = Hnsw::new(8, HnswParams::default());
        assert!(hnsw.search(&Embedding::zeros(8), 5, 10).is_empty());
    }

    #[test]
    fn single_vector_is_its_own_nearest_neighbour() {
        let mut hnsw = Hnsw::new(4, HnswParams::default());
        let v = Embedding::new(vec![1.0, 0.0, 0.0, 0.0]);
        let id = hnsw.insert(v.clone());
        let hits = hnsw.search(&v, 3, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, id);
        assert!((hits[0].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn asking_for_more_than_exist_returns_all() {
        let mut hnsw = Hnsw::new(4, HnswParams::default());
        for i in 0..3 {
            let mut raw = vec![0.0; 4];
            raw[i] = 1.0;
            hnsw.insert(Embedding::new(raw));
        }
        let hits = hnsw.search(&Embedding::new(vec![1.0, 0.0, 0.0, 0.0]), 10, 10);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn recall_against_brute_force_is_high() {
        let mut rng = StdRng::seed_from_u64(42);
        let dim = 48;
        let vectors: Vec<Embedding> = (0..800).map(|_| random_unit(&mut rng, dim)).collect();

        let mut hnsw = Hnsw::new(dim, HnswParams::default());
        for v in &vectors {
            hnsw.insert(v.clone());
        }

        let k = 10;
        let mut hits = 0usize;
        let mut total = 0usize;
        for _ in 0..50 {
            let q = random_unit(&mut rng, dim);
            let exact: std::collections::BTreeSet<u32> =
                brute_force(&vectors, &q, k).into_iter().collect();
            let approx: Vec<u32> = hnsw.search(&q, k, 64).into_iter().map(|(i, _)| i).collect();
            hits += approx.iter().filter(|i| exact.contains(i)).count();
            total += k;
        }
        let recall = hits as f64 / total as f64;
        assert!(
            recall >= 0.9,
            "recall@{k} was {recall:.3}, expected >= 0.90"
        );
    }

    #[test]
    fn results_are_sorted_by_descending_similarity() {
        let mut rng = StdRng::seed_from_u64(7);
        let dim = 16;
        let mut hnsw = Hnsw::new(dim, HnswParams::default());
        for _ in 0..200 {
            hnsw.insert(random_unit(&mut rng, dim));
        }
        let q = random_unit(&mut rng, dim);
        let hits = hnsw.search(&q, 10, 32);
        for pair in hits.windows(2) {
            assert!(pair[0].1 >= pair[1].1);
        }
    }

    #[test]
    fn build_is_deterministic() {
        let mut rng = StdRng::seed_from_u64(1);
        let dim = 16;
        let vectors: Vec<Embedding> = (0..300).map(|_| random_unit(&mut rng, dim)).collect();
        let q = random_unit(&mut rng, dim);

        let build = || {
            let mut h = Hnsw::new(dim, HnswParams::default());
            for v in &vectors {
                h.insert(v.clone());
            }
            h.search(&q, 10, 32)
        };
        assert_eq!(build(), build());
    }
}
