//! The permission engine: evaluates a request against grants and the
//! default decision table, and is the *only* thing in the workspace that
//! can mint an [`Authorization`] (D2) — nothing else, not even this
//! crate's other modules, has access to
//! [`Authorization::issue`](crate::authorization::Authorization::issue).
//!
//! Every decision is journaled together with the rule that produced it
//! (§22: "why was this allowed?" must be answerable after the fact).
//!
//! Scope note: git-history-modification stays hard-denied in every mode.
//! The PRD describes an eventual per-workspace override for this; wiring
//! that through (workspace config -> engine construction) is left to the
//! layer that owns workspace config (`valyria-config`, `valyria-app`)
//! rather than implemented here, so this engine's `Deny` for that category
//! is unconditional today.

use std::collections::HashMap;

use parking_lot::RwLock;
use valyria_types::{PermissionCategory, PermissionMode, SessionId, TaskId, Timestamp};
use valyria_util::Clock;

use crate::authorization::{Authorization, AuthorizationKey};
use crate::grants::{Grant, GrantScope, GrantStore};
use crate::request::PermissionRequest;
use crate::rules::{default_decision, DefaultDecision};

#[derive(Debug)]
pub enum Decision {
    Allow(Authorization),
    Deny { reason: String },
    Ask { prompt: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    ExistingGrant,
    DefaultRule,
    ExplicitApproval,
}

#[derive(Debug, Clone)]
pub struct DecisionRecord {
    pub tool: &'static str,
    pub category: PermissionCategory,
    pub target: String,
    pub outcome: &'static str, // "allow" | "deny" | "ask"
    pub source: DecisionSource,
    pub at: Timestamp,
}

pub struct PermissionEngine {
    mode: RwLock<PermissionMode>,
    /// Per-task autonomy overrides (§25, G1). A task created with an
    /// explicit `permission_mode` gets an entry here; its decisions resolve
    /// against this instead of the workspace-global `mode`, so two tasks
    /// running concurrently can sit at different autonomy levels without a
    /// daemon restart. Cleared when the task reaches a terminal state.
    task_modes: RwLock<HashMap<TaskId, PermissionMode>>,
    grants: GrantStore,
    clock: std::sync::Arc<dyn Clock>,
    session: Option<SessionId>,
    journal: RwLock<Vec<DecisionRecord>>,
}

impl PermissionEngine {
    pub fn new(mode: PermissionMode, clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            mode: RwLock::new(mode),
            task_modes: RwLock::new(HashMap::new()),
            grants: GrantStore::new(),
            clock,
            session: None,
            journal: RwLock::new(Vec::new()),
        }
    }

    pub fn with_session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// The workspace-global default mode (the daemon start-time value).
    pub fn mode(&self) -> PermissionMode {
        *self.mode.read()
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write() = mode;
    }

    /// Pin `task` to `mode` for the life of that task (§25, G1).
    pub fn set_task_mode(&self, task: TaskId, mode: PermissionMode) {
        self.task_modes.write().insert(task, mode);
    }

    /// Drop any per-task override for `task` (call when it terminates).
    pub fn clear_task_mode(&self, task: TaskId) {
        self.task_modes.write().remove(&task);
    }

    /// The mode a decision for `task` resolves against: its per-task
    /// override if it has one, else the workspace-global default.
    pub fn effective_mode(&self, task: TaskId) -> PermissionMode {
        self.task_modes
            .read()
            .get(&task)
            .copied()
            .unwrap_or_else(|| self.mode())
    }

    /// Evaluate `req` against existing grants, then the default decision
    /// table. Never mints an `Authorization` for anything the caller
    /// didn't explicitly ask about — this is the single entry point tools
    /// go through before every side-effecting action.
    pub fn evaluate(&self, req: PermissionRequest) -> Decision {
        let now = self.clock.now();

        if let Some(grant) = self.grants.find_covering(&req, self.session, now) {
            let auth = self.mint(&req, now, grant.expires_at);
            self.record(&req, "allow", DecisionSource::ExistingGrant, now);
            return Decision::Allow(auth);
        }

        match default_decision(self.effective_mode(req.task_id), &req) {
            DefaultDecision::Allow => {
                let auth = self.mint(&req, now, None);
                self.record(&req, "allow", DecisionSource::DefaultRule, now);
                Decision::Allow(auth)
            }
            DefaultDecision::Deny => {
                let reason = format!("{:?} is denied by policy regardless of mode", req.category);
                self.record(&req, "deny", DecisionSource::DefaultRule, now);
                Decision::Deny { reason }
            }
            DefaultDecision::Ask => {
                let prompt = format!("Allow {} to {}?", req.tool, req.target);
                self.record(&req, "ask", DecisionSource::DefaultRule, now);
                Decision::Ask { prompt }
            }
        }
    }

    /// Called after a human (or an automated policy sitting above this
    /// engine) approves an `Ask`. Mints the `Authorization` for the
    /// specific request that was asked about, and — unless `scope` is
    /// [`GrantScope::OneShot`] — remembers the approval so the same class
    /// of request doesn't ask again within that scope.
    pub fn approve(
        &self,
        req: PermissionRequest,
        scope: GrantScope,
        expires_at: Option<Timestamp>,
    ) -> Authorization {
        let now = self.clock.now();
        self.grants.add(Grant {
            scope,
            tool: req.tool,
            category: req.category,
            action: req.action,
            granted_at: now,
            expires_at,
        });
        let auth = self.mint(&req, now, expires_at);
        self.record(&req, "allow", DecisionSource::ExplicitApproval, now);
        auth
    }

    pub fn journal(&self) -> Vec<DecisionRecord> {
        self.journal.read().clone()
    }

    fn mint(
        &self,
        req: &PermissionRequest,
        now: Timestamp,
        expires_at: Option<Timestamp>,
    ) -> Authorization {
        let key = AuthorizationKey {
            task_id: req.task_id,
            step_id: req.step_id,
            tool: req.tool,
            input_hash: req.input_hash,
        };
        Authorization::issue(key, now, expires_at)
    }

    fn record(
        &self,
        req: &PermissionRequest,
        outcome: &'static str,
        source: DecisionSource,
        at: Timestamp,
    ) {
        self.journal.write().push(DecisionRecord {
            tool: req.tool,
            category: req.category,
            target: req.target.clone(),
            outcome,
            source,
            at,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{ActionKind, RiskLevel};
    use valyria_types::{StepId, TaskId};
    use valyria_util::{ContentHash, FixedClock};

    fn engine(mode: PermissionMode) -> PermissionEngine {
        PermissionEngine::new(mode, std::sync::Arc::new(FixedClock::at_millis(1000)))
    }

    fn req(
        category: PermissionCategory,
        action: ActionKind,
        in_plan_scope: bool,
    ) -> PermissionRequest {
        PermissionRequest {
            task_id: TaskId::new(),
            step_id: StepId::new(),
            tool: "write_file",
            category,
            action,
            risk: RiskLevel::Safe,
            input_hash: ContentHash::of_bytes(b"content"),
            target: "src/lib.rs".into(),
            in_plan_scope,
        }
    }

    #[test]
    fn allow_mints_a_matching_authorization() {
        let e = engine(PermissionMode::Autonomous);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        let (task_id, step_id, tool, hash) = (r.task_id, r.step_id, r.tool, r.input_hash);

        match e.evaluate(r) {
            Decision::Allow(auth) => assert!(auth.matches(task_id, step_id, tool, hash)),
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn deny_never_mints_an_authorization() {
        let e = engine(PermissionMode::Autonomous);
        let r = req(
            PermissionCategory::GitHistoryModification,
            ActionKind::Write,
            true,
        );
        assert!(matches!(e.evaluate(r), Decision::Deny { .. }));
    }

    #[test]
    fn ask_never_mints_an_authorization_until_approved() {
        let e = engine(PermissionMode::Manual);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        assert!(matches!(e.evaluate(r), Decision::Ask { .. }));
    }

    #[test]
    fn approve_mints_authorization_and_stores_a_grant() {
        let e = engine(PermissionMode::Manual);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        let (task_id, step_id, tool, hash) = (r.task_id, r.step_id, r.tool, r.input_hash);

        let auth = e.approve(r, GrantScope::Workspace, None);
        assert!(auth.matches(task_id, step_id, tool, hash));

        // A second, different request in the same category/action now
        // auto-allows via the stored workspace grant.
        let r2 = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        assert!(matches!(e.evaluate(r2), Decision::Allow(_)));
    }

    #[test]
    fn one_shot_approval_does_not_carry_over() {
        let e = engine(PermissionMode::Manual);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        e.approve(r, GrantScope::OneShot, None);

        let r2 = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        assert!(matches!(e.evaluate(r2), Decision::Ask { .. }));
    }

    #[test]
    fn journal_records_the_rule_source_for_every_decision() {
        let e = engine(PermissionMode::Autonomous);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        e.evaluate(r);

        let journal = e.journal();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].outcome, "allow");
        assert_eq!(journal[0].source, DecisionSource::DefaultRule);
    }

    #[test]
    fn journal_distinguishes_grant_based_from_default_rule_decisions() {
        let e = engine(PermissionMode::Manual);
        let r1 = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        e.approve(r1, GrantScope::Workspace, None);

        let r2 = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        e.evaluate(r2);

        let journal = e.journal();
        assert_eq!(journal.len(), 2);
        assert_eq!(journal[0].source, DecisionSource::ExplicitApproval);
        assert_eq!(journal[1].source, DecisionSource::ExistingGrant);
    }

    #[test]
    fn mode_can_be_changed_at_runtime() {
        let e = engine(PermissionMode::Manual);
        assert_eq!(e.mode(), PermissionMode::Manual);
        e.set_mode(PermissionMode::Autonomous);
        assert_eq!(e.mode(), PermissionMode::Autonomous);
    }

    #[test]
    fn a_per_task_override_beats_the_global_mode_for_that_task_only() {
        // Workspace-global default is Autonomous.
        let e = engine(PermissionMode::Autonomous);
        let pinned = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        let other = req(PermissionCategory::Filesystem, ActionKind::Write, true);

        // Pin just the first task to Manual.
        e.set_task_mode(pinned.task_id, PermissionMode::Manual);
        assert_eq!(e.effective_mode(pinned.task_id), PermissionMode::Manual);
        assert_eq!(e.effective_mode(other.task_id), PermissionMode::Autonomous);

        // The pinned task now has to ask; the other still auto-allows.
        assert!(matches!(e.evaluate(pinned), Decision::Ask { .. }));
        assert!(matches!(e.evaluate(other), Decision::Allow(_)));
    }

    #[test]
    fn clearing_a_task_override_falls_back_to_the_global_mode() {
        let e = engine(PermissionMode::Autonomous);
        let r = req(PermissionCategory::Filesystem, ActionKind::Write, true);
        let task = r.task_id;

        e.set_task_mode(task, PermissionMode::Manual);
        assert_eq!(e.effective_mode(task), PermissionMode::Manual);

        e.clear_task_mode(task);
        assert_eq!(e.effective_mode(task), PermissionMode::Autonomous);
        assert!(matches!(e.evaluate(r), Decision::Allow(_)));
    }
}
