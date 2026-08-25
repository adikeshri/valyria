//! Opening a repository and reading HEAD (§24).

use std::path::Path;

use crate::error::{GitError, Result};

pub struct Repo {
    pub(crate) inner: gix::Repository,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    /// The branch name if HEAD points at one (`refs/heads/<name>`), or
    /// `None` if detached.
    pub branch: Option<String>,
    /// The commit HEAD resolves to, as a hex SHA. `None` for an unborn
    /// HEAD (a freshly `git init`'d repo with no commits yet).
    pub commit: Option<String>,
    pub detached: bool,
}

impl Repo {
    pub fn open(path: &Path) -> Result<Self> {
        let inner = gix::open(path).map_err(|e| GitError::Open {
            path: path.display().to_string(),
            source: Box::new(e),
        })?;
        Ok(Self { inner })
    }

    pub fn head_info(&self) -> Result<HeadInfo> {
        let head = self.inner.head().map_err(|e| GitError::Op(e.to_string()))?;

        let detached = head.is_detached();
        let branch = head
            .clone()
            .try_into_referent()
            .and_then(|r| r.name().shorten().to_string().into());

        let commit = head.id().map(|id| id.to_string());

        Ok(HeadInfo {
            branch,
            commit,
            detached,
        })
    }

    pub fn workdir(&self) -> Option<&Path> {
        self.inner.work_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo_with_commit;

    #[test]
    fn opens_a_real_repository() {
        let (dir, _sha) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        assert!(repo.workdir().is_some());
    }

    #[test]
    fn reads_head_branch_and_commit() {
        let (dir, sha) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        let head = repo.head_info().unwrap();

        assert!(!head.detached);
        assert_eq!(head.commit.as_deref(), Some(sha.as_str()));
        assert!(head.branch.is_some());
    }

    #[test]
    fn fails_to_open_a_non_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Repo::open(dir.path()).is_err());
    }
}
