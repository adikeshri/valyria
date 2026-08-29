//! Property fuzzing for tool-input handling (§7: "tool inputs ...
//! proptest").
//!
//! The security-load-bearing target is [`canonical_input_hash`]: D2 binds
//! an `Authorization` to `canonical_input_hash(input)`, so if two inputs
//! that are semantically the same could hash differently — or if a
//! hostile input could panic the hasher — the TOCTOU guarantee ("you
//! cannot get approval for `rm ./tmp` and then run `rm -rf /`") would
//! have a hole.

use proptest::prelude::*;
use serde_json::{json, Value};
use valyria_tools::{all_tools, canonical_input_hash};

/// A recursive arbitrary JSON value, bounded in depth and size.
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-zA-Z0-9 _.:/-]{0,20}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 40, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::hash_map("[a-z_]{1,8}", inner, 0..6)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Rebuild a value so any object is reconstructed from a shuffled
/// key/value list — structurally equal, physically fresh.
fn rebuilt(v: &Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.iter().map(rebuilt).collect()),
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> =
                map.iter().map(|(k, v)| (k.clone(), rebuilt(v))).collect();
            pairs.reverse();
            Value::Object(pairs.into_iter().collect())
        }
        other => other.clone(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 600, ..ProptestConfig::default() })]

    #[test]
    fn canonical_hash_is_total_and_deterministic(v in arb_json()) {
        let h1 = canonical_input_hash(&v);
        let h2 = canonical_input_hash(&v);
        prop_assert_eq!(h1, h2);
    }

    #[test]
    fn canonical_hash_ignores_object_key_order(v in arb_json()) {
        prop_assert_eq!(canonical_input_hash(&v), canonical_input_hash(&rebuilt(&v)));
    }

    #[test]
    fn a_changed_value_changes_the_hash(path in "[a-z]{1,6}", a in any::<i64>(), b in any::<i64>()) {
        prop_assume!(a != b);
        let va = json!({ &path: a });
        let vb = json!({ &path: b });
        prop_assert_ne!(canonical_input_hash(&va), canonical_input_hash(&vb));
    }
}

#[test]
fn every_registered_tool_has_a_well_formed_descriptor() {
    let descriptors = all_tools().descriptors();
    assert!(!descriptors.is_empty());
    let mut seen = std::collections::HashSet::new();
    for d in &descriptors {
        assert!(!d.name.is_empty(), "a tool has an empty name");
        assert!(seen.insert(d.name), "duplicate tool name: {}", d.name);
        // The input schema must at least be a JSON object.
        assert!(
            d.input_schema.is_object(),
            "{} has a non-object input_schema",
            d.name
        );
    }
}
