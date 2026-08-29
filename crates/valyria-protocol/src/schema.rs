//! Schema export (§4.27, D11). The wire contract — [`Request`],
//! [`Response`], [`WireEvent`] — rendered as JSON Schema, so it can be
//! checked into `docs/protocol/` and a CI gate (`xtask check-protocol`)
//! can fail any drift that did not also bump [`crate::PROTOCOL_VERSION`].
//!
//! Keeping the generation *here* (rather than in `xtask`) means the schema
//! is derived from the exact types the runtime serializes — `xtask` only
//! writes the files and diffs them.

use crate::envelope::{Request, Response};
use crate::messages::WireEvent;

/// One exported schema file: `(filename, pretty-printed JSON)`.
pub type SchemaFile = (&'static str, String);

/// Every schema file `xtask schema` writes into `docs/protocol/`, in a
/// deterministic order. `version.txt` pins the schema to a protocol
/// version so the compat gate can tell "changed, version bumped" from
/// "changed, version not bumped".
pub fn export() -> Vec<SchemaFile> {
    vec![
        ("request.schema.json", schema_json::<Request>()),
        ("response.schema.json", schema_json::<Response>()),
        ("event.schema.json", schema_json::<WireEvent>()),
        ("version.txt", format!("{}\n", crate::PROTOCOL_VERSION)),
    ]
}

fn schema_json<T: schemars::JsonSchema>() -> String {
    let schema = schemars::schema_for!(T);
    let mut s = serde_json::to_string_pretty(&schema).expect("schema serializes");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_is_deterministic() {
        assert_eq!(export(), export());
    }

    #[test]
    fn export_covers_the_three_wire_types_plus_version() {
        let names: Vec<_> = export().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            vec![
                "request.schema.json",
                "response.schema.json",
                "event.schema.json",
                "version.txt",
            ]
        );
    }

    #[test]
    fn request_schema_mentions_every_method_tag() {
        let json = schema_json::<Request>();
        for method in [
            "hello",
            "task_create",
            "task_list",
            "task_report",
            "task_plan",
            "task_rollback",
            "doctor_run",
            "storage_inspect",
            "storage_purge",
            "config_show",
            "memory_list",
            "model_list",
            "workspace_status",
        ] {
            assert!(json.contains(method), "schema missing method `{method}`");
        }
    }

    #[test]
    fn version_file_matches_the_constant() {
        let (_, v) = export()
            .into_iter()
            .find(|(n, _)| *n == "version.txt")
            .unwrap();
        assert_eq!(v.trim(), crate::PROTOCOL_VERSION);
    }
}
