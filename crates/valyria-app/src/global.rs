//! `GlobalStore` — the assembly point for `~/.valyria/global.db` (§4.1:
//! "one global database for models/user memory/config state").
//!
//! Until Phase 10 the installed-model index (`valyria-model-store`, block
//! 900-999) and user-scoped memory (`valyria-memory`, block 600-699) each
//! defined a global-db schema slice but nothing opened the database. This
//! is that opener: one `Store` over `global.db`, the concatenation of
//! every crate's global migrations, plus this crate's own tiny
//! workspace-registry table (block 10_100-10_199 — well clear of the
//! workspace.db hundred-blocks so the two databases can never be confused
//! by version number alone).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::params;
use valyria_memory::MemoryStore;
use valyria_model_store::InstalledModelStore;
use valyria_store::{Migration, Store};
use valyria_types::WorkspaceId;

use crate::error::{AppError, Result};

const APP_GLOBAL_MIGRATIONS: &[Migration] = &[Migration {
    version: 10_100,
    description: "create workspace_registry table",
    sql: "CREATE TABLE workspace_registry (
        workspace_id TEXT PRIMARY KEY,
        path         TEXT NOT NULL,
        first_seen_ms INTEGER NOT NULL,
        last_seen_ms  INTEGER NOT NULL
    );",
}];

/// Every migration that shapes `global.db`, concatenated in one place —
/// the same pattern [`crate::migrations::workspace_migrations`] uses for
/// `workspace.db`.
pub fn global_migrations() -> Vec<Migration> {
    let mut all: Vec<Migration> = valyria_model_store::MIGRATIONS.to_vec();
    all.extend(valyria_memory::MIGRATIONS.iter().copied());
    all.extend(APP_GLOBAL_MIGRATIONS.iter().copied());
    all
}

/// One row of [`GlobalStore::workspaces`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRegistration {
    pub workspace_id: WorkspaceId,
    pub path: PathBuf,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
}

pub struct GlobalStore {
    root: PathBuf,
    store: Arc<Store>,
    models: Arc<InstalledModelStore>,
    user_memory: Arc<MemoryStore>,
}

impl std::fmt::Debug for GlobalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl GlobalStore {
    /// The default global directory: `$VALYRIA_HOME`, else `$HOME/.valyria`.
    /// Falls back to `./.valyria` only if neither is set (CI containers,
    /// mostly) so nothing ever panics for want of a home directory.
    pub fn default_root() -> PathBuf {
        if let Ok(explicit) = std::env::var("VALYRIA_HOME") {
            return PathBuf::from(explicit);
        }
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".valyria");
        }
        PathBuf::from(".valyria")
    }

    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| {
            AppError::Vfs(valyria_vfs::VfsError::Io {
                path: root.display().to_string(),
                source: e,
            })
        })?;
        let store = Arc::new(Store::open(&root.join("global.db"), &global_migrations())?);
        Ok(Self {
            models: Arc::new(InstalledModelStore::new(store.clone())),
            user_memory: Arc::new(MemoryStore::new(store.clone())),
            store,
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn models(&self) -> &Arc<InstalledModelStore> {
        &self.models
    }

    pub fn user_memory(&self) -> &Arc<MemoryStore> {
        &self.user_memory
    }

    /// Record that `path` is a valyria workspace, or bump its
    /// `last_seen_ms` if it already is. Idempotent.
    pub async fn register_workspace(
        &self,
        workspace_id: WorkspaceId,
        path: &Path,
        now_ms: i64,
    ) -> Result<()> {
        let id = workspace_id.to_string();
        let path = path.display().to_string();
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO workspace_registry (workspace_id, path, first_seen_ms, last_seen_ms)
                     VALUES (?1, ?2, ?3, ?3)
                     ON CONFLICT(workspace_id) DO UPDATE SET path = ?2, last_seen_ms = ?3",
                    params![id, path, now_ms],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn workspaces(&self) -> Result<Vec<WorkspaceRegistration>> {
        let rows = self
            .store
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT workspace_id, path, first_seen_ms, last_seen_ms
                     FROM workspace_registry ORDER BY last_seen_ms DESC",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, path, first, last)| {
                Some(WorkspaceRegistration {
                    workspace_id: id.parse().ok()?,
                    path: PathBuf::from(path),
                    first_seen_ms: first,
                    last_seen_ms: last,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_migrations_have_no_version_collisions() {
        let all = global_migrations();
        let mut versions: Vec<i64> = all.iter().map(|m| m.version).collect();
        let before = versions.len();
        versions.sort_unstable();
        versions.dedup();
        assert_eq!(versions.len(), before, "duplicate version in {all:?}");
    }

    #[tokio::test]
    async fn open_creates_the_db_and_registry_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let global = GlobalStore::open(dir.path()).await.unwrap();
        assert!(dir.path().join("global.db").exists());

        let ws = WorkspaceId::new();
        global
            .register_workspace(ws, Path::new("/tmp/project"), 1_000)
            .await
            .unwrap();
        // Re-register: idempotent, bumps last_seen.
        global
            .register_workspace(ws, Path::new("/tmp/project"), 2_000)
            .await
            .unwrap();

        let all = global.workspaces().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].workspace_id, ws);
        assert_eq!(all[0].first_seen_ms, 1_000);
        assert_eq!(all[0].last_seen_ms, 2_000);
    }

    #[tokio::test]
    async fn model_and_memory_slices_are_present() {
        let dir = tempfile::tempdir().unwrap();
        let global = GlobalStore::open(dir.path()).await.unwrap();
        // Both sub-stores can touch their tables without erroring.
        assert!(global.models().list().await.unwrap().is_empty());
        let stats = global.user_memory().stats().await.unwrap();
        assert_eq!(stats.live, 0);
        assert_eq!(stats.retired, 0);
    }
}
