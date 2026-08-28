//! [`Embedding`]: a dense vector and the handful of operations search
//! needs over one.
//!
//! Vectors are stored L2-normalized, so cosine similarity is just a dot
//! product. [`cosine`] still divides by the norms defensively — a
//! zero vector (a chunk with no recognisable tokens) must compare as
//! "unrelated to everything" rather than produce a `NaN`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Embedding(Vec<f32>);

impl Embedding {
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    pub fn zeros(dim: usize) -> Self {
        Self(vec![0.0; dim])
    }

    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<f32> {
        self.0
    }

    pub fn norm(&self) -> f32 {
        self.0.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    /// Scale to unit length in place. A zero vector is left as-is: there
    /// is no meaningful direction to normalize it to, and callers treat
    /// "norm 0" as "no signal".
    pub fn normalize(&mut self) {
        let norm = self.norm();
        if norm > 0.0 {
            for x in &mut self.0 {
                *x /= norm;
            }
        }
    }

    pub fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    pub fn dot(&self, other: &Embedding) -> f32 {
        self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum()
    }

    /// Little-endian `f32` bytes, for the `vector BLOB` column. Endianness
    /// is fixed rather than native so an index built on one machine reads
    /// correctly on another.
    pub fn to_blob(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() * 4);
        for x in &self.0 {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out
    }

    /// Inverse of [`Self::to_blob`]. `None` for a byte string whose length
    /// is not a multiple of four (a truncated or corrupt blob).
    pub fn from_blob(bytes: &[u8]) -> Option<Self> {
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let values = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Some(Self(values))
    }
}

/// Cosine similarity in `[-1.0, 1.0]`. `0.0` whenever either side has no
/// magnitude, so an empty chunk is simply unrelated to everything rather
/// than a source of `NaN` that would poison a ranking.
pub fn cosine(a: &Embedding, b: &Embedding) -> f32 {
    let na = a.norm();
    let nb = b.norm();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (a.dot(b) / (na * nb)).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing_gives_unit_length() {
        let v = Embedding::new(vec![3.0, 4.0]).normalized();
        assert!((v.norm() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_zero_vector_survives_normalization_and_compares_as_unrelated() {
        let z = Embedding::zeros(4).normalized();
        assert_eq!(z.norm(), 0.0);
        assert_eq!(cosine(&z, &Embedding::new(vec![1.0, 0.0, 0.0, 0.0])), 0.0);
    }

    #[test]
    fn identical_direction_is_similarity_one() {
        let a = Embedding::new(vec![1.0, 2.0, 3.0]);
        let b = Embedding::new(vec![2.0, 4.0, 6.0]);
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_direction_is_similarity_minus_one() {
        let a = Embedding::new(vec![1.0, 0.0]);
        let b = Embedding::new(vec![-1.0, 0.0]);
        assert!((cosine(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn blob_round_trips_exactly() {
        let v = Embedding::new(vec![0.5, -0.25, 1.0, 0.0, -3.5]);
        let back = Embedding::from_blob(&v.to_blob()).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn a_blob_of_the_wrong_length_is_rejected() {
        assert!(Embedding::from_blob(&[1, 2, 3]).is_none());
    }
}
