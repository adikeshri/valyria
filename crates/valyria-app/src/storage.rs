//! Storage inspection and reclamation (§4.1, §48: "users must be able to
//! inspect and delete"). `valyria storage` / `valyria clean` are built
//! entirely from [`StorageInspector::inspect`] and
//! [`StorageInspector::purge`] — the CLI holds no deletion logic of its
//! own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use valyria_memory::{MemoryStore, PurgeScope as MemoryPurgeScope};

use crate::error::Result;

/// One line of a [`StorageReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    pub name: String,
    pub bytes: u64,
    pub detail: Option<String>,
    /// Whether [`StorageInspector::purge`] can reclaim it.
    pub purgeable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageReport {
    pub entries: Vec<StorageEntry>,
}

impl StorageReport {
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }
}

/// What [`StorageInspector::purge`] should reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeScope {
    /// Agent-extracted and session/task memory in this workspace, plus
    /// global user memory. User-authored workspace entries are kept —
    /// `MemoryStore::purge` handles that distinction.
    Memory,
    /// The `cache/` directory.
    Cache,
    /// Per-task artifact directories under `tasks/` (transcripts, diffs).
    /// The durable task rows in `workspace.db` are untouched.
    Tasks,
    /// The global `logs/` directory.
    Logs,
}

impl PurgeScope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "memory" => Some(Self::Memory),
            "cache" => Some(Self::Cache),
            "tasks" => Some(Self::Tasks),
            "logs" => Some(Self::Logs),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Cache => "cache",
            Self::Tasks => "tasks",
            Self::Logs => "logs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PurgeOutcome {
    pub freed_bytes: u64,
    pub items_removed: u64,
    pub dry_run: bool,
}

pub struct StorageInspector {
    data_dir: PathBuf,
    global_root: PathBuf,
    workspace_memory: Arc<MemoryStore>,
    user_memory: Arc<MemoryStore>,
}

impl std::fmt::Debug for StorageInspector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageInspector")
            .field("data_dir", &self.data_dir)
            .field("global_root", &self.global_root)
            .finish_non_exhaustive()
    }
}

impl StorageInspector {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        global_root: impl Into<PathBuf>,
        workspace_memory: Arc<MemoryStore>,
        user_memory: Arc<MemoryStore>,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            global_root: global_root.into(),
            workspace_memory,
            user_memory,
        }
    }

    pub fn inspect(&self) -> StorageReport {
        let d = &self.data_dir;
        let g = &self.global_root;
        let entries = vec![
            entry(
                "workspace.db",
                file_size(&d.join("workspace.db")),
                None,
                false,
            ),
            entry("blobs", dir_size(&d.join("blobs")), None, false),
            entry("index", dir_size(&d.join("index")), None, false),
            entry(
                "cache",
                dir_size(&d.join("cache")),
                Some("`valyria clean --scope cache`".into()),
                true,
            ),
            entry(
                "tasks",
                dir_size(&d.join("tasks")),
                Some("`valyria clean --scope tasks`".into()),
                true,
            ),
            entry("global.db", file_size(&g.join("global.db")), None, false),
            entry("models", dir_size(&g.join("models")), None, false),
            entry(
                "logs",
                dir_size(&g.join("logs")),
                Some("`valyria clean --scope logs`".into()),
                true,
            ),
        ];
        StorageReport { entries }
    }

    pub async fn purge(&self, scope: PurgeScope, dry_run: bool) -> Result<PurgeOutcome> {
        match scope {
            PurgeScope::Memory => {
                if dry_run {
                    let ws = self.workspace_memory.stats().await?;
                    let user = self.user_memory.stats().await?;
                    return Ok(PurgeOutcome {
                        freed_bytes: 0,
                        items_removed: ws.live + ws.retired + user.live + user.retired,
                        dry_run: true,
                    });
                }
                let a = self
                    .workspace_memory
                    .purge(MemoryPurgeScope::Retired)
                    .await?;
                let b = self.user_memory.purge(MemoryPurgeScope::Retired).await?;
                Ok(PurgeOutcome {
                    freed_bytes: 0,
                    items_removed: a + b,
                    dry_run: false,
                })
            }
            PurgeScope::Cache => self.purge_dir(&self.data_dir.join("cache"), dry_run),
            PurgeScope::Tasks => self.purge_dir(&self.data_dir.join("tasks"), dry_run),
            PurgeScope::Logs => self.purge_dir(&self.global_root.join("logs"), dry_run),
        }
    }

    fn purge_dir(&self, dir: &Path, dry_run: bool) -> Result<PurgeOutcome> {
        let (bytes, count) = count_dir(dir);
        if !dry_run && dir.exists() {
            for child in read_dir(dir) {
                if child.is_dir() {
                    let _ = std::fs::remove_dir_all(&child);
                } else {
                    let _ = std::fs::remove_file(&child);
                }
            }
        }
        Ok(PurgeOutcome {
            freed_bytes: bytes,
            items_removed: count,
            dry_run,
        })
    }
}

fn entry(name: &str, bytes: u64, detail: Option<String>, purgeable: bool) -> StorageEntry {
    StorageEntry {
        name: name.to_string(),
        bytes,
        detail,
        purgeable,
    }
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect()
}

/// Recursive on-disk size of `dir`, `0` if it does not exist.
fn dir_size(dir: &Path) -> u64 {
    count_dir(dir).0
}

/// `(total_bytes, file_count)` under `dir`, recursively.
fn count_dir(dir: &Path) -> (u64, u64) {
    let mut bytes = 0;
    let mut count = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            for child in read_dir(&path) {
                stack.push(child);
            }
        } else {
            bytes += meta.len();
            count += 1;
        }
    }
    (bytes, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use valyria_store::Store;

    fn mem_store() -> Arc<MemoryStore> {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(Store::open(&dir.path().join("m.db"), valyria_memory::MIGRATIONS).unwrap());
        // Leak the tempdir so the file outlives the test body.
        std::mem::forget(dir);
        Arc::new(MemoryStore::new(store))
    }

    #[test]
    fn purge_scope_round_trips_its_string() {
        for s in ["memory", "cache", "tasks", "logs"] {
            assert_eq!(PurgeScope::parse(s).unwrap().as_str(), s);
        }
        assert!(PurgeScope::parse("bogus").is_none());
    }

    #[test]
    fn inspect_reports_zero_for_a_bare_workspace_and_lists_every_entry() {
        let data = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        let insp = StorageInspector::new(data.path(), global.path(), mem_store(), mem_store());
        let report = insp.inspect();
        let names: Vec<_> = report.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"workspace.db"));
        assert!(names.contains(&"blobs"));
        assert!(names.contains(&"models"));
        assert_eq!(report.total_bytes(), 0);
    }

    #[tokio::test]
    async fn purge_cache_removes_files_and_reports_bytes() {
        let data = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        let cache = data.path().join("cache");
        std::fs::create_dir_all(cache.join("sub")).unwrap();
        std::fs::write(cache.join("a.bin"), vec![0u8; 2048]).unwrap();
        std::fs::write(cache.join("sub/b.bin"), vec![0u8; 1024]).unwrap();

        let insp = StorageInspector::new(data.path(), global.path(), mem_store(), mem_store());

        let dry = insp.purge(PurgeScope::Cache, true).await.unwrap();
        assert_eq!(dry.freed_bytes, 3072);
        assert_eq!(dry.items_removed, 2);
        assert!(dry.dry_run);
        assert!(cache.join("a.bin").exists(), "dry run must not delete");

        let wet = insp.purge(PurgeScope::Cache, false).await.unwrap();
        assert_eq!(wet.freed_bytes, 3072);
        assert!(!cache.join("a.bin").exists());
        assert_eq!(dir_size(&cache), 0);
    }
}
