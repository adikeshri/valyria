//! Branch listing (§24).

use crate::error::{GitError, Result};
use crate::repo::Repo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub commit: String,
    pub is_head: bool,
}

impl Repo {
    pub fn branches(&self) -> Result<Vec<BranchInfo>> {
        let head_branch = self
            .head_info()
            .ok()
            .and_then(|h| if h.detached { None } else { h.branch });

        let platform = self
            .inner
            .references()
            .map_err(|e| GitError::Op(e.to_string()))?;
        let local_branches = platform
            .local_branches()
            .map_err(|e| GitError::Op(e.to_string()))?;

        let mut out = Vec::new();
        for branch in local_branches {
            let mut branch = branch.map_err(|e| GitError::Op(e.to_string()))?;
            let name = branch.name().shorten().to_string();
            let commit = branch
                .peel_to_id_in_place()
                .map_err(|e| GitError::Op(e.to_string()))?
                .to_string();
            let is_head = head_branch.as_deref() == Some(name.as_str());
            out.push(BranchInfo {
                name,
                commit,
                is_head,
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{checkout, create_branch, init_repo_with_commit};

    #[test]
    fn lists_the_default_branch() {
        let (dir, sha) = init_repo_with_commit();
        let repo = Repo::open(dir.path()).unwrap();
        let branches = repo.branches().unwrap();

        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0].name, "main");
        assert_eq!(branches[0].commit, sha);
        assert!(branches[0].is_head);
    }

    #[test]
    fn lists_multiple_branches_and_marks_head_correctly() {
        let (dir, _sha) = init_repo_with_commit();
        create_branch(dir.path(), "feature");

        let repo = Repo::open(dir.path()).unwrap();
        let branches = repo.branches().unwrap();
        assert_eq!(branches.len(), 2);

        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"feature"));

        let main = branches.iter().find(|b| b.name == "main").unwrap();
        let feature = branches.iter().find(|b| b.name == "feature").unwrap();
        assert!(main.is_head);
        assert!(!feature.is_head);
    }

    #[test]
    fn is_head_follows_checkout() {
        let (dir, _sha) = init_repo_with_commit();
        create_branch(dir.path(), "feature");
        checkout(dir.path(), "feature");

        let repo = Repo::open(dir.path()).unwrap();
        let branches = repo.branches().unwrap();
        let feature = branches.iter().find(|b| b.name == "feature").unwrap();
        let main = branches.iter().find(|b| b.name == "main").unwrap();
        assert!(feature.is_head);
        assert!(!main.is_head);
    }
}
