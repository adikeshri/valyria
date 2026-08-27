//! The change ledger (§26) and user-change protection (§25).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use valyria_store::BlobStore;
use valyria_types::{LedgerEntryId, StepId, TaskId, ToolInvocationId};
use valyria_util::{Clock, ContentHash};
use valyria_vfs::WorkspaceRoot;

use crate::error::{LedgerError, Result};
use crate::types::{AgentFileState, ChangeClassification, FileBaseline, LedgerEntry};

pub struct Ledger {
    baselines: RwLock<HashMap<PathBuf, FileBaseline>>,
    entries: RwLock<Vec<LedgerEntry>>,
    blobs: BlobStore,
}

impl Ledger {
    pub fn new(blob_root: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            baselines: RwLock::new(HashMap::new()),
            entries: RwLock::new(Vec::new()),
            blobs: BlobStore::new(blob_root)?,
        })
    }

    /// Record what a path looked like when the task first became
    /// interested in it. First touch wins — a later call for the same
    /// path is a no-op, since re-baselining mid-task would erase the very
    /// "what did this look like at the start" information §25 depends on.
    pub fn record_baseline(&self, path: PathBuf, hash_at_task_start: Option<ContentHash>) {
        self.baselines
            .write()
            .entry(path.clone())
            .or_insert(FileBaseline {
                path,
                hash_at_task_start,
                agent_state: AgentFileState::Untouched,
            });
    }

    /// Record a successful agent-authored write. `before_content` is
    /// stored in the blob store (if present) so the entry can later be
    /// rolled back to byte-for-byte, not just described.
    #[allow(clippy::too_many_arguments)]
    pub fn record_write(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: Option<ContentHash>,
        before_content: Option<&[u8]>,
        after_hash: ContentHash,
        clock: &dyn Clock,
    ) -> Result<LedgerEntryId> {
        let id = self.push_entry(
            task_id,
            step_id,
            tool_invocation_id,
            path.clone(),
            before_hash,
            before_content,
            Some(after_hash),
            None,
            clock,
        )?;
        self.set_agent_state(path, before_hash, AgentFileState::Written(after_hash));
        Ok(id)
    }

    /// Record a successful agent-authored deletion.
    #[allow(clippy::too_many_arguments)]
    pub fn record_delete(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: ContentHash,
        before_content: Option<&[u8]>,
        clock: &dyn Clock,
    ) -> Result<LedgerEntryId> {
        let id = self.push_entry(
            task_id,
            step_id,
            tool_invocation_id,
            path.clone(),
            Some(before_hash),
            before_content,
            None,
            None,
            clock,
        )?;
        self.set_agent_state(path, Some(before_hash), AgentFileState::Deleted);
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_entry(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: Option<ContentHash>,
        before_content: Option<&[u8]>,
        after_hash: Option<ContentHash>,
        reverts: Option<LedgerEntryId>,
        clock: &dyn Clock,
    ) -> Result<LedgerEntryId> {
        let content_retained = match before_content {
            Some(content) => {
                self.blobs.put(content)?;
                true
            }
            None => false,
        };

        let id = LedgerEntryId::new();
        self.entries.write().push(LedgerEntry {
            id,
            task_id,
            step_id,
            tool_invocation_id,
            path,
            before_hash,
            after_hash,
            timestamp: clock.now(),
            content_retained,
            reverts,
        });
        Ok(id)
    }

    /// Recovery-only counterpart to [`Ledger::record_write`]: reconstructs a
    /// previously-recorded write in the in-memory index after a crash,
    /// without re-`put`-ing content into the blob store — the bytes are
    /// presumed already durable there from the original, pre-crash write.
    /// Only `Ledger`'s own index (baselines/entries) is lost on crash; blob
    /// content survives on disk regardless. Used by `valyria-task`'s
    /// crash-recovery path (§4.23), never by normal tool execution.
    #[allow(clippy::too_many_arguments)]
    pub fn rehydrate_write(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: Option<ContentHash>,
        after_hash: ContentHash,
        clock: &dyn Clock,
    ) -> LedgerEntryId {
        let id = self.push_rehydrated_entry(
            task_id,
            step_id,
            tool_invocation_id,
            path.clone(),
            before_hash,
            Some(after_hash),
            None,
            clock,
        );
        self.set_agent_state(path, before_hash, AgentFileState::Written(after_hash));
        id
    }

    /// Recovery-only counterpart to [`Ledger::record_delete`]. See
    /// [`Ledger::rehydrate_write`].
    pub fn rehydrate_delete(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: ContentHash,
        clock: &dyn Clock,
    ) -> LedgerEntryId {
        let id = self.push_rehydrated_entry(
            task_id,
            step_id,
            tool_invocation_id,
            path.clone(),
            Some(before_hash),
            None,
            None,
            clock,
        );
        self.set_agent_state(path, Some(before_hash), AgentFileState::Deleted);
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn push_rehydrated_entry(
        &self,
        task_id: TaskId,
        step_id: StepId,
        tool_invocation_id: Option<ToolInvocationId>,
        path: PathBuf,
        before_hash: Option<ContentHash>,
        after_hash: Option<ContentHash>,
        reverts: Option<LedgerEntryId>,
        clock: &dyn Clock,
    ) -> LedgerEntryId {
        // Unlike `push_entry`, there is no `before_content` to `put` — we
        // only record whether the blob the original write already stored
        // is still present, so `rollback_entry` fails loudly instead of
        // silently misreporting retention.
        let content_retained = before_hash.is_some_and(|h| self.blobs.exists(h));
        let id = LedgerEntryId::new();
        self.entries.write().push(LedgerEntry {
            id,
            task_id,
            step_id,
            tool_invocation_id,
            path,
            before_hash,
            after_hash,
            timestamp: clock.now(),
            content_retained,
            reverts,
        });
        id
    }

    fn set_agent_state(
        &self,
        path: PathBuf,
        hash_at_task_start_if_new: Option<ContentHash>,
        state: AgentFileState,
    ) {
        self.baselines
            .write()
            .entry(path.clone())
            .or_insert(FileBaseline {
                path,
                hash_at_task_start: hash_at_task_start_if_new,
                agent_state: AgentFileState::Untouched,
            })
            .agent_state = state;
    }

    /// Classify a newly observed state for `path` against what the ledger
    /// knows — the core of §25's user-change protection. `observed` is
    /// `None` if the path currently doesn't exist.
    pub fn classify(&self, path: &Path, observed: Option<ContentHash>) -> ChangeClassification {
        let baselines = self.baselines.read();
        let Some(baseline) = baselines.get(path) else {
            return ChangeClassification::Unknown;
        };

        match baseline.agent_state {
            AgentFileState::Written(last_write) if Some(last_write) == observed => {
                ChangeClassification::AgentAuthored
            }
            AgentFileState::Deleted if observed.is_none() => ChangeClassification::AgentAuthored,
            AgentFileState::Written(_) | AgentFileState::Deleted => {
                ChangeClassification::ConcurrentUserModification
            }
            AgentFileState::Untouched => {
                if baseline.hash_at_task_start == observed {
                    ChangeClassification::PreExisting
                } else {
                    ChangeClassification::ConcurrentUserModification
                }
            }
        }
    }

    pub fn entries_for_task(&self, task_id: TaskId) -> Vec<LedgerEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| e.task_id == task_id)
            .cloned()
            .collect()
    }

    pub fn entry(&self, id: LedgerEntryId) -> Option<LedgerEntry> {
        self.entries.read().iter().find(|e| e.id == id).cloned()
    }

    /// Roll back one entry, restoring `path` to `before_hash`'s content
    /// (or deleting it, if it didn't exist before this entry). Refuses if
    /// the file has been touched since — by anyone, agent included — since
    /// the ledger's whole purpose is to never blindly overwrite work that
    /// happened after the point it's reverting to.
    pub fn rollback_entry(
        &self,
        id: LedgerEntryId,
        current_hash: Option<ContentHash>,
        root: &WorkspaceRoot,
        task_id: TaskId,
        step_id: StepId,
        clock: &dyn Clock,
    ) -> Result<()> {
        let entry = self.entry(id).ok_or(LedgerError::UnknownEntry(id))?;

        if current_hash != entry.after_hash {
            return Err(LedgerError::RollbackConflict);
        }

        let resolved = root.resolve(&entry.path)?;

        match entry.before_hash {
            Some(before_hash) => {
                if !entry.content_retained {
                    return Err(LedgerError::ContentNotRetained);
                }
                let content = self.blobs.get(before_hash)?;
                valyria_vfs::write_atomic(&resolved, &content)?;
            }
            None => {
                if let Err(e) = std::fs::remove_file(&resolved) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(LedgerError::Vfs(valyria_vfs::VfsError::Io {
                            path: resolved.display().to_string(),
                            source: e,
                        }));
                    }
                }
            }
        }

        let reverted_id = self.push_entry(
            task_id,
            step_id,
            None,
            entry.path.clone(),
            entry.after_hash,
            None,
            entry.before_hash,
            Some(id),
            clock,
        )?;
        let _ = reverted_id;

        let restored_state = match entry.before_hash {
            Some(h) => AgentFileState::Written(h),
            None => AgentFileState::Deleted,
        };
        self.baselines
            .write()
            .entry(entry.path.clone())
            .and_modify(|b| b.agent_state = restored_state);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_util::FixedClock;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path().join("blobs")).unwrap();
        (dir, ledger)
    }

    #[test]
    fn classify_unknown_without_a_baseline() {
        let (_dir, ledger) = ledger();
        let h = ContentHash::of_bytes(b"anything");
        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(h)),
            ChangeClassification::Unknown
        );
    }

    #[test]
    fn classify_pre_existing_when_agent_never_touched_it() {
        let (_dir, ledger) = ledger();
        let start_hash = ContentHash::of_bytes(b"original");
        ledger.record_baseline("f.txt".into(), Some(start_hash));
        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(start_hash)),
            ChangeClassification::PreExisting
        );
    }

    #[test]
    fn classify_agent_authored_after_a_matching_write() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let before = ContentHash::of_bytes(b"before");
        let after = ContentHash::of_bytes(b"after");
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger
            .record_write(
                TaskId::new(),
                StepId::new(),
                None,
                "f.txt".into(),
                Some(before),
                Some(b"before"),
                after,
                &clock,
            )
            .unwrap();

        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(after)),
            ChangeClassification::AgentAuthored
        );
    }

    #[test]
    fn classify_agent_authored_after_a_matching_delete() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let before = ContentHash::of_bytes(b"before");
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger
            .record_delete(
                TaskId::new(),
                StepId::new(),
                None,
                "f.txt".into(),
                before,
                Some(b"before"),
                &clock,
            )
            .unwrap();

        assert_eq!(
            ledger.classify(Path::new("f.txt"), None),
            ChangeClassification::AgentAuthored
        );
    }

    #[test]
    fn classify_concurrent_modification_when_file_reappears_after_agent_deleted_it() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let before = ContentHash::of_bytes(b"before");
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger
            .record_delete(
                TaskId::new(),
                StepId::new(),
                None,
                "f.txt".into(),
                before,
                Some(b"before"),
                &clock,
            )
            .unwrap();

        let someone_recreated_it = ContentHash::of_bytes(b"recreated by someone else");
        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(someone_recreated_it)),
            ChangeClassification::ConcurrentUserModification
        );
    }

    #[test]
    fn classify_concurrent_modification_when_observed_hash_diverges_from_last_write() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let before = ContentHash::of_bytes(b"before");
        let agent_after = ContentHash::of_bytes(b"agent wrote this");
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger
            .record_write(
                TaskId::new(),
                StepId::new(),
                None,
                "f.txt".into(),
                Some(before),
                None,
                agent_after,
                &clock,
            )
            .unwrap();

        let someone_elses_edit = ContentHash::of_bytes(b"a human edited this meanwhile");
        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(someone_elses_edit)),
            ChangeClassification::ConcurrentUserModification
        );
    }

    #[test]
    fn classify_concurrent_modification_when_never_agent_written_and_diverged_from_start() {
        let (_dir, ledger) = ledger();
        let start = ContentHash::of_bytes(b"start");
        ledger.record_baseline("f.txt".into(), Some(start));

        let changed = ContentHash::of_bytes(b"changed without the agent's involvement");
        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(changed)),
            ChangeClassification::ConcurrentUserModification
        );
    }

    #[test]
    fn record_baseline_is_first_touch_wins() {
        let (_dir, ledger) = ledger();
        let first = ContentHash::of_bytes(b"first observed");
        let second = ContentHash::of_bytes(b"second observed, should be ignored as a baseline");
        ledger.record_baseline("f.txt".into(), Some(first));
        ledger.record_baseline("f.txt".into(), Some(second));

        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(first)),
            ChangeClassification::PreExisting
        );
    }

    #[test]
    fn rollback_restores_prior_content() {
        let (_dir, ledger) = ledger();
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let clock = FixedClock::at_millis(0);
        let task_id = TaskId::new();
        let step_id = StepId::new();

        ws.write("f.txt", "original content");
        let before = ContentHash::of_bytes(b"original content");
        ledger.record_baseline("f.txt".into(), Some(before));

        ws.write("f.txt", "agent's edit");
        let after = ContentHash::of_bytes(b"agent's edit");
        let entry_id = ledger
            .record_write(
                task_id,
                step_id,
                None,
                "f.txt".into(),
                Some(before),
                Some(b"original content"),
                after,
                &clock,
            )
            .unwrap();

        ledger
            .rollback_entry(entry_id, Some(after), &root, task_id, step_id, &clock)
            .unwrap();
        assert_eq!(ws.read("f.txt"), "original content");
    }

    #[test]
    fn rollback_refuses_when_file_changed_since_the_write() {
        let (_dir, ledger) = ledger();
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let clock = FixedClock::at_millis(0);
        let task_id = TaskId::new();
        let step_id = StepId::new();

        ws.write("f.txt", "original");
        let before = ContentHash::of_bytes(b"original");
        ledger.record_baseline("f.txt".into(), Some(before));

        ws.write("f.txt", "agent's edit");
        let after = ContentHash::of_bytes(b"agent's edit");
        let entry_id = ledger
            .record_write(
                task_id,
                step_id,
                None,
                "f.txt".into(),
                Some(before),
                Some(b"original"),
                after,
                &clock,
            )
            .unwrap();

        // A human touches the file after the agent's write.
        ws.write("f.txt", "human's further edit, unrelated to the agent");
        let human_hash = ContentHash::of_bytes(b"human's further edit, unrelated to the agent");

        let err = ledger
            .rollback_entry(entry_id, Some(human_hash), &root, task_id, step_id, &clock)
            .unwrap_err();
        assert!(matches!(err, LedgerError::RollbackConflict));
        assert_eq!(
            ws.read("f.txt"),
            "human's further edit, unrelated to the agent"
        );
    }

    #[test]
    fn rollback_of_a_newly_created_file_deletes_it() {
        let (_dir, ledger) = ledger();
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let clock = FixedClock::at_millis(0);
        let task_id = TaskId::new();
        let step_id = StepId::new();

        ledger.record_baseline("new.txt".into(), None);
        ws.write("new.txt", "brand new");
        let after = ContentHash::of_bytes(b"brand new");
        let entry_id = ledger
            .record_write(
                task_id,
                step_id,
                None,
                "new.txt".into(),
                None,
                None,
                after,
                &clock,
            )
            .unwrap();

        ledger
            .rollback_entry(entry_id, Some(after), &root, task_id, step_id, &clock)
            .unwrap();
        assert!(!ws.exists("new.txt"));
    }

    #[test]
    fn rollback_of_a_delete_restores_the_file() {
        let (_dir, ledger) = ledger();
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let clock = FixedClock::at_millis(0);
        let task_id = TaskId::new();
        let step_id = StepId::new();

        ws.write("f.txt", "will be deleted");
        let before = ContentHash::of_bytes(b"will be deleted");
        ledger.record_baseline("f.txt".into(), Some(before));

        std::fs::remove_file(ws.full_path("f.txt")).unwrap();
        let entry_id = ledger
            .record_delete(
                task_id,
                step_id,
                None,
                "f.txt".into(),
                before,
                Some(b"will be deleted"),
                &clock,
            )
            .unwrap();

        ledger
            .rollback_entry(entry_id, None, &root, task_id, step_id, &clock)
            .unwrap();
        assert_eq!(ws.read("f.txt"), "will be deleted");
    }

    #[test]
    fn rehydrate_write_reconstructs_classification_without_reputting_blobs() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let task_id = TaskId::new();
        let step_id = StepId::new();
        let before = ContentHash::of_bytes(b"before");
        let after = ContentHash::of_bytes(b"after");

        // Simulate: the original write happened pre-crash (blob content is
        // durable) but the in-memory ledger index was lost.
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger.rehydrate_write(
            task_id,
            step_id,
            None,
            "f.txt".into(),
            Some(before),
            after,
            &clock,
        );

        assert_eq!(
            ledger.classify(Path::new("f.txt"), Some(after)),
            ChangeClassification::AgentAuthored
        );
    }

    #[test]
    fn rehydrate_write_reports_content_not_retained_when_blob_is_absent() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        // Never `put` into the blob store for this hash.
        let before = ContentHash::of_bytes(b"never stored");
        let after = ContentHash::of_bytes(b"after");
        let id = ledger.rehydrate_write(
            TaskId::new(),
            StepId::new(),
            None,
            "f.txt".into(),
            Some(before),
            after,
            &clock,
        );
        assert!(!ledger.entry(id).unwrap().content_retained);
    }

    #[test]
    fn rehydrate_delete_reconstructs_classification() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let before = ContentHash::of_bytes(b"before");
        ledger.record_baseline("f.txt".into(), Some(before));
        ledger.rehydrate_delete(
            TaskId::new(),
            StepId::new(),
            None,
            "f.txt".into(),
            before,
            &clock,
        );

        assert_eq!(
            ledger.classify(Path::new("f.txt"), None),
            ChangeClassification::AgentAuthored
        );
    }

    #[test]
    fn entries_for_task_filters_correctly() {
        let (_dir, ledger) = ledger();
        let clock = FixedClock::at_millis(0);
        let task_a = TaskId::new();
        let task_b = TaskId::new();

        ledger
            .record_write(
                task_a,
                StepId::new(),
                None,
                "a.txt".into(),
                None,
                None,
                ContentHash::of_bytes(b"a"),
                &clock,
            )
            .unwrap();
        ledger
            .record_write(
                task_b,
                StepId::new(),
                None,
                "b.txt".into(),
                None,
                None,
                ContentHash::of_bytes(b"b"),
                &clock,
            )
            .unwrap();

        let entries = ledger.entries_for_task(task_a);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("a.txt"));
    }
}
