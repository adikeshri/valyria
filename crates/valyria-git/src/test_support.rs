//! Test-only fixture repos, built by shelling out to the real `git` CLI.
//! This is deliberately the *only* place in this crate that touches a
//! `git` binary — production code paths always go through `gix` — but for
//! building known-shape fixtures in tests, the real CLI is the simplest
//! source of ground truth to assert `gix`'s reads against.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

pub fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be installed to run these tests");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be installed to run these tests");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub fn init_empty_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    configure_identity(dir.path());
    dir
}

fn configure_identity(dir: &Path) {
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test User"]);
    // Keep fixtures reproducible across whatever the host's default
    // branch/signing config happens to be.
    run_git(dir, &["config", "commit.gpgsign", "false"]);
}

/// A repo with one commit adding `README.md`. Returns the repo dir and the
/// new commit's SHA.
pub fn init_repo_with_commit() -> (TempDir, String) {
    let dir = init_empty_repo();
    std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-q", "-m", "initial commit"]);
    let sha = git_output(dir.path(), &["rev-parse", "HEAD"]);
    (dir, sha)
}

pub fn commit_all(dir: &Path, message: &str) -> String {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", message]);
    git_output(dir, &["rev-parse", "HEAD"])
}

pub fn create_branch(dir: &Path, name: &str) {
    run_git(dir, &["branch", name]);
}

pub fn checkout(dir: &Path, name: &str) {
    run_git(dir, &["checkout", "-q", name]);
}
