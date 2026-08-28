//! Turning a [`ValidatedPlan`] into an execution order (§4.25:
//! "dependency/parallelism").
//!
//! The schedule is a list of **waves** (dependency layers). Within a wave,
//! steps the model marked `parallelizable` are grouped so a later change
//! can run them concurrently; Phase 8 still executes everything one step at
//! a time, but the grouping is computed and tested now so that change is
//! local.
//!
//! [`Schedule::next_incomplete`] is a pure function of the plan and the
//! durable set of completed step ids — which is what makes resuming a
//! half-run plan after a process restart a lookup, not a reconstruction.

use std::collections::BTreeSet;

use crate::model::{Plan, PlanStep, PlanStepId};
use crate::validate::ValidatedPlan;

/// A parallel group within a wave: one entry for a solo (non-parallelizable)
/// step, or several ids that may run together.
pub type Group = Vec<PlanStepId>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    plan: Plan,
    /// `waves[i]` is a list of groups; each group is one or more step ids.
    waves: Vec<Vec<Group>>,
    /// Flattened deterministic order: wave order, then declared order.
    order: Vec<PlanStepId>,
}

impl Schedule {
    pub fn waves(&self) -> &[Vec<Group>] {
        &self.waves
    }

    /// Every step id in deterministic execution order.
    pub fn order(&self) -> &[PlanStepId] {
        &self.order
    }

    pub fn step(&self, id: &PlanStepId) -> Option<&PlanStep> {
        self.plan.step(id)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The next step to run given the steps already finished — the first
    /// step in [`Schedule::order`] not in `done`. `None` means the plan is
    /// fully executed.
    pub fn next_incomplete(&self, done: &BTreeSet<PlanStepId>) -> Option<&PlanStep> {
        self.order
            .iter()
            .find(|id| !done.contains(*id))
            .and_then(|id| self.plan.step(id))
    }

    /// How many steps remain given `done`.
    pub fn remaining(&self, done: &BTreeSet<PlanStepId>) -> usize {
        self.order.iter().filter(|id| !done.contains(*id)).count()
    }
}

/// Build the schedule from a validated plan (whose waves are already
/// computed and acyclic).
pub fn schedule(validated: &ValidatedPlan) -> Schedule {
    let plan = validated.plan().clone();
    let mut waves: Vec<Vec<Group>> = Vec::new();
    let mut order: Vec<PlanStepId> = Vec::new();

    for wave_ids in validated.waves() {
        let mut groups: Vec<Group> = Vec::new();
        let mut parallel_bucket: Group = Vec::new();
        for id in wave_ids {
            order.push(id.clone());
            let parallelizable = plan.step(id).map(|s| s.parallelizable).unwrap_or(false);
            if parallelizable {
                parallel_bucket.push(id.clone());
            } else {
                groups.push(vec![id.clone()]);
            }
        }
        if !parallel_bucket.is_empty() {
            groups.push(parallel_bucket);
        }
        waves.push(groups);
    }

    Schedule { plan, waves, order }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EstimatedScope, VerificationRequirement};
    use crate::validate::{validate, PlanContext};
    use valyria_types::PermissionMode;
    use valyria_vfs::WorkspaceRoot;

    fn step(id: &str, deps: &[&str], parallelizable: bool) -> PlanStep {
        PlanStep {
            id: PlanStepId::new(id).unwrap(),
            intent: format!("do {id}"),
            targets: vec![],
            depends_on: deps.iter().map(|d| PlanStepId::new(*d).unwrap()).collect(),
            parallelizable,
            checkpoint: false,
            verification: VerificationRequirement::None,
            rollback_boundary: false,
            approval_required: false,
            estimated_scope: EstimatedScope::default(),
        }
    }

    fn validated(steps: Vec<PlanStep>) -> ValidatedPlan {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "\n");
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let cx = PlanContext {
            workspace_root: &root,
            permission_mode: PermissionMode::Assisted,
            allowed_write_roots: vec![root.as_path().to_path_buf()],
        };
        let plan = Plan {
            plan_scope: vec![],
            steps,
        };
        validate(&plan, &cx).unwrap()
    }

    fn id(s: &str) -> PlanStepId {
        PlanStepId::new(s).unwrap()
    }

    #[test]
    fn diamond_dag_produces_three_waves() {
        // a -> {b, c} -> d
        let s = schedule(&validated(vec![
            step("a", &[], false),
            step("b", &["a"], true),
            step("c", &["a"], true),
            step("d", &["b", "c"], false),
        ]));
        assert_eq!(s.order(), &[id("a"), id("b"), id("c"), id("d")]);
        assert_eq!(s.waves().len(), 3);
        // middle wave groups b and c into one parallel group.
        assert_eq!(s.waves()[1], vec![vec![id("b"), id("c")]]);
        // last wave: d solo.
        assert_eq!(s.waves()[2], vec![vec![id("d")]]);
    }

    #[test]
    fn non_parallelizable_steps_in_a_wave_are_separate_groups() {
        let s = schedule(&validated(vec![
            step("a", &[], false),
            step("b", &[], false),
        ]));
        assert_eq!(s.waves()[0], vec![vec![id("a")], vec![id("b")]]);
    }

    #[test]
    fn next_incomplete_walks_the_order_and_resumes_from_done() {
        let s = schedule(&validated(vec![
            step("a", &[], false),
            step("b", &["a"], false),
            step("c", &["b"], false),
        ]));
        let mut done = BTreeSet::new();
        assert_eq!(s.next_incomplete(&done).unwrap().id, id("a"));
        done.insert(id("a"));
        assert_eq!(s.next_incomplete(&done).unwrap().id, id("b"));
        done.insert(id("b"));
        assert_eq!(s.next_incomplete(&done).unwrap().id, id("c"));
        assert_eq!(s.remaining(&done), 1);
        done.insert(id("c"));
        assert!(s.next_incomplete(&done).is_none());
        assert_eq!(s.remaining(&done), 0);
    }

    #[test]
    fn next_incomplete_is_stable_even_if_done_is_out_of_order() {
        // A resume where step b somehow finished but a didn't still picks a.
        let s = schedule(&validated(vec![
            step("a", &[], false),
            step("b", &[], false),
        ]));
        let done: BTreeSet<PlanStepId> = [id("b")].into_iter().collect();
        assert_eq!(s.next_incomplete(&done).unwrap().id, id("a"));
    }
}
