//! `valyria-git` — layer 1 (Platform).
//!
//! Git as a first-class subsystem (§24): status, diff, log, blame, show,
//! branches, renames, merge state — read via `gix` (fast, no shell, no
//! libgit2 build dependency), never by shelling out to a `git` binary in
//! production code paths. Write operations are behind the permission
//! engine (layer 3, not this crate) and are out of scope here.

#![forbid(unsafe_code)]

pub mod branches;
pub mod diff;
pub mod error;
pub mod log;
pub mod repo;
pub mod status;

#[cfg(test)]
mod test_support;

pub use branches::BranchInfo;
pub use diff::{ChangeKind, FileDiff};
pub use error::{GitError, Result};
pub use log::CommitInfo;
pub use repo::{HeadInfo, Repo};
pub use status::{FileStatus, RepoStatus, StatusKind};
