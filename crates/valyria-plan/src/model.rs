//! The plan model (§4.25): a DAG of [`PlanStep`]s the model proposes and
//! the runtime validates, executes and — as a *living document* — revises.
//!
//! Step ids are **model-authored strings**, not ULIDs: they must be stable
//! and referenceable across revisions ("step `add_tests` now depends on
//! `refactor_api`"), so [`PlanStepId`] is a validated newtype over the
//! author's own name rather than something the runtime mints.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use valyria_types::Timestamp;
use valyria_util::ContentHash;

use crate::error::PlanFormatError;

/// A human/model-authored step identifier: non-empty, `[a-z0-9_-]`, at
/// most 64 chars. Kept deliberately narrow so it is safe to interpolate
/// into prompts, journal payloads and table keys without escaping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlanStepId(String);

impl PlanStepId {
    pub fn new(raw: impl Into<String>) -> Result<Self, PlanFormatError> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > 64 {
            return Err(PlanFormatError::StepId {
                got: raw,
                why: "must be 1-64 characters",
            });
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err(PlanFormatError::StepId {
                got: raw,
                why: "only [a-z0-9_-] allowed",
            });
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PlanStepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PlanStepId {
    type Error = PlanFormatError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PlanStepId> for String {
    fn from(value: PlanStepId) -> Self {
        value.0
    }
}

/// How a step's mutation is to be checked before the step counts as done
/// (§4.25: "verification attached to every mutating step").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VerificationRequirement {
    /// No check — only legal for a non-mutating step (validation enforces).
    #[default]
    None,
    /// Run the repository's discovered escalation strategy for this step's
    /// change set — the Phase 7 machinery.
    Inherit,
    /// Run this exact command (relative to the workspace root).
    Command { command: String },
}

impl VerificationRequirement {
    pub fn is_none(&self) -> bool {
        matches!(self, VerificationRequirement::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    #[default]
    Low,
    Medium,
    High,
}

/// The model's own estimate of a step's blast radius, used for approval
/// gating and (later) scheduling heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EstimatedScope {
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub risk: Risk,
}

/// One node in the plan DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: PlanStepId,
    pub intent: String,
    #[serde(default)]
    pub targets: Vec<PathBuf>,
    #[serde(default)]
    pub depends_on: Vec<PlanStepId>,
    #[serde(default)]
    pub parallelizable: bool,
    #[serde(default)]
    pub checkpoint: bool,
    #[serde(default)]
    pub verification: VerificationRequirement,
    #[serde(default)]
    pub rollback_boundary: bool,
    #[serde(default)]
    pub approval_required: bool,
    #[serde(default)]
    pub estimated_scope: EstimatedScope,
}

impl PlanStep {
    /// A step is *mutating* if it declares file targets — the signal
    /// validation uses to require a verification and a checkpoint pairing.
    pub fn is_mutating(&self) -> bool {
        !self.targets.is_empty()
    }
}

/// A plan as proposed by the model. `plan_scope` is the set of path
/// prefixes the plan promises to stay within — it drives permission
/// auto-allow (§4.9) and every step target is validated against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub plan_scope: Vec<String>,
    pub steps: Vec<PlanStep>,
}

impl Plan {
    /// A content hash over the plan's canonical JSON. Stable across
    /// field/key ordering because `serde_json::to_vec` on these structs is
    /// deterministic and the struct field order is fixed.
    pub fn content_hash(&self) -> ContentHash {
        let canonical = serde_json::to_vec(self).expect("Plan serializes");
        ContentHash::of_bytes(&canonical)
    }

    pub fn step(&self, id: &PlanStepId) -> Option<&PlanStep> {
        self.steps.iter().find(|s| &s.id == id)
    }

    pub fn step_ids(&self) -> BTreeSet<PlanStepId> {
        self.steps.iter().map(|s| s.id.clone()).collect()
    }

    /// Structural diff against a prior revision of the same plan — the
    /// "each revision journaled and diffable" requirement (§4.25).
    pub fn diff(&self, prior: &Plan) -> PlanDiff {
        let now = self.step_ids();
        let before = prior.step_ids();
        let added: Vec<PlanStepId> = now.difference(&before).cloned().collect();
        let removed: Vec<PlanStepId> = before.difference(&now).cloned().collect();
        let changed: Vec<PlanStepId> = now
            .intersection(&before)
            .filter(|id| self.step(id) != prior.step(id))
            .cloned()
            .collect();
        PlanDiff {
            added,
            removed,
            changed,
        }
    }
}

/// The result of [`Plan::diff`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanDiff {
    pub added: Vec<PlanStepId>,
    pub removed: Vec<PlanStepId>,
    pub changed: Vec<PlanStepId>,
}

impl PlanDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// One persisted revision of a task's plan. Plans are living documents:
/// each accepted revision is stored, keyed by an incrementing number, with
/// the hash of its parent so the chain is auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevision {
    pub revision: u32,
    /// `content_hash` of the previous revision's plan, or `None` for the
    /// first.
    pub parent_hash: Option<String>,
    pub rationale: String,
    pub plan: Plan,
    pub created_at: Timestamp,
}

impl PlanRevision {
    pub fn first(plan: Plan, rationale: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            revision: 1,
            parent_hash: None,
            rationale: rationale.into(),
            plan,
            created_at,
        }
    }

    /// Build the next revision on top of `self`.
    pub fn revise(&self, plan: Plan, rationale: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            revision: self.revision + 1,
            parent_hash: Some(self.plan.content_hash().to_hex()),
            rationale: rationale.into(),
            plan,
            created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, deps: &[&str]) -> PlanStep {
        PlanStep {
            id: PlanStepId::new(id).unwrap(),
            intent: format!("do {id}"),
            targets: vec![],
            depends_on: deps.iter().map(|d| PlanStepId::new(*d).unwrap()).collect(),
            parallelizable: false,
            checkpoint: false,
            verification: VerificationRequirement::None,
            rollback_boundary: false,
            approval_required: false,
            estimated_scope: EstimatedScope::default(),
        }
    }

    #[test]
    fn step_id_rejects_bad_shapes() {
        assert!(PlanStepId::new("").is_err());
        assert!(PlanStepId::new("Has Caps").is_err());
        assert!(PlanStepId::new("space bar").is_err());
        assert!(PlanStepId::new("x".repeat(65)).is_err());
        assert!(PlanStepId::new("ok_step-2").is_ok());
    }

    #[test]
    fn plan_serde_round_trip() {
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![step("a", &[]), step("b", &["a"])],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn content_hash_is_order_stable_for_equal_plans() {
        let a = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![step("a", &[]), step("b", &["a"])],
        };
        let b = a.clone();
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn content_hash_changes_when_a_step_changes() {
        let a = Plan {
            plan_scope: vec![],
            steps: vec![step("a", &[])],
        };
        let mut b = a.clone();
        b.steps[0].intent = "something else".into();
        assert_ne!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn diff_reports_added_removed_changed() {
        let base = Plan {
            plan_scope: vec![],
            steps: vec![step("a", &[]), step("b", &[]), step("c", &[])],
        };
        let mut next = base.clone();
        next.steps.remove(2); // drop c
        next.steps.push(step("d", &[])); // add d
        next.steps[1].intent = "changed b".into(); // change b
        let diff = next.diff(&base);
        assert_eq!(diff.added, vec![PlanStepId::new("d").unwrap()]);
        assert_eq!(diff.removed, vec![PlanStepId::new("c").unwrap()]);
        assert_eq!(diff.changed, vec![PlanStepId::new("b").unwrap()]);
        assert!(!diff.is_empty());
    }

    #[test]
    fn revision_chain_links_parent_hash() {
        let p1 = Plan {
            plan_scope: vec![],
            steps: vec![step("a", &[])],
        };
        let r1 = PlanRevision::first(p1.clone(), "initial", Timestamp::from_millis(1));
        let mut p2 = p1.clone();
        p2.steps.push(step("b", &["a"]));
        let r2 = r1.revise(p2, "add b", Timestamp::from_millis(2));
        assert_eq!(r2.revision, 2);
        assert_eq!(r2.parent_hash, Some(p1.content_hash().to_hex()));
    }
}
