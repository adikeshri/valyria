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

/// One exported schema file: `(relative path, contents)`. The path may
/// contain a subdirectory (`events/…`).
pub type SchemaFile = (String, String);

/// Every schema file `xtask schema` writes into `docs/protocol/`, in a
/// deterministic order. `version.txt` pins the schema to a protocol
/// version so the compat gate can tell "changed, version bumped" from
/// "changed, version not bumped". `events/<kind>.schema.json` pins each
/// event payload contract (G12).
pub fn export() -> Vec<SchemaFile> {
    let mut files = vec![
        ("request.schema.json".to_string(), schema_json::<Request>()),
        (
            "response.schema.json".to_string(),
            schema_json::<Response>(),
        ),
        ("event.schema.json".to_string(), schema_json::<WireEvent>()),
        (
            "version.txt".to_string(),
            format!("{}\n", crate::PROTOCOL_VERSION),
        ),
    ];
    files.push((
        "event-kinds.txt".to_string(),
        format!("{}\n", crate::event_payloads::EVENT_KINDS.join("\n")),
    ));
    for (kind, json) in crate::event_payloads::payload_schemas() {
        files.push((format!("events/{kind}.schema.json"), json));
    }
    files
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
    fn export_covers_the_three_wire_types_plus_version_then_event_payloads() {
        let names: Vec<String> = export().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            &names[..4],
            &[
                "request.schema.json".to_string(),
                "response.schema.json".to_string(),
                "event.schema.json".to_string(),
                "version.txt".to_string(),
            ]
        );
        assert!(names
            .iter()
            .any(|n| n == "events/context_retrieved.schema.json"));
        assert!(names
            .iter()
            .any(|n| n == "events/tool_completed.schema.json"));
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
