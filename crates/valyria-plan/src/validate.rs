//! Plan validation (§4.25): "the model proposes; the runtime **validates**".
//!
//! Every check is a distinct [`PlanErrorCode`] so a rejection is
//! machine-readable — the repair loop hands the model a structured list of
//! codes + hints, not a prose paragraph. Validation collects *all* the
//! problems in one pass rather than failing on the first.
//!
//! Target resolution is against the **workspace filesystem** (does the path
//! resolve safely under the root), not the repository index — wiring the
//! index into the live agent loop is still a separate follow-up. A plan
//! whose targets are all resolvable and in-scope passes here; the index
//! check is additive and can be layered on without changing this contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use valyria_types::PermissionMode;
use valyria_vfs::WorkspaceRoot;

use crate::model::{Plan, PlanStepId};

/// A single semantic problem with a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError {
    pub code: PlanErrorCode,
    /// The offending step, when the problem is step-local.
    pub step: Option<PlanStepId>,
    pub message: String,
    /// A concrete, actionable hint the model can act on.
    pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanErrorCode {
    EmptyPlan,
    /// The model's response was not a `submit_plan` action at all
    /// (driver-produced, never by [`validate`]).
    NotSubmitted,
    /// The plan payload did not deserialize into a [`Plan`]
    /// (driver-produced, never by [`validate`]).
    Malformed,
    DuplicateStepId,
    UnknownDependency,
    CyclicDependency,
    MutatingStepWithoutVerification,
    RollbackBoundaryWithoutCheckpoint,
    TargetOutsidePlanScope,
    TargetUnresolvable,
    PlanScopeOutsideProfile,
}

impl PlanErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanErrorCode::EmptyPlan => "empty_plan",
            PlanErrorCode::NotSubmitted => "plan_not_submitted",
            PlanErrorCode::Malformed => "plan_malformed",
            PlanErrorCode::DuplicateStepId => "duplicate_step_id",
            PlanErrorCode::UnknownDependency => "unknown_dependency",
            PlanErrorCode::CyclicDependency => "cyclic_dependency",
            PlanErrorCode::MutatingStepWithoutVerification => "mutating_step_without_verification",
            PlanErrorCode::RollbackBoundaryWithoutCheckpoint => {
                "rollback_boundary_without_checkpoint"
            }
            PlanErrorCode::TargetOutsidePlanScope => "target_outside_plan_scope",
            PlanErrorCode::TargetUnresolvable => "target_unresolvable",
            PlanErrorCode::PlanScopeOutsideProfile => "plan_scope_outside_profile",
        }
    }
}

/// Everything validation needs to know about the world outside the plan.
pub struct PlanContext<'a> {
    pub workspace_root: &'a WorkspaceRoot,
    pub permission_mode: PermissionMode,
    /// Absolute path prefixes writes are permitted under (the workspace
    /// sandbox's allow-write roots). A `plan_scope` entry must resolve
    /// under one of these.
    pub allowed_write_roots: Vec<PathBuf>,
}

impl std::fmt::Debug for PlanContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanContext")
            .field("permission_mode", &self.permission_mode)
            .field("allowed_write_roots", &self.allowed_write_roots)
            .finish()
    }
}

/// A plan that has passed every check in [`validate`]. The topological
/// `waves` are computed once, here, and carried so [`crate::schedule`] and
/// the driver never re-derive them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    plan: Plan,
    waves: Vec<Vec<PlanStepId>>,
}

impl ValidatedPlan {
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    pub fn into_plan(self) -> Plan {
        self.plan
    }
    /// Steps grouped into dependency layers: every step in wave *n* depends
    /// only on steps in waves `< n`.
    pub fn waves(&self) -> &[Vec<PlanStepId>] {
        &self.waves
    }
    pub fn step_count(&self) -> usize {
        self.plan.steps.len()
    }
}

/// Validate `plan` against `cx`. `Ok` is a [`ValidatedPlan`]; `Err` is
/// every problem found, so the model can fix them all in one revision.
pub fn validate(plan: &Plan, cx: &PlanContext) -> Result<ValidatedPlan, Vec<PlanError>> {
    let mut errors = Vec::new();

    if plan.steps.is_empty() {
        errors.push(PlanError {
            code: PlanErrorCode::EmptyPlan,
            step: None,
            message: "the plan has no steps".into(),
            hint: "produce at least one step, even for a trivial task".into(),
        });
        return Err(errors);
    }

    // --- unique ids ---------------------------------------------------
    let mut seen: BTreeSet<&PlanStepId> = BTreeSet::new();
    for step in &plan.steps {
        if !seen.insert(&step.id) {
            errors.push(PlanError {
                code: PlanErrorCode::DuplicateStepId,
                step: Some(step.id.clone()),
                message: format!("step id `{}` is used more than once", step.id),
                hint: "give every step a unique id".into(),
            });
        }
    }
    let ids: BTreeSet<PlanStepId> = plan.step_ids();

    // --- dependency references + acyclicity -------------------------
    for step in &plan.steps {
        for dep in &step.depends_on {
            if !ids.contains(dep) {
                errors.push(PlanError {
                    code: PlanErrorCode::UnknownDependency,
                    step: Some(step.id.clone()),
                    message: format!("step `{}` depends on unknown step `{}`", step.id, dep),
                    hint: "every `depends_on` entry must name another step's id".into(),
                });
            }
        }
    }
    if let Some(cycle) = find_cycle(plan) {
        let rendered = cycle
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(" -> ");
        errors.push(PlanError {
            code: PlanErrorCode::CyclicDependency,
            step: cycle.first().cloned(),
            message: format!("dependency cycle: {rendered}"),
            hint: "break the cycle — a plan must be a DAG".into(),
        });
    }

    // --- per-step mutation invariants ------------------------------
    for step in &plan.steps {
        if step.is_mutating() && step.verification.is_none() {
            errors.push(PlanError {
                code: PlanErrorCode::MutatingStepWithoutVerification,
                step: Some(step.id.clone()),
                message: format!("step `{}` changes files but has no verification", step.id),
                hint: "set `verification` to `inherit` or a concrete command on every \
                       step that has `targets`"
                    .into(),
            });
        }
        if step.rollback_boundary && !step.checkpoint {
            errors.push(PlanError {
                code: PlanErrorCode::RollbackBoundaryWithoutCheckpoint,
                step: Some(step.id.clone()),
                message: format!(
                    "step `{}` is a rollback boundary but takes no checkpoint",
                    step.id
                ),
                hint: "a rollback boundary needs `checkpoint: true` so there is a \
                       state to roll back to"
                    .into(),
            });
        }
    }

    // --- scope: targets in plan_scope, plan_scope in profile ------
    validate_scope(plan, cx, &mut errors);

    if errors.is_empty() {
        Ok(ValidatedPlan {
            plan: plan.clone(),
            waves: topological_waves(plan).expect("acyclic: checked above"),
        })
    } else {
        Err(errors)
    }
}

fn validate_scope(plan: &Plan, cx: &PlanContext, errors: &mut Vec<PlanError>) {
    // plan_scope entries must resolve inside the workspace and under an
    // allowed write root.
    for raw in &plan.plan_scope {
        match cx.workspace_root.resolve(raw.trim_end_matches('/')) {
            Ok(resolved) => {
                let ok = cx.allowed_write_roots.is_empty()
                    || cx
                        .allowed_write_roots
                        .iter()
                        .any(|root| resolved.starts_with(root));
                if !ok {
                    errors.push(PlanError {
                        code: PlanErrorCode::PlanScopeOutsideProfile,
                        step: None,
                        message: format!(
                            "plan_scope entry `{raw}` is outside the permitted write roots"
                        ),
                        hint: "narrow plan_scope to paths the permission profile allows \
                               writing"
                            .into(),
                    });
                }
            }
            Err(_) => errors.push(PlanError {
                code: PlanErrorCode::PlanScopeOutsideProfile,
                step: None,
                message: format!("plan_scope entry `{raw}` does not resolve inside the workspace"),
                hint: "plan_scope entries are workspace-relative path prefixes".into(),
            }),
        }
    }

    // In Manual mode a plan may not carry an auto-scope at all — every
    // write must be asked for individually.
    if cx.permission_mode == PermissionMode::Manual && !plan.plan_scope.is_empty() {
        errors.push(PlanError {
            code: PlanErrorCode::PlanScopeOutsideProfile,
            step: None,
            message: "plan_scope is not honoured in Manual permission mode".into(),
            hint: "leave plan_scope empty in Manual mode; each write is approved individually"
                .into(),
        });
    }

    let scope_prefixes: Vec<String> = plan
        .plan_scope
        .iter()
        .map(|s| {
            let t = s.trim_end_matches('/');
            if t.is_empty() {
                String::new()
            } else {
                format!("{t}/")
            }
        })
        .collect();

    for step in &plan.steps {
        for target in &step.targets {
            if cx.workspace_root.resolve(target).is_err() {
                errors.push(PlanError {
                    code: PlanErrorCode::TargetUnresolvable,
                    step: Some(step.id.clone()),
                    message: format!(
                        "target `{}` in step `{}` does not resolve safely under the workspace root",
                        target.display(),
                        step.id
                    ),
                    hint: "targets are workspace-relative paths and may not escape the root".into(),
                });
                continue;
            }
            if !scope_prefixes.is_empty() && !in_scope(target, &scope_prefixes) {
                errors.push(PlanError {
                    code: PlanErrorCode::TargetOutsidePlanScope,
                    step: Some(step.id.clone()),
                    message: format!(
                        "target `{}` in step `{}` is outside plan_scope",
                        target.display(),
                        step.id
                    ),
                    hint: "either add the target's directory to plan_scope or drop the target"
                        .into(),
                });
            }
        }
    }
}

fn in_scope(target: &Path, scope_prefixes: &[String]) -> bool {
    let t = target.to_string_lossy().replace('\\', "/");
    scope_prefixes.iter().any(|p| {
        if p.is_empty() {
            true
        } else {
            t == p.trim_end_matches('/') || t.starts_with(p.as_str())
        }
    })
}

/// Kahn's algorithm; `None` if the plan is acyclic.
fn find_cycle(plan: &Plan) -> Option<Vec<PlanStepId>> {
    if topological_waves(plan).is_some() {
        return None;
    }
    // A cycle exists — walk it for a readable path. Start from any node
    // still having unresolved deps after the wave computation stalls.
    let adj = adjacency(plan);
    let mut colour: BTreeMap<PlanStepId, u8> = BTreeMap::new(); // 0=white,1=grey,2=black
    let mut stack: Vec<PlanStepId> = Vec::new();
    for start in adj.keys() {
        if colour.get(start).copied().unwrap_or(0) == 0 {
            if let Some(cycle) = dfs_cycle(start, &adj, &mut colour, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

fn dfs_cycle(
    node: &PlanStepId,
    adj: &BTreeMap<PlanStepId, Vec<PlanStepId>>,
    colour: &mut BTreeMap<PlanStepId, u8>,
    stack: &mut Vec<PlanStepId>,
) -> Option<Vec<PlanStepId>> {
    colour.insert(node.clone(), 1);
    stack.push(node.clone());
    for next in adj.get(node).into_iter().flatten() {
        match colour.get(next).copied().unwrap_or(0) {
            1 => {
                // back-edge: the cycle is stack[pos..] + next
                let pos = stack.iter().position(|s| s == next).unwrap_or(0);
                let mut cycle = stack[pos..].to_vec();
                cycle.push(next.clone());
                return Some(cycle);
            }
            0 => {
                if let Some(c) = dfs_cycle(next, adj, colour, stack) {
                    return Some(c);
                }
            }
            _ => {}
        }
    }
    stack.pop();
    colour.insert(node.clone(), 2);
    None
}

/// `depends_on` edges as forward adjacency (`dep -> dependent` is *not*
/// what we store; we store `step -> its deps`, which is the edge direction
/// a topological sort consumes directly).
fn adjacency(plan: &Plan) -> BTreeMap<PlanStepId, Vec<PlanStepId>> {
    let ids = plan.step_ids();
    let mut adj: BTreeMap<PlanStepId, Vec<PlanStepId>> = BTreeMap::new();
    for step in &plan.steps {
        let deps: Vec<PlanStepId> = step
            .depends_on
            .iter()
            .filter(|d| ids.contains(*d))
            .cloned()
            .collect();
        adj.insert(step.id.clone(), deps);
    }
    adj
}

/// Group steps into dependency waves. `None` if the graph has a cycle.
pub(crate) fn topological_waves(plan: &Plan) -> Option<Vec<Vec<PlanStepId>>> {
    let adj = adjacency(plan);
    let mut remaining: BTreeSet<PlanStepId> = adj.keys().cloned().collect();
    let mut waves: Vec<Vec<PlanStepId>> = Vec::new();

    while !remaining.is_empty() {
        // A step is ready when all its deps are already placed.
        let ready: Vec<PlanStepId> = remaining
            .iter()
            .filter(|id| {
                adj.get(*id)
                    .map(|deps| deps.iter().all(|d| !remaining.contains(d)))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if ready.is_empty() {
            return None; // cycle
        }
        // Preserve the plan's declared order within a wave for determinism.
        let mut wave: Vec<PlanStepId> = plan
            .steps
            .iter()
            .map(|s| s.id.clone())
            .filter(|id| ready.contains(id))
            .collect();
        wave.dedup();
        for id in &wave {
            remaining.remove(id);
        }
        waves.push(wave);
    }
    Some(waves)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EstimatedScope, PlanStep, VerificationRequirement};

    fn ws() -> (valyria_testkit::TempWorkspace, WorkspaceRoot) {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "fn a() {}\n");
        ws.write("src/other.rs", "fn b() {}\n");
        ws.write("tests/it.rs", "\n");
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        (ws, root)
    }

    fn cx<'a>(root: &'a WorkspaceRoot) -> PlanContext<'a> {
        PlanContext {
            workspace_root: root,
            permission_mode: PermissionMode::Assisted,
            allowed_write_roots: vec![root.as_path().to_path_buf()],
        }
    }

    fn mk_step(id: &str) -> PlanStep {
        PlanStep {
            id: PlanStepId::new(id).unwrap(),
            intent: format!("do {id}"),
            targets: vec![],
            depends_on: vec![],
            parallelizable: false,
            checkpoint: false,
            verification: VerificationRequirement::None,
            rollback_boundary: false,
            approval_required: false,
            estimated_scope: EstimatedScope::default(),
        }
    }

    fn dep(step: &mut PlanStep, on: &str) {
        step.depends_on.push(PlanStepId::new(on).unwrap());
    }

    fn codes(errs: &[PlanError]) -> Vec<PlanErrorCode> {
        errs.iter().map(|e| e.code).collect()
    }

    #[test]
    fn empty_plan_is_rejected() {
        let (_ws, root) = ws();
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert_eq!(codes(&errs), vec![PlanErrorCode::EmptyPlan]);
    }

    #[test]
    fn duplicate_step_id_is_rejected() {
        let (_ws, root) = ws();
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![mk_step("a"), mk_step("a")],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::DuplicateStepId));
    }

    #[test]
    fn unknown_dependency_is_rejected() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        dep(&mut a, "ghost");
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::UnknownDependency));
    }

    #[test]
    fn cyclic_dependency_is_rejected_with_a_path() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        let mut b = mk_step("b");
        dep(&mut a, "b");
        dep(&mut b, "a");
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![a, b],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        let cyc = errs
            .iter()
            .find(|e| e.code == PlanErrorCode::CyclicDependency)
            .unwrap();
        assert!(cyc.message.contains("->"), "{}", cyc.message);
    }

    #[test]
    fn mutating_step_without_verification_is_rejected() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.targets = vec!["src/lib.rs".into()];
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::MutatingStepWithoutVerification));
    }

    #[test]
    fn rollback_boundary_without_checkpoint_is_rejected() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.rollback_boundary = true;
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::RollbackBoundaryWithoutCheckpoint));
    }

    #[test]
    fn target_outside_plan_scope_is_rejected() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.targets = vec!["tests/it.rs".into()];
        a.verification = VerificationRequirement::Inherit;
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::TargetOutsidePlanScope));
    }

    #[test]
    fn unresolvable_target_is_rejected() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.targets = vec!["../escape.rs".into()];
        a.verification = VerificationRequirement::Inherit;
        let plan = Plan {
            plan_scope: vec![],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::TargetUnresolvable));
    }

    #[test]
    fn plan_scope_outside_profile_is_rejected() {
        let (_ws, root) = ws();
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![mk_step("a")],
        };
        let mut context = cx(&root);
        // Permit writes only under a sibling that isn't the workspace.
        context.allowed_write_roots = vec![root.as_path().join("does-not-exist")];
        let errs = validate(&plan, &context).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::PlanScopeOutsideProfile));
    }

    #[test]
    fn manual_mode_forbids_plan_scope() {
        let (_ws, root) = ws();
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![mk_step("a")],
        };
        let mut context = cx(&root);
        context.permission_mode = PermissionMode::Manual;
        let errs = validate(&plan, &context).unwrap_err();
        assert!(codes(&errs).contains(&PlanErrorCode::PlanScopeOutsideProfile));
    }

    #[test]
    fn a_well_formed_plan_validates_and_yields_waves() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.targets = vec!["src/lib.rs".into()];
        a.verification = VerificationRequirement::Inherit;
        a.checkpoint = true;
        a.rollback_boundary = true;
        let mut b = mk_step("b");
        b.targets = vec!["src/other.rs".into()];
        b.verification = VerificationRequirement::Command {
            command: "cargo check".into(),
        };
        dep(&mut b, "a");
        let mut c = mk_step("c");
        dep(&mut c, "a");
        c.parallelizable = true;

        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![a, b, c],
        };
        let validated = validate(&plan, &cx(&root)).unwrap();
        assert_eq!(validated.step_count(), 3);
        assert_eq!(
            validated.waves(),
            &[
                vec![PlanStepId::new("a").unwrap()],
                vec![PlanStepId::new("b").unwrap(), PlanStepId::new("c").unwrap()],
            ]
        );
    }

    #[test]
    fn errors_accumulate_rather_than_short_circuit() {
        let (_ws, root) = ws();
        let mut a = mk_step("a");
        a.targets = vec!["tests/it.rs".into()]; // outside scope + no verification
        a.rollback_boundary = true; // + no checkpoint
        let plan = Plan {
            plan_scope: vec!["src/".into()],
            steps: vec![a],
        };
        let errs = validate(&plan, &cx(&root)).unwrap_err();
        assert!(errs.len() >= 3, "expected several errors, got {errs:?}");
    }
}
