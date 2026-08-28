//! The memory entry and its vocabulary. Pure data + the decay function;
//! persistence lives in [`crate::store`].

use serde::{Deserialize, Serialize};
use valyria_types::{MemoryId, SessionId, TaskId, Trust};

/// 30 days. After this long without being reinforced, an entry's
/// confidence has halved.
pub const DEFAULT_HALF_LIFE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Which of the four memory tiers an entry belongs to. The scope decides
/// where the entry is visible and how it is retrieved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum MemoryScope {
    /// One client session — always surfaced in the context header.
    Session(SessionId),
    /// One task — retrieved scoped to that task, recency-weighted.
    Task(TaskId),
    /// The whole workspace, persistent across tasks.
    Repository,
    /// Global across every workspace. Written explicitly only.
    User,
}

impl MemoryScope {
    pub(crate) fn kind_str(&self) -> &'static str {
        match self {
            MemoryScope::Session(_) => "session",
            MemoryScope::Task(_) => "task",
            MemoryScope::Repository => "repository",
            MemoryScope::User => "user",
        }
    }

    pub(crate) fn id_str(&self) -> Option<String> {
        match self {
            MemoryScope::Session(id) => Some(id.to_string()),
            MemoryScope::Task(id) => Some(id.to_string()),
            MemoryScope::Repository | MemoryScope::User => None,
        }
    }

    pub(crate) fn from_row(kind: &str, id: Option<&str>, row_id: &str) -> crate::Result<Self> {
        Ok(match kind {
            "session" => MemoryScope::Session(
                id.and_then(|s| s.parse().ok())
                    .ok_or(crate::MemoryError::Corrupt(row_id.to_string(), "scope_id"))?,
            ),
            "task" => MemoryScope::Task(
                id.and_then(|s| s.parse().ok())
                    .ok_or(crate::MemoryError::Corrupt(row_id.to_string(), "scope_id"))?,
            ),
            "repository" => MemoryScope::Repository,
            "user" => MemoryScope::User,
            _ => {
                return Err(crate::MemoryError::Corrupt(
                    row_id.to_string(),
                    "scope_kind",
                ))
            }
        })
    }

    /// Session memory is pinned: it is always placed in the prompt header
    /// rather than competing for a slot in ranked retrieval.
    pub fn is_pinned(&self) -> bool {
        matches!(self, MemoryScope::Session(_))
    }
}

/// Who wrote the entry — which fixes its trust level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthor {
    /// The operator stated it. `Trust::Instruction`.
    User,
    /// The runtime extracted it from what it observed. `Trust::Evidence` —
    /// it informs, it does not command.
    Agent,
}

impl MemoryAuthor {
    pub fn trust(self) -> Trust {
        match self {
            MemoryAuthor::User => Trust::Instruction,
            MemoryAuthor::Agent => Trust::Evidence,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MemoryAuthor::User => "user",
            MemoryAuthor::Agent => "agent",
        }
    }

    pub(crate) fn parse(s: &str, row_id: &str) -> crate::Result<Self> {
        match s {
            "user" => Ok(MemoryAuthor::User),
            "agent" => Ok(MemoryAuthor::Agent),
            _ => Err(crate::MemoryError::Corrupt(row_id.to_string(), "author")),
        }
    }
}

/// What kind of thing an entry records. Used for trigger-based retrieval
/// and for display; not load-bearing beyond that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A build/test/lint command observed to work.
    Command,
    /// A directory or naming convention.
    Convention,
    /// A pitfall hit before — a thing that looked right and was not.
    Pitfall,
    /// A test known to be flaky.
    FlakyTest,
    /// An architectural note.
    ArchNote,
    /// Anything else.
    Freeform,
}

impl MemoryKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Command => "command",
            MemoryKind::Convention => "convention",
            MemoryKind::Pitfall => "pitfall",
            MemoryKind::FlakyTest => "flaky_test",
            MemoryKind::ArchNote => "arch_note",
            MemoryKind::Freeform => "freeform",
        }
    }

    pub(crate) fn parse(s: &str, row_id: &str) -> crate::Result<Self> {
        Ok(match s {
            "command" => MemoryKind::Command,
            "convention" => MemoryKind::Convention,
            "pitfall" => MemoryKind::Pitfall,
            "flaky_test" => MemoryKind::FlakyTest,
            "arch_note" => MemoryKind::ArchNote,
            "freeform" => MemoryKind::Freeform,
            _ => return Err(crate::MemoryError::Corrupt(row_id.to_string(), "kind")),
        })
    }
}

/// One thing the runtime remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: MemoryId,
    pub scope: MemoryScope,
    pub author: MemoryAuthor,
    pub kind: MemoryKind,
    pub text: String,
    /// Where this came from — a task id, a tool invocation, "operator", a
    /// verification run. Carried into `Provenance` when the entry reaches
    /// context.
    pub provenance: String,
    /// Confidence at write time, `[0, 1]`. Decays; see
    /// [`MemoryEntry::effective_confidence`].
    pub confidence: f64,
    pub created_ms: i64,
    /// Last time the entry was written or reinforced. Decay is measured
    /// from here, so a repeatedly-confirmed memory never fades.
    pub last_seen_ms: i64,
    pub uses: u32,
    pub retired: bool,
    pub retired_reason: Option<String>,
}

impl MemoryEntry {
    /// A fresh agent-authored entry with `now` as both timestamps.
    pub fn agent(
        scope: MemoryScope,
        kind: MemoryKind,
        text: impl Into<String>,
        provenance: impl Into<String>,
        confidence: f64,
        now_ms: i64,
    ) -> Self {
        Self {
            id: MemoryId::new(),
            scope,
            author: MemoryAuthor::Agent,
            kind,
            text: text.into(),
            provenance: provenance.into(),
            confidence: confidence.clamp(0.0, 1.0),
            created_ms: now_ms,
            last_seen_ms: now_ms,
            uses: 0,
            retired: false,
            retired_reason: None,
        }
    }

    /// A fresh user-authored entry.
    pub fn user(
        scope: MemoryScope,
        kind: MemoryKind,
        text: impl Into<String>,
        now_ms: i64,
    ) -> Self {
        Self {
            id: MemoryId::new(),
            scope,
            author: MemoryAuthor::User,
            kind,
            text: text.into(),
            provenance: "operator".to_string(),
            confidence: 1.0,
            created_ms: now_ms,
            last_seen_ms: now_ms,
            uses: 0,
            retired: false,
            retired_reason: None,
        }
    }

    pub fn trust(&self) -> Trust {
        self.author.trust()
    }

    /// Confidence after time-decay: `confidence * 0.5^(age / half_life)`,
    /// where `age` is the silence since `last_seen_ms`. A user-authored
    /// entry does not decay — the operator's word stands until they retire
    /// it.
    pub fn effective_confidence(&self, now_ms: i64, half_life_ms: i64) -> f64 {
        if self.retired {
            return 0.0;
        }
        if self.author == MemoryAuthor::User {
            return self.confidence;
        }
        let age = (now_ms - self.last_seen_ms).max(0) as f64;
        let half_life = (half_life_ms.max(1)) as f64;
        (self.confidence * 0.5_f64.powf(age / half_life)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_fixes_trust() {
        assert_eq!(MemoryAuthor::User.trust(), Trust::Instruction);
        assert_eq!(MemoryAuthor::Agent.trust(), Trust::Evidence);
    }

    #[test]
    fn agent_confidence_halves_every_half_life() {
        let e = MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "cargo test works",
            "task_x",
            0.8,
            0,
        );
        assert!((e.effective_confidence(0, DEFAULT_HALF_LIFE_MS) - 0.8).abs() < 1e-9);
        assert!(
            (e.effective_confidence(DEFAULT_HALF_LIFE_MS, DEFAULT_HALF_LIFE_MS) - 0.4).abs() < 1e-9
        );
        assert!(
            (e.effective_confidence(2 * DEFAULT_HALF_LIFE_MS, DEFAULT_HALF_LIFE_MS) - 0.2).abs()
                < 1e-9
        );
    }

    #[test]
    fn user_memory_does_not_decay() {
        let e = MemoryEntry::user(
            MemoryScope::User,
            MemoryKind::Convention,
            "tabs, not spaces",
            0,
        );
        assert_eq!(
            e.effective_confidence(100 * DEFAULT_HALF_LIFE_MS, DEFAULT_HALF_LIFE_MS),
            1.0
        );
    }

    #[test]
    fn retired_entries_have_zero_effective_confidence() {
        let mut e = MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Pitfall,
            "x",
            "y",
            0.9,
            0,
        );
        e.retired = true;
        assert_eq!(e.effective_confidence(0, DEFAULT_HALF_LIFE_MS), 0.0);
    }

    #[test]
    fn confidence_is_clamped_on_construction() {
        let e = MemoryEntry::agent(
            MemoryScope::Repository,
            MemoryKind::Command,
            "x",
            "y",
            5.0,
            0,
        );
        assert_eq!(e.confidence, 1.0);
    }

    #[test]
    fn scope_round_trips_through_row_columns() {
        let sid = SessionId::new();
        let scope = MemoryScope::Session(sid);
        let back = MemoryScope::from_row("session", scope.id_str().as_deref(), "mem_x").unwrap();
        assert_eq!(scope, back);
        let repo = MemoryScope::from_row("repository", None, "mem_x").unwrap();
        assert_eq!(repo, MemoryScope::Repository);
    }
}
