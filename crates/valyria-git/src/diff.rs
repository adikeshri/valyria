//! Commit diffs / `show` (§24): structured per-file changes between a
//! commit and its first parent (or, for a root commit, every file it
//! introduces). Deliberately path/status-level for now, not hunk-level —
//! see the module docs on scope.

use crate::error::{GitError, Result};
use crate::repo::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: ChangeKind,
    pub rename_from: Option<String>,
}

impl Repo {
    /// The set of file-level changes a commit introduced relative to its
    /// first parent. A root commit (no parents) reports every file it
    /// contains as `Added`.
    pub fn show(&self, sha: &str) -> Result<Vec<FileDiff>> {
        let id =
            gix::ObjectId::from_hex(sha.as_bytes()).map_err(|e| GitError::Op(e.to_string()))?;
        let commit = self
            .inner
            .find_object(id)
            .map_err(|e| GitError::Op(e.to_string()))?
            .try_into_commit()
            .map_err(|e| GitError::Op(e.to_string()))?;

        let tree_new = commit.tree().map_err(|e| GitError::Op(e.to_string()))?;
        let parent_tree = match commit.parent_ids().next() {
            Some(parent_id) => {
                let parent_commit = parent_id
                    .object()
                    .map_err(|e| GitError::Op(e.to_string()))?
                    .try_into_commit()
                    .map_err(|e| GitError::Op(e.to_string()))?;
                Some(
                    parent_commit
                        .tree()
                        .map_err(|e| GitError::Op(e.to_string()))?,
                )
            }
            None => None,
        };

        match parent_tree {
            Some(mut old) => diff_trees(&mut old, &tree_new),
            None => root_commit_additions(&tree_new),
        }
    }
}

fn diff_trees(old: &mut gix::Tree<'_>, new: &gix::Tree<'_>) -> Result<Vec<FileDiff>> {
    use gix::object::tree::diff::change::Event;
    use gix::object::tree::diff::Action;

    let mut out = Vec::new();
    let mut platform = old.changes().map_err(|e| GitError::Op(e.to_string()))?;
    // `location` on each change is empty unless path tracking is turned
    // on explicitly. Rewrite (rename/copy) tracking is off for now to
    // keep this module's scope at plain add/delete/modify — turning it
    // back on is future work alongside actual rename-aware `FileDiff`
    // consumers.
    platform.track_path();
    platform.track_rewrites(None);
    platform
        .for_each_to_obtain_tree(new, |change| {
            let path = change.location.to_string();
            match change.event {
                Event::Addition { .. } => out.push(FileDiff {
                    path,
                    status: ChangeKind::Added,
                    rename_from: None,
                }),
                Event::Deletion { .. } => out.push(FileDiff {
                    path,
                    status: ChangeKind::Deleted,
                    rename_from: None,
                }),
                Event::Modification { .. } => out.push(FileDiff {
                    path,
                    status: ChangeKind::Modified,
                    rename_from: None,
                }),
                Event::Rewrite {
                    source_location, ..
                } => out.push(FileDiff {
                    path,
                    status: ChangeKind::Renamed,
                    rename_from: Some(source_location.to_string()),
                }),
            }
            Ok::<_, std::convert::Infallible>(Action::Continue)
        })
        .map_err(|e| GitError::Op(e.to_string()))?;
    Ok(out)
}

fn root_commit_additions(tree: &gix::Tree<'_>) -> Result<Vec<FileDiff>> {
    let files = tree
        .traverse()
        .breadthfirst
        .files()
        .map_err(|e| GitError::Op(e.to_string()))?;
    Ok(files
        .into_iter()
        .map(|f| FileDiff {
            path: f.filepath.to_string(),
            status: ChangeKind::Added,
            rename_from: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo_with_commit};

    #[test]
    fn root_commit_shows_every_file_as_added() {
        let (dir, sha) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo.show(&sha).unwrap();

        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].path, "README.md");
        assert_eq!(diff[0].status, ChangeKind::Added);
    }

    #[test]
    fn shows_a_modification_relative_to_parent() {
        let (dir, _first_sha) = init_repo_with_commit();
        std::fs::write(dir.path().join("README.md"), "changed\n").unwrap();
        let second_sha = commit_all(dir.path(), "update readme");

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo.show(&second_sha).unwrap();

        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].path, "README.md");
        assert_eq!(diff[0].status, ChangeKind::Modified);
    }

    #[test]
    fn shows_an_addition_and_a_deletion_in_the_same_commit() {
        let (dir, _) = init_repo_with_commit();
        std::fs::write(dir.path().join("new.txt"), "new content").unwrap();
        std::fs::remove_file(dir.path().join("README.md")).unwrap();
        let sha = commit_all(dir.path(), "swap files");

        let repo = Repo::open(dir.path()).unwrap();
        let diff = repo.show(&sha).unwrap();

        assert_eq!(diff.len(), 2);
        assert!(diff
            .iter()
            .any(|d| d.path == "new.txt" && d.status == ChangeKind::Added));
        assert!(diff
            .iter()
            .any(|d| d.path == "README.md" && d.status == ChangeKind::Deleted));
    }
}
