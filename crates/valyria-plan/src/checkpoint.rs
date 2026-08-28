//! Checkpoints and rollback boundaries (§4.12, §4.25 — "rollback to a
//! checkpoint restores the tree exactly and refuses on user-touched
//! files").
//!
//! A [`Checkpoint`] is a marker, not a copy: it records the set of
//! task-touched files with their on-disk hashes at the moment it was
//! taken, plus a **watermark** into the change ledger. Rolling back to it
//! replays every ledger entry recorded *after* the watermark in reverse
//! through [`valyria_ledger::Ledger::rollback_entry`], which already
//! refuses (`RollbackConflict`) to revert a file that anyone — the user
//! included — has touched since. The first such refusal aborts the whole
//! rollback with the offending path and leaves the tree exactly as it was.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use valyria_ledger::{Ledger, LedgerError};
use valyria_types::{CheckpointId, StepId, TaskId, Timestamp};
use valyria_util::{Clock, ContentHash};
use valyria_vfs::WorkspaceRoot;

use crate::model::PlanStepId;

/// A rollback point taken before (or after) a plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub task_id: TaskId,
    /// The plan step this checkpoint is associated with.
    pub step_id: PlanStepId,
    /// Every path the task had touched at checkpoint time and its on-disk
    /// content hash (`None` = the path did not exist).
    pub files: BTreeMap<PathBuf, Option<String>>,
    /// Number of ledger entries the task had accrued when the checkpoint
    /// was taken. Rollback targets everything after this index.
    pub ledger_watermark: usize,
    pub created_at: Timestamp,
}

impl Checkpoint {
    pub fn file_hash(&self, path: &PathBuf) -> Option<ContentHash> {
        self.files
            .get(path)
            .and_then(|h| h.as_ref())
            .and_then(|h| parse_hex(h))
    }
}

/// The outcome of a successful rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    /// Paths whose content was reverted, in the order they were reverted.
    pub reverted: Vec<PathBuf>,
    pub checkpoint: CheckpointId,
}

#[derive(Debug, thiserror::Error)]
pub enum RollbackError {
    #[error("no checkpoint `{0}`")]
    NotFound(String),
    #[error("refusing to roll back `{path}`: it has been modified since the checkpoint")]
    UserModified { path: PathBuf },
    #[error("cannot roll back `{path}`: its pre-edit content was not retained")]
    ContentNotRetained { path: PathBuf },
    #[error(
        "rollback completed but `{path}` does not match its checkpoint hash — \
         the tree was not restored exactly"
    )]
    IntegrityCheckFailed { path: PathBuf },
    #[error("ledger error during rollback: {0}")]
    Ledger(String),
    #[error("filesystem error during rollback: {0}")]
    Vfs(String),
}

/// Capture a checkpoint. `touched` is the task's currently-changed file
/// set (paths + current on-disk hashes); `ledger_watermark` is
/// `ledger.entries_for_task(task_id).len()` at this instant.
pub fn capture(
    task_id: TaskId,
    step_id: &PlanStepId,
    touched: impl IntoIterator<Item = (PathBuf, Option<ContentHash>)>,
    ledger_watermark: usize,
    now: Timestamp,
) -> Checkpoint {
    let files = touched
        .into_iter()
        .map(|(p, h)| (p, h.map(|h| h.to_hex())))
        .collect();
    Checkpoint {
        id: CheckpointId::new(),
        task_id,
        step_id: step_id.clone(),
        files,
        ledger_watermark,
        created_at: now,
    }
}

/// Roll the workspace back to `cp`. Reverts every ledger entry recorded
/// after `cp.ledger_watermark`, newest first. Aborts on the first entry
/// whose file has diverged from the ledger's record of it — that is a
/// user modification and must not be clobbered.
pub fn rollback(
    cp: &Checkpoint,
    ledger: &Ledger,
    root: &WorkspaceRoot,
    revert_step: StepId,
    clock: &dyn Clock,
) -> Result<RollbackReport, RollbackError> {
    let all = ledger.entries_for_task(cp.task_id);
    let since: Vec<_> = all.into_iter().skip(cp.ledger_watermark).collect();

    let mut reverted = Vec::new();
    for entry in since.iter().rev() {
        // The ledger's own precondition check (`current_hash ==
        // entry.after_hash`) is what enforces "refuses on user-touched
        // files"; feed it the live on-disk hash.
        let current = current_hash(root, &entry.path);
        match ledger.rollback_entry(entry.id, current, root, cp.task_id, revert_step, clock) {
            Ok(()) => {
                if !reverted.contains(&entry.path) {
                    reverted.push(entry.path.clone());
                }
            }
            Err(LedgerError::RollbackConflict) => {
                return Err(RollbackError::UserModified {
                    path: entry.path.clone(),
                });
            }
            Err(LedgerError::ContentNotRetained) => {
                return Err(RollbackError::ContentNotRetained {
                    path: entry.path.clone(),
                });
            }
            Err(LedgerError::Vfs(e)) => return Err(RollbackError::Vfs(e.to_string())),
            Err(other) => return Err(RollbackError::Ledger(other.to_string())),
        }
    }

    // "restores the tree exactly": every checkpointed file must now hash to
    // what the checkpoint recorded.
    for (path, expected_hex) in &cp.files {
        let expected = expected_hex.as_ref().and_then(|h| parse_hex(h));
        if current_hash(root, path) != expected {
            return Err(RollbackError::IntegrityCheckFailed { path: path.clone() });
        }
    }

    Ok(RollbackReport {
        reverted,
        checkpoint: cp.id,
    })
}

fn current_hash(root: &WorkspaceRoot, rel: &PathBuf) -> Option<ContentHash> {
    let resolved = root.resolve(rel).ok()?;
    let bytes = std::fs::read(&resolved).ok()?;
    Some(ContentHash::of_bytes(&bytes))
}

/// `ContentHash` serializes as its 64-char hex string, so this is the
/// documented way to reconstruct one from the hex we stored.
fn parse_hex(hex: &str) -> Option<ContentHash> {
    serde_json::from_value(serde_json::Value::String(hex.to_string())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_testkit::TempWorkspace;
    use valyria_util::FixedClock;

    struct Fx {
        _blob: tempfile::TempDir,
        ledger: Ledger,
        ws: TempWorkspace,
        root: WorkspaceRoot,
        task: TaskId,
        clock: FixedClock,
    }

    fn fx() -> Fx {
        let blob = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(blob.path()).unwrap();
        let ws = TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        Fx {
            _blob: blob,
            ledger,
            ws,
            root,
            task: TaskId::new(),
            clock: FixedClock::at_millis(1),
        }
    }

    /// Record an agent write exactly as the edit tool does: baseline, write
    /// bytes, ledger `record_write` with before-content retained.
    fn agent_write(fx: &Fx, path: &str, before: Option<&str>, after: &str) {
        let before_hash = before.map(|b| ContentHash::of_bytes(b.as_bytes()));
        fx.ledger.record_baseline(path.into(), before_hash);
        fx.ws.write(path, after);
        fx.ledger
            .record_write(
                fx.task,
                StepId::new(),
                None,
                path.into(),
                before_hash,
                before.map(|b| b.as_bytes()),
                ContentHash::of_bytes(after.as_bytes()),
                &fx.clock,
            )
            .unwrap();
    }

    fn touched_now(fx: &Fx, paths: &[&str]) -> Vec<(PathBuf, Option<ContentHash>)> {
        paths
            .iter()
            .map(|p| {
                let h = std::fs::read(fx.ws.full_path(p))
                    .ok()
                    .map(|b| ContentHash::of_bytes(&b));
                (PathBuf::from(p), h)
            })
            .collect()
    }

    #[test]
    fn rollback_restores_exact_bytes() {
        let fx = fx();
        agent_write(&fx, "a.txt", None, "a-v1");
        agent_write(&fx, "b.txt", None, "b-v1");
        let watermark = fx.ledger.entries_for_task(fx.task).len();
        let cp = capture(
            fx.task,
            &PlanStepId::new("step-1").unwrap(),
            touched_now(&fx, &["a.txt", "b.txt"]),
            watermark,
            Timestamp::from_millis(10),
        );

        // Step 2 edits b and creates c.
        agent_write(&fx, "b.txt", Some("b-v1"), "b-v2-DID-STUFF");
        agent_write(&fx, "c.txt", None, "c-brand-new");

        let report = rollback(&cp, &fx.ledger, &fx.root, StepId::new(), &fx.clock).unwrap();
        assert_eq!(fx.ws.read("a.txt"), "a-v1");
        assert_eq!(fx.ws.read("b.txt"), "b-v1");
        assert!(!fx.ws.exists("c.txt"), "c.txt should have been removed");
        assert!(report.reverted.contains(&PathBuf::from("b.txt")));
        assert!(report.reverted.contains(&PathBuf::from("c.txt")));
    }

    #[test]
    fn rollback_refuses_and_leaves_tree_untouched_when_user_edited_a_file() {
        let fx = fx();
        agent_write(&fx, "a.txt", None, "a-v1");
        let watermark = fx.ledger.entries_for_task(fx.task).len();
        let cp = capture(
            fx.task,
            &PlanStepId::new("step-1").unwrap(),
            touched_now(&fx, &["a.txt"]),
            watermark,
            Timestamp::from_millis(10),
        );

        agent_write(&fx, "b.txt", None, "b-agent-v1");
        // A human edits b.txt out of band — no ledger entry.
        fx.ws.write("b.txt", "HUMAN WAS HERE");

        let err = rollback(&cp, &fx.ledger, &fx.root, StepId::new(), &fx.clock).unwrap_err();
        assert!(matches!(
            err,
            RollbackError::UserModified { ref path } if path == &PathBuf::from("b.txt")
        ));
        // Nothing rolled back.
        assert_eq!(fx.ws.read("b.txt"), "HUMAN WAS HERE");
        assert_eq!(fx.ws.read("a.txt"), "a-v1");
    }

    #[test]
    fn empty_since_watermark_is_a_noop_success() {
        let fx = fx();
        agent_write(&fx, "a.txt", None, "a-v1");
        let watermark = fx.ledger.entries_for_task(fx.task).len();
        let cp = capture(
            fx.task,
            &PlanStepId::new("s").unwrap(),
            touched_now(&fx, &["a.txt"]),
            watermark,
            Timestamp::from_millis(10),
        );
        let report = rollback(&cp, &fx.ledger, &fx.root, StepId::new(), &fx.clock).unwrap();
        assert!(report.reverted.is_empty());
        assert_eq!(fx.ws.read("a.txt"), "a-v1");
    }
}
