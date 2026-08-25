//! `Rng` (determinism convention, §3): an injected randomness source so
//! anything that samples, jitters, or shuffles is reproducible under test.

use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use std::sync::Arc;

pub trait Rng: Send + Sync {
    fn next_u64(&self) -> u64;

    fn gen_range(&self, low: u64, high: u64) -> u64 {
        assert!(low < high, "gen_range requires low < high");
        low + self.next_u64() % (high - low)
    }
}

#[derive(Clone)]
pub struct OsRng;

impl Rng for OsRng {
    fn next_u64(&self) -> u64 {
        rand::rngs::OsRng.next_u64()
    }
}

/// A seeded, reproducible RNG for tests: the same seed always produces the
/// same sequence, across processes and platforms.
#[derive(Clone)]
pub struct DeterministicRng(Arc<Mutex<StdRng>>);

impl DeterministicRng {
    pub fn from_seed(seed: u64) -> Self {
        Self(Arc::new(Mutex::new(StdRng::seed_from_u64(seed))))
    }
}

impl Rng for DeterministicRng {
    fn next_u64(&self) -> u64 {
        self.0.lock().next_u64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let a = DeterministicRng::from_seed(7);
        let b = DeterministicRng::from_seed(7);
        let seq_a: Vec<u64> = (0..10).map(|_| a.next_u64()).collect();
        let seq_b: Vec<u64> = (0..10).map(|_| b.next_u64()).collect();
        assert_eq!(seq_a, seq_b);
    }

    #[test]
    fn different_seeds_differ() {
        let a = DeterministicRng::from_seed(1);
        let b = DeterministicRng::from_seed(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn gen_range_stays_in_bounds() {
        let rng = DeterministicRng::from_seed(42);
        for _ in 0..1000 {
            let v = rng.gen_range(10, 20);
            assert!((10..20).contains(&v));
        }
    }
}
