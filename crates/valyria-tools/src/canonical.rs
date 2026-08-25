//! Canonical input hashing: what an `Authorization` actually binds to.
//! `serde_json`'s default `Map` (no `preserve_order` feature) is a
//! `BTreeMap`, so key order in the serialized bytes is deterministic
//! regardless of the order fields were inserted in — no extra
//! canonicalization pass needed.

use serde_json::Value;
use valyria_util::ContentHash;

pub fn canonical_input_hash(input: &Value) -> ContentHash {
    ContentHash::of_bytes(&serde_json::to_vec(input).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_order_does_not_affect_the_hash() {
        let a = serde_json::json!({"b": 1, "a": 2});
        let b = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(canonical_input_hash(&a), canonical_input_hash(&b));
    }

    #[test]
    fn different_values_hash_differently() {
        let a = serde_json::json!({"path": "a.txt"});
        let b = serde_json::json!({"path": "b.txt"});
        assert_ne!(canonical_input_hash(&a), canonical_input_hash(&b));
    }
}
