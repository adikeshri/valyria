//! Multi-agent as **roles over the same machinery** (§4.25).
//!
//! Researcher / Planner / Implementer / Tester / Reviewer differ only in
//! their tool allowlist, whether they may write, their permission ceiling,
//! and (later) their model binding. They communicate exclusively through
//! the typed [`Artifact`]s persisted in the task store — never by handing
//! each other raw conversation.
//!
//! Phase 8 ships the role definitions and artifact types; spawning a role
//! as its own child task is a documented follow-up. Every role maps to the
//! single bound model role until a real split exists.

use serde::{Deserialize, Serialize};
use valyria_types::{PermissionMode, TaskId, Timestamp};

use crate::model::PlanRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Researcher,
    Planner,
    Implementer,
    Tester,
    Reviewer,
}

impl AgentRole {
    pub const ALL: [AgentRole; 5] = [
        AgentRole::Researcher,
        AgentRole::Planner,
        AgentRole::Implementer,
        AgentRole::Tester,
        AgentRole::Reviewer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Researcher => "researcher",
            AgentRole::Planner => "planner",
            AgentRole::Implementer => "implementer",
            AgentRole::Tester => "tester",
            AgentRole::Reviewer => "reviewer",
        }
    }

    /// The tools this role may invoke. Read-only roles never get an
    /// editing or command tool.
    pub fn tool_allowlist(self) -> &'static [&'static str] {
        const READ: &[&str] = &["read_file", "list_dir", "search", "symbol_search", "grep"];
        const READ_RUN: &[&str] = &[
            "read_file",
            "list_dir",
            "search",
            "symbol_search",
            "grep",
            "run_command",
        ];
        const WRITE: &[&str] = &[
            "read_file",
            "list_dir",
            "search",
            "symbol_search",
            "grep",
            "run_command",
            "edit_file",
            "write_file",
            "delete_file",
            "create_file",
        ];
        match self {
            AgentRole::Researcher | AgentRole::Reviewer => READ,
            AgentRole::Planner => READ,
            AgentRole::Tester => READ_RUN,
            AgentRole::Implementer => WRITE,
        }
    }

    /// Whether this role may modify files. Researcher, Planner, Tester and
    /// Reviewer may not — enforced by the allowlist, asserted in tests.
    pub fn can_write(self) -> bool {
        matches!(self, AgentRole::Implementer)
    }

    /// The most permissive permission mode this role may run under. A
    /// read-only role is capped at `Manual` (it should never be
    /// auto-allowed to do anything surprising); the Implementer inherits
    /// the task's mode.
    pub fn permission_ceiling(self) -> PermissionMode {
        if self.can_write() {
            PermissionMode::Autonomous
        } else {
            PermissionMode::Manual
        }
    }

    /// The artifact this role is responsible for producing.
    pub fn produces(self) -> ArtifactKind {
        match self {
            AgentRole::Researcher => ArtifactKind::ResearchBrief,
            AgentRole::Planner => ArtifactKind::Plan,
            AgentRole::Implementer => ArtifactKind::ChangeSet,
            AgentRole::Tester => ArtifactKind::VerificationReport,
            AgentRole::Reviewer => ArtifactKind::ReviewFindings,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    ResearchBrief,
    Plan,
    ChangeSet,
    VerificationReport,
    ReviewFindings,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::ResearchBrief => "research_brief",
            ArtifactKind::Plan => "plan",
            ArtifactKind::ChangeSet => "change_set",
            ArtifactKind::VerificationReport => "verification_report",
            ArtifactKind::ReviewFindings => "review_findings",
        }
    }
}

/// What one role hands to the next. The only inter-role channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Artifact {
    ResearchBrief {
        summary: String,
        relevant_files: Vec<String>,
        open_questions: Vec<String>,
    },
    Plan {
        revision: PlanRevision,
    },
    ChangeSet {
        summary: String,
        files_changed: Vec<String>,
        ledger_entries: usize,
    },
    VerificationReport {
        passed: bool,
        commands_run: Vec<String>,
        failures: Vec<String>,
    },
    ReviewFindings {
        approved: bool,
        findings: Vec<String>,
    },
}

impl Artifact {
    pub fn kind(&self) -> ArtifactKind {
        match self {
            Artifact::ResearchBrief { .. } => ArtifactKind::ResearchBrief,
            Artifact::Plan { .. } => ArtifactKind::Plan,
            Artifact::ChangeSet { .. } => ArtifactKind::ChangeSet,
            Artifact::VerificationReport { .. } => ArtifactKind::VerificationReport,
            Artifact::ReviewFindings { .. } => ArtifactKind::ReviewFindings,
        }
    }
}

/// An artifact as stored against a task, with provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredArtifact {
    pub task_id: TaskId,
    pub produced_by: AgentRole,
    pub artifact: Artifact,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_roles_cannot_write() {
        for role in AgentRole::ALL {
            let writes = ["edit_file", "write_file", "delete_file", "create_file"];
            let has_write_tool = role.tool_allowlist().iter().any(|t| writes.contains(t));
            assert_eq!(
                has_write_tool,
                role.can_write(),
                "{} allowlist / can_write disagree",
                role.as_str()
            );
        }
        assert!(!AgentRole::Researcher.can_write());
        assert!(!AgentRole::Planner.can_write());
        assert!(!AgentRole::Tester.can_write());
        assert!(!AgentRole::Reviewer.can_write());
        assert!(AgentRole::Implementer.can_write());
    }

    #[test]
    fn only_the_tester_and_implementer_may_run_commands() {
        assert!(AgentRole::Tester.tool_allowlist().contains(&"run_command"));
        assert!(AgentRole::Implementer
            .tool_allowlist()
            .contains(&"run_command"));
        assert!(!AgentRole::Researcher
            .tool_allowlist()
            .contains(&"run_command"));
        assert!(!AgentRole::Reviewer
            .tool_allowlist()
            .contains(&"run_command"));
    }

    #[test]
    fn permission_ceiling_is_tighter_for_read_only_roles() {
        assert_eq!(
            AgentRole::Reviewer.permission_ceiling(),
            PermissionMode::Manual
        );
        assert_eq!(
            AgentRole::Implementer.permission_ceiling(),
            PermissionMode::Autonomous
        );
    }

    #[test]
    fn each_role_owns_a_distinct_artifact_kind() {
        let kinds: Vec<ArtifactKind> = AgentRole::ALL.iter().map(|r| r.produces()).collect();
        for (i, k) in kinds.iter().enumerate() {
            assert!(
                !kinds[i + 1..].contains(k),
                "artifact kind {} produced by two roles",
                k.as_str()
            );
        }
    }

    #[test]
    fn artifact_serde_round_trip() {
        let a = Artifact::VerificationReport {
            passed: false,
            commands_run: vec!["cargo test".into()],
            failures: vec!["tests::x failed".into()],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        assert_eq!(back.kind(), ArtifactKind::VerificationReport);
    }
}
