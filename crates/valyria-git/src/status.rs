//! Working tree status (§24): staged/unstaged changes, untracked files,
//! conflicts.
//!
//! `gix`'s own `status()` platform only computes worktree-vs-index
//! (unstaged) changes and untracked files — even `gix::Repository::is_dirty`
//! carries an upstream "Incomplete Implementation Warning" that it does not
//! (yet) compute head-vs-index (staged) changes. So staged status here is
//! computed separately: every index entry is looked up by path in the HEAD
//! tree (added/modified), and every HEAD tree entry is checked for
//! presence in the index (deleted) — verified against real repos with
//! `git add`-only and `git add`-then-edit fixtures in this module's tests.

use std::collections::HashSet;

use crate::error::{GitError, Result};
use crate::repo::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Added,
    Modified,
    Deleted,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub kind: StatusKind,
    pub staged: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub files: Vec<FileStatus>,
}

impl RepoStatus {
    pub fn is_clean(&self) -> bool {
        self.files.is_empty()
    }
}

impl Repo {
    pub fn status(&self) -> Result<RepoStatus> {
        // `gix` 0.66's index-worktree status iterator unconditionally
        // touches HEAD-relative submodule resolution during its directory
        // walk (to check whether an untracked directory is actually an
        // unlisted submodule mount), which errors even with submodule
        // handling explicitly disabled (`index_worktree_submodules(None)`)
        // when HEAD is unborn — a freshly `git init`'d repo with no
        // commits yet. Staged-status computation doesn't share that path
        // and works correctly in that case (see `staged_status`), so an
        // unborn-HEAD repo reports staged changes only; unstaged/untracked
        // detection for that specific narrow case is future work pending
        // an upstream fix or a lower-level workaround.
        let mut files = if self.inner.head_id().is_ok() {
            self.unstaged_status()?
        } else {
            Vec::new()
        };
        files.extend(self.staged_status()?);
        Ok(RepoStatus { files })
    }

    /// Worktree-vs-index: unstaged modifications and untracked files.
    fn unstaged_status(&self) -> Result<Vec<FileStatus>> {
        use gix::status::index_worktree::iter::Item;
        use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

        let platform = self
            .inner
            .status(gix::progress::Discard)
            .map_err(|e| GitError::Op(e.to_string()))?
            // Submodules are out of scope for now, and — critically — even
            // an `Ignore::All` policy still enumerates submodules first
            // (via `Repository::modules()`, which reads `.gitmodules` from
            // the HEAD tree) before deciding to ignore them, which errors
            // out on an unborn HEAD. Passing `None` here skips submodule
            // handling entirely rather than configuring it permissively.
            .index_worktree_submodules(None);

        let iter = platform
            .into_index_worktree_iter(Vec::new())
            .map_err(|e| GitError::Op(e.to_string()))?;

        let mut out = Vec::new();
        for item in iter {
            let item = item.map_err(|e| GitError::Op(e.to_string()))?;
            match item {
                Item::Modification {
                    rela_path, status, ..
                } => {
                    let kind = match status {
                        EntryStatus::Conflict(_) => StatusKind::Conflicted,
                        EntryStatus::Change(Change::Removed) => StatusKind::Deleted,
                        EntryStatus::Change(_) => StatusKind::Modified,
                        // "needs stat update" / intent-to-add: not a
                        // user-visible change, nothing to report.
                        EntryStatus::NeedsUpdate(_) | EntryStatus::IntentToAdd => continue,
                    };
                    out.push(FileStatus {
                        path: rela_path.to_string(),
                        kind,
                        staged: false,
                    });
                }
                Item::DirectoryContents { entry, .. } => {
                    if entry.status == gix::dir::entry::Status::Untracked {
                        out.push(FileStatus {
                            path: entry.rela_path.to_string(),
                            kind: StatusKind::Untracked,
                            staged: false,
                        });
                    }
                }
                Item::Rewrite { dirwalk_entry, .. } => {
                    out.push(FileStatus {
                        path: dirwalk_entry.rela_path.to_string(),
                        kind: StatusKind::Modified,
                        staged: false,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Index-vs-HEAD: what's actually staged for the next commit.
    fn staged_status(&self) -> Result<Vec<FileStatus>> {
        let index = self
            .inner
            .index()
            .map_err(|e| GitError::Op(e.to_string()))?;
        let head_tree = match self.inner.head_commit() {
            Ok(commit) => Some(commit.tree().map_err(|e| GitError::Op(e.to_string()))?),
            Err(_) => None, // unborn HEAD: everything staged is new
        };

        let mut out = Vec::new();
        let mut buf = Vec::new();
        let mut index_paths: HashSet<String> = HashSet::new();

        for entry in index.entries() {
            let path = entry.path(&index).to_string();
            index_paths.insert(path.clone());

            let tree_entry = match &head_tree {
                Some(tree) => tree
                    .lookup_entry_by_path(&path, &mut buf)
                    .map_err(|e| GitError::Op(e.to_string()))?,
                None => None,
            };

            match tree_entry {
                Some(te) if te.oid() == entry.id.as_ref() => {} // unchanged
                Some(_) => out.push(FileStatus {
                    path,
                    kind: StatusKind::Modified,
                    staged: true,
                }),
                None => out.push(FileStatus {
                    path,
                    kind: StatusKind::Added,
                    staged: true,
                }),
            }
        }

        if let Some(tree) = &head_tree {
            let all_head_files = tree
                .traverse()
                .breadthfirst
                .files()
                .map_err(|e| GitError::Op(e.to_string()))?;
            for file in all_head_files {
                let path = file.filepath.to_string();
                if !index_paths.contains(&path) {
                    out.push(FileStatus {
                        path,
                        kind: StatusKind::Deleted,
                        staged: true,
                    });
                }
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{init_empty_repo, init_repo_with_commit, run_git as git};

    fn find<'a>(status: &'a RepoStatus, path: &str) -> &'a FileStatus {
        status
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no status entry for {path}, got {:?}", status.files))
    }

    #[test]
    fn clean_repo_has_no_status_entries() {
        let (dir, _) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        assert!(repo.status().unwrap().is_clean());
    }

    #[test]
    fn detects_an_untracked_file() {
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("new.txt"), "content").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "new.txt");
        assert_eq!(entry.kind, StatusKind::Untracked);
        assert!(!entry.staged);
    }

    #[test]
    fn detects_a_staged_new_file() {
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("new.txt"), "content").unwrap();
        git(dir.path(), &["add", "new.txt"]);

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "new.txt");
        assert_eq!(entry.kind, StatusKind::Added);
        assert!(entry.staged);
    }

    #[test]
    fn detects_an_unstaged_modification_to_a_tracked_file() {
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("README.md"), "changed content\n").unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "README.md");
        assert_eq!(entry.kind, StatusKind::Modified);
        assert!(!entry.staged);
    }

    #[test]
    fn detects_a_staged_modification() {
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("README.md"), "changed content\n").unwrap();
        git(dir.path(), &["add", "README.md"]);

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "README.md");
        assert_eq!(entry.kind, StatusKind::Modified);
        assert!(entry.staged);
    }

    #[test]
    fn detects_a_staged_deletion() {
        let (dir, _) = init_repo_with_commit();
        git(dir.path(), &["rm", "README.md"]);

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "README.md");
        assert_eq!(entry.kind, StatusKind::Deleted);
        assert!(entry.staged);
    }

    #[test]
    fn a_file_can_have_both_a_staged_and_unstaged_entry() {
        // committed at v1; staged edit to v2; further unstaged edit to v3
        // on top of that — so the same path shows up once as a staged
        // modification and once as an unstaged one.
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("README.md"), "version two content").unwrap();
        git(dir.path(), &["add", "README.md"]);
        // Different length (not just different bytes) and a short delay:
        // gix's worktree-vs-index comparison short-circuits on a stat
        // match (mtime + size), so an in-place same-size, same-instant
        // rewrite can be indistinguishable from "unchanged" on filesystems
        // with coarse mtime resolution.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            dir.path().join("README.md"),
            "version three content, made longer",
        )
        .unwrap();

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entries: Vec<&FileStatus> = status
            .files
            .iter()
            .filter(|f| f.path == "README.md")
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "expected both a staged and unstaged entry, got {entries:?}"
        );
        assert!(entries
            .iter()
            .any(|e| e.staged && e.kind == StatusKind::Modified));
        assert!(entries
            .iter()
            .any(|e| !e.staged && e.kind == StatusKind::Modified));
    }

    #[test]
    fn unborn_head_reports_staged_additions() {
        // Documents the current, narrow limitation: unstaged/untracked
        // detection is skipped for an unborn HEAD (see `Repo::status`'s
        // doc comment for why), but staged changes are still reported
        // correctly since `staged_status` doesn't share that code path.
        let dir = init_empty_repo();
        std::fs::write(dir.path().join("f.txt"), "content").unwrap();
        git(dir.path(), &["add", "f.txt"]);

        let repo = Repo::open(dir.path()).unwrap();
        let status = repo.status().unwrap();
        let entry = find(&status, "f.txt");
        assert_eq!(entry.kind, StatusKind::Added);
        assert!(entry.staged);
    }
}
