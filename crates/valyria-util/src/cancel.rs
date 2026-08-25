//! Cancellation (cross-cutting conventions, §3): one token tree per task,
//! propagated into every process, model call, and index query.
//!
//! Thin wrapper over [`tokio_util::sync::CancellationToken`] so the rest of
//! the workspace depends on `valyria_util::CancellationToken` rather than a
//! third-party path directly, and so we have one place to add task-scoped
//! behavior (e.g. recording *why* a branch was cancelled) later.

use std::fmt;

#[derive(Clone)]
pub struct CancellationToken(tokio_util::sync::CancellationToken);

impl CancellationToken {
    pub fn new() -> Self {
        Self(tokio_util::sync::CancellationToken::new())
    }

    /// A child token: cancelling the parent cancels every child, but
    /// cancelling a child never propagates back up. This is the shape a
    /// task's cancellation tree needs — cancelling the task cancels every
    /// in-flight tool/model call it spawned, but a single tool timing out
    /// must not cancel the whole task.
    pub fn child(&self) -> Self {
        Self(self.0.child_token())
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationToken")
            .field("is_cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_is_cancelled_when_parent_is() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn parent_survives_child_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_future_resolves_after_cancel() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            "done"
        });
        token.cancel();
        assert_eq!(handle.await.unwrap(), "done");
    }
}
