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

        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }

        let mut seen = BTreeSet::new();
        // Drain whatever batches arrive within a generous window; the
        // point under test is that all five files are eventually reported,
        // not the exact number of batches (debouncer coalescing behavior
        // is not something this crate should assert exact shape of).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && seen.len() < 5 {
            if let Some(change) = watcher.recv_timeout(Duration::from_millis(500)) {
                seen.extend(change.paths);
            }
        }

        assert_eq!(
            seen.len(),
            5,
            "expected all 5 files to be reported, saw {seen:?}"
        );
    }
}
