//! Commit history (§24). Feeds search ranking (recently-touched files rank
//! up) and repository memory ("who owns this area") per the build plan's
//! search and memory sections.

use crate::error::{GitError, Result};
use crate::repo::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub message: String,
    /// Author time, unix seconds.
    pub time: i64,
    pub parents: Vec<String>,
}

impl Repo {
    /// Walks first-parent-inclusive history from HEAD, newest first,
    /// yielding at most `max_count` commits.
    pub fn log(&self, max_count: usize) -> Result<Vec<CommitInfo>> {
        let head_id = self.inner.head_id().map_err(|_| GitError::UnbornHead)?;

        let walk = head_id
            .ancestors()
            .all()
            .map_err(|e| GitError::Op(e.to_string()))?;

        let mut out = Vec::with_capacity(max_count);
        for info in walk.take(max_count) {
            let info = info.map_err(|e| GitError::Op(e.to_string()))?;
            let commit = info.object().map_err(|e| GitError::Op(e.to_string()))?;
            let message = commit.message().map_err(|e| GitError::Op(e.to_string()))?;
            let author = commit.author().map_err(|e| GitError::Op(e.to_string()))?;
            let time = commit.time().map_err(|e| GitError::Op(e.to_string()))?;

            out.push(CommitInfo {
                sha: info.id.to_string(),
                author_name: author.name.to_string(),
                author_email: author.email.to_string(),
                message: message.title.to_string().trim().to_string(),
                time: time.seconds,
                parents: info.parent_ids().map(|id| id.to_string()).collect(),
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo_with_commit};

    #[test]
    fn logs_a_single_commit() {
        let (dir, sha) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        let log = repo.log(10).unwrap();

        assert_eq!(log.len(), 1);
        assert_eq!(log[0].sha, sha);
        assert_eq!(log[0].message, "initial commit");
        assert_eq!(log[0].author_email, "test@example.com");
        assert!(log[0].parents.is_empty());
    }

    #[test]
    fn logs_multiple_commits_newest_first() {
        let (dir, first_sha) = init_repo_with_commit();
        std::fs::write(dir.path().join("second.txt"), "content").unwrap();
        let second_sha = commit_all(dir.path(), "second commit");

        let repo = Repo::open(dir.path()).unwrap();
        let log = repo.log(10).unwrap();

        assert_eq!(log.len(), 2);
        assert_eq!(log[0].sha, second_sha, "newest commit first");
        assert_eq!(log[1].sha, first_sha);
        assert_eq!(log[0].parents, vec![first_sha]);
    }

    #[test]
    fn respects_max_count() {
        let (dir, _) = init_repo_with_commit();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
            commit_all(dir.path(), &format!("commit {i}"));
        }

        let repo = Repo::open(dir.path()).unwrap();
        let log = repo.log(3).unwrap();
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn unborn_head_is_a_typed_error() {
        let dir = crate::test_support::init_empty_repo();
        let repo = Repo::open(dir.path()).unwrap();
        assert!(matches!(repo.log(10), Err(GitError::UnbornHead)));
    }
}
