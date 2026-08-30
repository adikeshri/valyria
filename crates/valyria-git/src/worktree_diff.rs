//! Textual working-tree diffs (§24, desktop-client gap G3).
//!
//! [`Repo::show`](crate::diff) is *commit*-level and file-status-only. The
//! desktop client's diff viewer needs the ordinary "what has changed but
//! is not committed yet" view as unified-diff text — `git diff` and
//! `git diff --staged` — without shelling out to a `git` binary. This
//! module builds that from `gix` blob reads plus `imara-diff`'s unified
//! formatter.
//!
//! Scope: text files. A file whose old or new content contains a NUL byte
//! is reported as `Binary files … differ`, matching `git diff`.

use imara_diff::intern::InternedInput;
use imara_diff::{diff, Algorithm, UnifiedDiffBuilder};

use crate::error::{GitError, Result};
use crate::repo::Repo;
use crate::status::StatusKind;

/// A rendered working-tree diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeDiff {
    /// Concatenated unified-diff text (one `diff --git` section per file).
    /// Empty when there is nothing to show.
    pub unified: String,
    /// `true` when [`WorktreeDiff::unified`] was clipped at `max_bytes`.
    pub truncated: bool,
}

impl Repo {
    /// Unified-diff text for the working tree.
    ///
    /// * `staged == false` → worktree vs. index (`git diff`).
    /// * `staged == true`  → index vs. HEAD (`git diff --staged`).
    ///
    /// `pathspec`, when given, restricts the output to that exact
    /// repo-relative path. `max_bytes` caps the result; on overflow the
    /// text is truncated at a line boundary and `truncated` is set.
    pub fn worktree_diff(
        &self,
        pathspec: Option<&str>,
        staged: bool,
        max_bytes: usize,
    ) -> Result<WorktreeDiff> {
        let mut entries = self.status()?.files;
        entries.retain(|f| f.staged == staged);
        if let Some(want) = pathspec {
            entries.retain(|f| f.path == want);
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        let mut out = String::new();
        let mut truncated = false;

        for entry in &entries {
            if out.len() >= max_bytes {
                truncated = true;
                break;
            }

            let (old, new) = if staged {
                (self.head_blob(&entry.path)?, self.index_blob(&entry.path)?)
            } else {
                let old = match entry.kind {
                    StatusKind::Untracked => Vec::new(),
                    _ => self.index_blob(&entry.path)?,
                };
                let new = match entry.kind {
                    StatusKind::Deleted => Vec::new(),
                    _ => std::fs::read(self.workdir_join(&entry.path)?).unwrap_or_default(),
                };
                (old, new)
            };

            let section = render_file_diff(&entry.path, &old, &new);
            if section.is_empty() {
                continue;
            }
            if out.len() + section.len() > max_bytes {
                truncated = true;
                break;
            }
            out.push_str(&section);
        }

        Ok(WorktreeDiff {
            unified: out,
            truncated,
        })
    }

    /// Blob bytes for `path` in the current index, or empty if absent.
    fn index_blob(&self, path: &str) -> Result<Vec<u8>> {
        let index = self
            .inner
            .index()
            .map_err(|e| GitError::Op(e.to_string()))?;
        let Some(entry) = index.entry_by_path(path.into()) else {
            return Ok(Vec::new());
        };
        let obj = self
            .inner
            .find_object(entry.id)
            .map_err(|e| GitError::Op(e.to_string()))?;
        Ok(obj.data.clone())
    }

    /// Blob bytes for `path` in the HEAD tree, or empty if absent / unborn.
    fn head_blob(&self, path: &str) -> Result<Vec<u8>> {
        let Ok(commit) = self.inner.head_commit() else {
            return Ok(Vec::new());
        };
        let tree = commit.tree().map_err(|e| GitError::Op(e.to_string()))?;
        let mut buf = Vec::new();
        let Some(entry) = tree
            .lookup_entry_by_path(path, &mut buf)
            .map_err(|e| GitError::Op(e.to_string()))?
        else {
            return Ok(Vec::new());
        };
        let obj = self
            .inner
            .find_object(entry.oid())
            .map_err(|e| GitError::Op(e.to_string()))?;
        Ok(obj.data.clone())
    }

    fn workdir_join(&self, path: &str) -> Result<std::path::PathBuf> {
        let root = self
            .inner
            .work_dir()
            .ok_or_else(|| GitError::Op("bare repository has no working tree".into()))?;
        Ok(root.join(path))
    }
}

fn render_file_diff(path: &str, old: &[u8], new: &[u8]) -> String {
    if old == new {
        return String::new();
    }
    let header = format!("diff --git a/{path} b/{path}\n");

    if old.contains(&0) || new.contains(&0) {
        return format!("{header}Binary files a/{path} and b/{path} differ\n");
    }

    let old_s = String::from_utf8_lossy(old);
    let new_s = String::from_utf8_lossy(new);
    let input = InternedInput::new(old_s.as_ref(), new_s.as_ref());
    let body = diff(
        Algorithm::Histogram,
        &input,
        UnifiedDiffBuilder::new(&input),
    );
    if body.is_empty() {
        return String::new();
    }

    let old_label = if old.is_empty() {
        "--- /dev/null\n".to_string()
    } else {
        format!("--- a/{path}\n")
    };
    let new_label = if new.is_empty() {
        "+++ /dev/null\n".to_string()
    } else {
        format!("+++ b/{path}\n")
    };
    format!("{header}{old_label}{new_label}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_empty_repo, run_git as git};
    use std::path::Path;

    const CAP: usize = 1 << 20;

    /// A repo with `file.txt` committed at `contents`.
    fn repo_with_committed_file(contents: &str) -> tempfile::TempDir {
        let dir = init_empty_repo();
        std::fs::write(dir.path().join("file.txt"), contents).unwrap();
        commit_all(dir.path(), "add file.txt");
        dir
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn unstaged_edit_produces_a_unified_hunk() {
        let dir = repo_with_committed_file("alpha\nbeta\ngamma\n");
        // A length-changing edit so `gix` cannot skip it as racy-clean
        // (a same-second, same-size rewrite is the one case its worktree
        // status may not flag — real edits and real time gaps avoid it).
        write(
            dir.path(),
            "file.txt",
            "alpha\nBETA is different now\ngamma\n",
        );

        let repo = Repo::open(dir.path()).unwrap();
        let d = repo.worktree_diff(None, false, CAP).unwrap();

        assert!(d.unified.contains("diff --git a/file.txt b/file.txt"));
        assert!(d.unified.contains("--- a/file.txt"));
        assert!(d.unified.contains("-beta"));
        assert!(d.unified.contains("+BETA"));
        assert!(!d.truncated);
    }

    #[test]
    fn staged_diff_shows_only_what_is_in_the_index() {
        let dir = repo_with_committed_file("alpha\nbeta\ngamma\n");
        write(dir.path(), "file.txt", "alpha\nSTAGED\ngamma\n");
        git(dir.path(), &["add", "file.txt"]);
        // A further unstaged edit on top of the staged one:
        write(dir.path(), "file.txt", "alpha\nSTAGED\nWORKTREE\n");

        let repo = Repo::open(dir.path()).unwrap();

        let staged = repo.worktree_diff(None, true, CAP).unwrap();
        assert!(staged.unified.contains("+STAGED"));
        assert!(
            !staged.unified.contains("WORKTREE"),
            "staged diff must not include the later unstaged edit:\n{}",
            staged.unified
        );

        let unstaged = repo.worktree_diff(None, false, CAP).unwrap();
        assert!(unstaged.unified.contains("+WORKTREE"));
    }

    #[test]
    fn untracked_file_diffs_against_dev_null() {
        let dir = repo_with_committed_file("x\n");
        write(dir.path(), "new.txt", "one\ntwo\n");

        let repo = Repo::open(dir.path()).unwrap();
        let d = repo.worktree_diff(None, false, CAP).unwrap();
        assert!(d.unified.contains("diff --git a/new.txt b/new.txt"));
        assert!(d.unified.contains("--- /dev/null"));
        assert!(d.unified.contains("+one"));
        assert!(d.unified.contains("+two"));
    }

    #[test]
    fn pathspec_restricts_the_output() {
        let dir = init_empty_repo();
        write(dir.path(), "file.txt", "alpha\nbeta\ngamma\n");
        write(dir.path(), "other.txt", "brand new\n");
        commit_all(dir.path(), "two files");
        write(
            dir.path(),
            "file.txt",
            "alpha\nEDITED (longer line)\ngamma\n",
        );
        write(dir.path(), "other.txt", "brand new\nmore\n");

        let repo = Repo::open(dir.path()).unwrap();
        let only = repo.worktree_diff(Some("file.txt"), false, CAP).unwrap();
        assert!(only.unified.contains("a/file.txt"));
        assert!(!only.unified.contains("other.txt"));
    }

    #[test]
    fn clean_tree_yields_an_empty_diff() {
        let dir = repo_with_committed_file("stable\n");
        let repo = Repo::open(dir.path()).unwrap();
        let d = repo.worktree_diff(None, false, CAP).unwrap();
        assert!(d.unified.is_empty());
        assert!(!d.truncated);
    }

    #[test]
    fn a_tiny_cap_truncates_and_flags_it() {
        let dir = repo_with_committed_file("alpha\nbeta\ngamma\ndelta\nepsilon\n");
        write(
            dir.path(),
            "file.txt",
            "ALPHA one\nBETA two\nGAMMA three\nDELTA four\nEPSILON five\n",
        );
        let repo = Repo::open(dir.path()).unwrap();
        let d = repo.worktree_diff(None, false, 10).unwrap();
        assert!(d.truncated);
    }

    #[test]
    fn binary_content_is_reported_not_dumped() {
        let dir = repo_with_committed_file("x\n");
        std::fs::write(dir.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
        git(dir.path(), &["add", "blob.bin"]);

        let repo = Repo::open(dir.path()).unwrap();
        let d = repo.worktree_diff(None, true, CAP).unwrap();
        assert!(d
            .unified
            .contains("Binary files a/blob.bin and b/blob.bin differ"));
    }
}
