//! `Authorization` (D2): an unforgeable capability, not a boolean.
//!
//! There is no code path that executes a tool without one, and it cannot
//! be created by the agent crate, by a model response, or by a tool — the
//! constructor is `pub(crate)`, so only [`crate::engine::PermissionEngine`]
//! can mint one. Every authorization is bound to the exact
//! `(task_id, step_id, tool, canonical_input_hash)` it was issued for, so
//! approval for one call can never be "spent" on a different one — the
//! classic TOCTOU gap (approve `rm ./tmp`, then actually run
//! `rm -rf /`) is closed by binding the hash of the *exact* input, not
//! just the tool name.

use valyria_types::{StepId, TaskId, Timestamp};
use valyria_util::ContentHash;

/// Identifies exactly what an [`Authorization`] was issued for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthorizationKey {
    pub task_id: TaskId,
    pub step_id: StepId,
    pub tool: &'static str,
    pub input_hash: ContentHash,
}

#[derive(Debug, Clone)]
pub struct Authorization {
    key: AuthorizationKey,
    issued_at: Timestamp,
    expires_at: Option<Timestamp>,
}

impl Authorization {
    pub(crate) fn issue(
        key: AuthorizationKey,
        issued_at: Timestamp,
        expires_at: Option<Timestamp>,
    ) -> Self {
        Self {
            key,
            issued_at,
            expires_at,
        }
    }

    /// Whether this authorization covers exactly this request. A tool's
    /// `execute` should call this itself before doing anything
    /// side-effecting — defense in depth, not just trust in the caller.
    pub fn matches(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool: &str,
        input_hash: ContentHash,
    ) -> bool {
        self.key.task_id == task_id
            && self.key.step_id == step_id
            && self.key.tool == tool
            && self.key.input_hash == input_hash
    }

    pub fn is_expired(&self, now: Timestamp) -> bool {
        match self.expires_at {
            Some(exp) => now.as_millis() >= exp.as_millis(),
            None => false,
        }
    }

    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AuthorizationKey {
        AuthorizationKey {
            task_id: TaskId::new(),
            step_id: StepId::new(),
            tool: "write_file",
            input_hash: ContentHash::of_bytes(b"input a"),
        }
    }

    #[test]
    fn matches_exact_key() {
        let k = key();
        let auth = Authorization::issue(k, Timestamp::from_millis(0), None);
        assert!(auth.matches(k.task_id, k.step_id, k.tool, k.input_hash));
    }

    #[test]
    fn does_not_match_a_different_input_hash() {
        let k = key();
        let auth = Authorization::issue(k, Timestamp::from_millis(0), None);
        let different_hash = ContentHash::of_bytes(b"input b, a different command");
        assert!(!auth.matches(k.task_id, k.step_id, k.tool, different_hash));
    }

    #[test]
    fn does_not_match_a_different_task_or_step() {
        let k = key();
        let auth = Authorization::issue(k, Timestamp::from_millis(0), None);
        assert!(!auth.matches(TaskId::new(), k.step_id, k.tool, k.input_hash));
        assert!(!auth.matches(k.task_id, StepId::new(), k.tool, k.input_hash));
    }

    #[test]
    fn does_not_match_a_different_tool() {
        let k = key();
        let auth = Authorization::issue(k, Timestamp::from_millis(0), None);
        assert!(!auth.matches(k.task_id, k.step_id, "delete_file", k.input_hash));
    }

    #[test]
    fn never_expires_when_expiry_is_none() {
        let auth = Authorization::issue(key(), Timestamp::from_millis(0), None);
        assert!(!auth.is_expired(Timestamp::from_millis(u128::MAX / 2)));
    }

    #[test]
    fn expires_at_the_configured_time() {
        let auth = Authorization::issue(
            key(),
            Timestamp::from_millis(0),
            Some(Timestamp::from_millis(1000)),
        );
        assert!(!auth.is_expired(Timestamp::from_millis(999)));
        assert!(auth.is_expired(Timestamp::from_millis(1000)));
        assert!(auth.is_expired(Timestamp::from_millis(1001)));
    }
}
