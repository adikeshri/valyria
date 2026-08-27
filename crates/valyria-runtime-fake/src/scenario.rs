//! D12's scenario format: a fixed, turn-by-turn script the fake model plays
//! back. Designed for genuine reuse across the agent test suite ("nearly
//! all agent tests run against it"), not a one-off demo hack — the
//! `Malformed` variant exists for a later malformed-input corpus even
//! though the Phase 3 walking-skeleton scenario never uses it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{FakeRuntimeError, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub turns: Vec<ScriptedTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScriptedTurn {
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    Finish {
        summary: String,
    },
    Ask {
        question: String,
    },
    /// Reserved for a future malformed-output test corpus (D12); no Phase 3
    /// scenario emits this yet.
    Malformed {
        raw: String,
    },
}

impl Scenario {
    pub fn load_toml(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|source| FakeRuntimeError::Io {
            path: path.display().to_string(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| FakeRuntimeError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// The default demo/test scenario for Phase 3's walking skeleton: read a
    /// file, edit it (add a function), run a safe, auto-allowed command,
    /// finish. Paired with the fixture repo built by the walking-skeleton
    /// integration test (`crates/valyria-cli/tests/walking_skeleton.rs`) —
    /// the `edit_file` turn's `exact_replacement` anchor must match that
    /// fixture's `src/lib.rs` verbatim.
    pub fn default_walking_skeleton() -> Self {
        toml::from_str(include_str!("../scenarios/walking_skeleton.toml"))
            .expect("bundled default scenario must parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scenario_has_at_least_four_turns() {
        let scenario = Scenario::default_walking_skeleton();
        assert!(scenario.turns.len() >= 4, "{:?}", scenario.turns);
    }

    #[test]
    fn default_scenario_ends_with_finish() {
        let scenario = Scenario::default_walking_skeleton();
        assert!(matches!(
            scenario.turns.last(),
            Some(ScriptedTurn::Finish { .. })
        ));
    }

    #[test]
    fn default_scenario_reads_edits_and_runs() {
        let scenario = Scenario::default_walking_skeleton();
        let names: Vec<&str> = scenario
            .turns
            .iter()
            .filter_map(|t| match t {
                ScriptedTurn::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"run_command"));
    }

    #[test]
    fn toml_round_trip() {
        let scenario = Scenario {
            name: "t".into(),
            turns: vec![
                ScriptedTurn::ToolCall {
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                },
                ScriptedTurn::Ask {
                    question: "ok?".into(),
                },
                ScriptedTurn::Finish {
                    summary: "done".into(),
                },
            ],
        };
        let toml_str = toml::to_string(&scenario).unwrap();
        let back: Scenario = toml::from_str(&toml_str).unwrap();
        assert_eq!(scenario, back);
    }

    #[test]
    fn load_toml_missing_file_errors() {
        let err = Scenario::load_toml(Path::new("/nonexistent/scenario.toml")).unwrap_err();
        assert!(matches!(err, FakeRuntimeError::Io { .. }));
    }
}
