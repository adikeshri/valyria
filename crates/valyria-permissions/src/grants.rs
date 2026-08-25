//! Persisted grants: the result of a user answering `Ask` once, remembered
//! so the same class of request doesn't ask again within its scope.
//!
//! A grant is scoped narrower than "the tool is now always allowed" — it
//! covers one `(tool, category, action)` triple within a scope (one-shot,
//! this task, this workspace, this session), matching §22's "grants can be
//! scoped: one-shot, for-this-task, for-this-workspace, for-this-session,
//! with expiry".

use parking_lot::RwLock;
use valyria_types::{PermissionCategory, SessionId, TaskId, Timestamp};

use crate::request::{ActionKind, PermissionRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrantScope {
    /// Covers exactly the request that was approved, and nothing else —
    /// never stored for reuse (see [`GrantStore::add`]).
    OneShot,
    Task(TaskId),
    Workspace,
    Session(SessionId),
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub scope: GrantScope,
    pub tool: &'static str,
    pub category: PermissionCategory,
    pub action: ActionKind,
    pub granted_at: Timestamp,
    pub expires_at: Option<Timestamp>,
}

impl Grant {
    fn covers(
        &self,
        req: &PermissionRequest,
        current_session: Option<SessionId>,
        now: Timestamp,
    ) -> bool {
        if self.tool != req.tool || self.category != req.category || self.action != req.action {
            return false;
        }
        if let Some(exp) = self.expires_at {
            if now.as_millis() >= exp.as_millis() {
                return false;
            }
        }
        match self.scope {
            GrantScope::OneShot => false,
            GrantScope::Task(t) => t == req.task_id,
            GrantScope::Workspace => true,
            GrantScope::Session(s) => Some(s) == current_session,
        }
    }
}

#[derive(Default)]
pub struct GrantStore {
    grants: RwLock<Vec<Grant>>,
}

impl GrantStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a grant for future reuse. One-shot grants are deliberately
    /// not stored — they authorize only the single request they were
    /// issued for, which the engine handles by minting the `Authorization`
    /// directly rather than consulting the store again.
    pub fn add(&self, grant: Grant) {
        if grant.scope == GrantScope::OneShot {
            return;
        }
        self.grants.write().push(grant);
    }

    pub fn find_covering(
        &self,
        req: &PermissionRequest,
        current_session: Option<SessionId>,
        now: Timestamp,
    ) -> Option<Grant> {
        self.grants
            .read()
            .iter()
            .find(|g| g.covers(req, current_session, now))
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.grants.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_util::ContentHash;

    fn req(task_id: TaskId, in_plan_scope: bool) -> PermissionRequest {
        PermissionRequest {
            task_id,
            step_id: valyria_types::StepId::new(),
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            risk: crate::request::RiskLevel::Safe,
            input_hash: ContentHash::of_bytes(b"whatever"),
            target: "src/lib.rs".into(),
            in_plan_scope,
        }
    }

    #[test]
    fn workspace_grant_covers_any_task() {
        let store = GrantStore::new();
        store.add(Grant {
            scope: GrantScope::Workspace,
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            granted_at: Timestamp::from_millis(0),
            expires_at: None,
        });

        let r = req(TaskId::new(), true);
        assert!(store
            .find_covering(&r, None, Timestamp::from_millis(1))
            .is_some());
    }

    #[test]
    fn task_grant_does_not_cover_a_different_task() {
        let store = GrantStore::new();
        let task = TaskId::new();
        store.add(Grant {
            scope: GrantScope::Task(task),
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            granted_at: Timestamp::from_millis(0),
            expires_at: None,
        });

        let other_task_req = req(TaskId::new(), true);
        assert!(store
            .find_covering(&other_task_req, None, Timestamp::from_millis(1))
            .is_none());
    }

    #[test]
    fn one_shot_grants_are_never_stored() {
        let store = GrantStore::new();
        store.add(Grant {
            scope: GrantScope::OneShot,
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            granted_at: Timestamp::from_millis(0),
            expires_at: None,
        });
        assert!(store.is_empty());
    }

    #[test]
    fn expired_grants_do_not_cover() {
        let store = GrantStore::new();
        store.add(Grant {
            scope: GrantScope::Workspace,
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            granted_at: Timestamp::from_millis(0),
            expires_at: Some(Timestamp::from_millis(100)),
        });

        let r = req(TaskId::new(), true);
        assert!(store
            .find_covering(&r, None, Timestamp::from_millis(50))
            .is_some());
        assert!(store
            .find_covering(&r, None, Timestamp::from_millis(200))
            .is_none());
    }

    #[test]
    fn grant_does_not_cover_a_different_category_or_action() {
        let store = GrantStore::new();
        store.add(Grant {
            scope: GrantScope::Workspace,
            tool: "write_file",
            category: PermissionCategory::Filesystem,
            action: ActionKind::Write,
            granted_at: Timestamp::from_millis(0),
            expires_at: None,
        });

        let mut r = req(TaskId::new(), true);
        r.category = PermissionCategory::Network;
        assert!(store
            .find_covering(&r, None, Timestamp::from_millis(1))
            .is_none());

        let mut r2 = req(TaskId::new(), true);
        r2.action = ActionKind::Read;
        assert!(store
            .find_covering(&r2, None, Timestamp::from_millis(1))
            .is_none());
    }
}
