//! Typed, ULID-backed identifiers.
//!
//! ULIDs are lexically sortable by creation time and 128 bits, which makes
//! IDs generated under load orderable without a central counter. Every ID
//! type is a distinct Rust type (never a bare `String` or `Ulid`) so a
//! `TaskId` can never be passed where a `StepId` is expected — the compiler
//! catches the mixup that a stringly-typed system would push to runtime.
//!
//! Display format is `<prefix>_<ulid>` (e.g. `task_01ARZ3NDEKTSV4RRFFQ69G5FAV`),
//! matching the convention called out in the build plan's cross-cutting
//! conventions section, and round-trips through `FromStr`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdParseError {
    #[error("expected id with prefix `{expected}_`, got `{got}`")]
    BadPrefix { expected: &'static str, got: String },
    #[error("invalid ulid in id `{0}`")]
    BadUlid(String),
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(ulid::Ulid);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// Generate a new, time-sortable id.
            pub fn new() -> Self {
                Self(ulid::Ulid::new())
            }

            pub fn from_ulid(u: ulid::Ulid) -> Self {
                Self(u)
            }

            pub fn ulid(&self) -> ulid::Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", Self::PREFIX, self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let rest = s
                    .strip_prefix(Self::PREFIX)
                    .and_then(|r| r.strip_prefix('_'))
                    .ok_or_else(|| IdParseError::BadPrefix {
                        expected: Self::PREFIX,
                        got: s.to_string(),
                    })?;
                let ulid = ulid::Ulid::from_string(rest)
                    .map_err(|_| IdParseError::BadUlid(s.to_string()))?;
                Ok(Self(ulid))
            }
        }

        // Manual impls (rather than `#[serde(transparent)]`) so the wire
        // format is the prefixed display string (`ws_01ARZ...`), not the
        // bare ulid — otherwise the type-safety this module exists for
        // would vanish the moment an id crosses the protocol boundary.
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

typed_id!(
    /// Identifies one task: the durable unit of agent work (§9).
    TaskId,
    "task"
);
typed_id!(
    /// Identifies one step within a task's plan or execution journal.
    StepId,
    "step"
);
typed_id!(
    /// Identifies one recorded tool invocation (§18).
    ToolInvocationId,
    "tinv"
);
typed_id!(
    /// Identifies one event in the durable event journal (§43).
    EventId,
    "evt"
);
typed_id!(
    /// Identifies one opened workspace (a repository checkout).
    WorkspaceId,
    "ws"
);
typed_id!(
    /// Identifies one client session against the runtime.
    SessionId,
    "sess"
);
typed_id!(
    /// Identifies one plan revision (§10).
    PlanId,
    "plan"
);
typed_id!(
    /// Identifies one rollback checkpoint.
    CheckpointId,
    "ckpt"
);
typed_id!(
    /// Identifies one memory entry (§32).
    MemoryId,
    "mem"
);
typed_id!(
    /// Identifies one permission approval request/response pair (§22).
    ApprovalId,
    "appr"
);
typed_id!(
    /// Identifies one verification run (§27).
    VerificationRunId,
    "vrun"
);
typed_id!(
    /// Identifies one loaded model runtime instance (§35).
    ModelInstanceId,
    "minst"
);
typed_id!(
    /// Identifies one ledger entry (§26).
    LedgerEntryId,
    "ledg"
);
typed_id!(
    /// Identifies one assembled context snapshot (§11).
    ContextSnapshotId,
    "ctx"
);
typed_id!(
    /// Identifies one effect issued by the agent step driver (§7 D1: the
    /// journal records `EffectIssued`/`EffectCompleted` entries keyed by
    /// this id, so a resumed task can tell which in-flight effect a
    /// completion entry corresponds to).
    EffectId,
    "eff"
);

/// A monotonically increasing index generation (D8). Not ULID-backed: it is
/// a per-workspace counter, not a globally unique identifier, and ordering
/// by plain integer comparison is exactly the property callers need when
/// checking "did the index move since I planned against it?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Generation(pub u64);

impl Generation {
    pub const INITIAL: Generation = Generation(0);

    pub fn next(self) -> Generation {
        Generation(self.0 + 1)
    }
}

/// `INITIAL`, i.e. "nothing has been indexed yet" — the honest default for
/// a struct that carries a generation before one exists.
impl Default for Generation {
    fn default() -> Self {
        Generation::INITIAL
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gen{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_round_trip() {
        let id = TaskId::new();
        let s = id.to_string();
        assert!(s.starts_with("task_"));
        let parsed: TaskId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn rejects_wrong_prefix() {
        let step_str = StepId::new().to_string();
        let err = step_str.parse::<TaskId>().unwrap_err();
        assert!(matches!(err, IdParseError::BadPrefix { .. }));
    }

    #[test]
    fn rejects_garbage_ulid() {
        let err = "task_not-a-ulid".parse::<TaskId>().unwrap_err();
        assert!(matches!(err, IdParseError::BadUlid(_)));
    }

    #[test]
    fn distinct_id_types_do_not_collide_by_construction() {
        // This is a compile-time property (TaskId and StepId are different
        // types), exercised here just to document the intent.
        let t = TaskId::new();
        let s = StepId::new();
        assert_ne!(t.to_string(), s.to_string());
    }

    #[test]
    fn ids_are_time_sortable() {
        let a = TaskId::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = TaskId::new();
        assert!(a < b, "later-created id should sort greater");
    }

    #[test]
    fn generation_ordering() {
        let g0 = Generation::INITIAL;
        let g1 = g0.next();
        assert!(g1 > g0);
        assert_eq!(g1, Generation(1));
    }

    #[test]
    fn serde_round_trip_is_transparent_string() {
        let id = WorkspaceId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id));
        let back: WorkspaceId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
