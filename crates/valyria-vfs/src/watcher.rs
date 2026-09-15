//! Debounced filesystem watching (§4.4, §15): feeds incremental indexing
//! and external-modification detection. Wraps `notify` + a debouncer so
//! callers see batched `ChangeSet`s rather than a flood of raw OS events —
//! a single `git checkout` can otherwise produce thousands of individual
//! notifications for one logical change.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify_debouncer_full::notify::RecommendedWatcher;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};

use crate::error::{Result, VfsError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub paths: BTreeSet<PathBuf>,
}

pub struct Watcher {
    // Kept alive for as long as the `Watcher` exists — dropping it stops
    // watching and ends the debouncer's background thread.
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    rx: mpsc::Receiver<ChangeSet>,
}

impl Watcher {
    pub fn new(root: &Path, debounce: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<ChangeSet>();

        let mut debouncer = new_debouncer(debounce, None, move |result: DebounceEventResult| {
            if let Ok(events) = result {
                let paths: BTreeSet<PathBuf> = events
                    .into_iter()
                    .flat_map(|e| e.event.paths.clone())
                    .collect();
                if !paths.is_empty() {
                    let _ = tx.send(ChangeSet { paths });
                }
            }
        })
        .map_err(|e| VfsError::Watch(e.to_string()))?;

        debouncer
            .watch(
                root,
                notify_debouncer_full::notify::RecursiveMode::Recursive,
            )
            .map_err(|e| VfsError::Watch(e.to_string()))?;

        Ok(Self {
            _debouncer: debouncer,
            rx,
        })
    }

    /// Block for up to `timeout` for the next debounced batch of changes.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ChangeSet> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_batched_change_after_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = Watcher::new(dir.path(), Duration::from_millis(50)).unwrap();

        std::fs::write(dir.path().join("new.txt"), b"hello").unwrap();

        let change = watcher
            .recv_timeout(Duration::from_secs(5))
            .expect("expected a change within 5s");
        assert!(change
            .paths
            .iter()
            .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("new.txt")));
    }

    #[test]
    fn multiple_rapid_writes_can_batch_into_fewer_deliveries() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = Watcher::new(dir.path(), Duration::from_millis(200)).unwrap();

        let expected_names: BTreeSet<String> = (0..5).map(|i| format!("f{i}.txt")).collect();
        for name in &expected_names {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }

        let mut seen_names = BTreeSet::new();
        // Drain whatever batches arrive within a generous window; the
        // point under test is that all five files are eventually reported,
        // not the exact number of batches (debouncer coalescing behavior
        // is not something this crate should assert exact shape of), nor
        // the exact set of paths: some backends (e.g. macOS FSEvents) also
        // report the containing directory itself as changed, and may
        // report it via a canonicalized (symlink-resolved) path that
        // doesn't textually match `dir.path()` even though it's the same
        // directory — real OS-layer noise, not something this crate's
        // watcher introduces. Compare by file name only, exactly as
        // `reports_a_batched_change_after_a_write` above does, to sidestep
        // both.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !expected_names.is_subset(&seen_names) {
            if let Some(change) = watcher.recv_timeout(Duration::from_millis(500)) {
                seen_names.extend(
                    change
                        .paths
                        .iter()
                        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                        .map(String::from),
                );
            }
        }

        assert!(
            expected_names.is_subset(&seen_names),
            "expected all 5 files to be reported, saw {seen_names:?}"
        );
    }
}
